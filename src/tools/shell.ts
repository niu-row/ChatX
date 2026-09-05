import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import fs from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import type { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { config } from '../config.js';
import { assertExistingPath, assertPathAllowed } from '../security/path-policy.js';
import { errorResult, textResult, truncateText } from '../utils/results.js';

type ShellKind = 'auto' | 'powershell' | 'cmd' | 'bash' | 'sh';

type ManagedProcess = {
  id: string;
  command: string;
  cwd: string;
  shell: ShellKind;
  child: ChildProcessWithoutNullStreams;
  startedAt: Date;
  exitedAt: Date | null;
  exitCode: number | null;
  signal: NodeJS.Signals | null;
  output: string;
  droppedChars: number;
};

const processes = new Map<string, ManagedProcess>();

function shellInvocation(kind: ShellKind, command: string): { executable: string; args: string[] } {
  if (kind === 'powershell') {
    return {
      executable: process.platform === 'win32' ? 'powershell.exe' : 'pwsh',
      args: ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', command],
    };
  }

  if (kind === 'cmd') {
    if (process.platform !== 'win32') throw new Error('cmd shell is only available on Windows.');
    return { executable: process.env.ComSpec || 'cmd.exe', args: ['/d', '/s', '/c', command] };
  }

  if (kind === 'bash') return { executable: 'bash', args: ['-lc', command] };
  if (kind === 'sh') return { executable: 'sh', args: ['-lc', command] };

  // PowerShell is the most predictable default for complex quoted commands on Windows.
  // `cmd` remains available explicitly for callers that need cmd.exe syntax.
  if (process.platform === 'win32') {
    return {
      executable: 'powershell.exe',
      args: ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', command],
    };
  }
  return { executable: '/bin/sh', args: ['-lc', command] };
}

function appendOutput(record: ManagedProcess, stream: 'stdout' | 'stderr', chunk: Buffer | string): void {
  const text = typeof chunk === 'string' ? chunk : chunk.toString('utf8');
  record.output += `[${stream}] ${text}`;
  if (record.output.length > config.maxProcessBufferChars) {
    const overflow = record.output.length - config.maxProcessBufferChars;
    record.output = record.output.slice(overflow);
    record.droppedChars += overflow;
  }
}

async function resolveCwd(input?: string): Promise<string> {
  const cwd = input ? await assertExistingPath(input) : await assertPathAllowed(process.cwd());
  const stat = await fs.stat(cwd);
  if (!stat.isDirectory()) throw new Error(`Working directory is not a directory: ${cwd}`);
  return cwd;
}

function createChild(
  command: string,
  cwd: string,
  shell: ShellKind,
  extraEnv: Record<string, string> | undefined,
): ChildProcessWithoutNullStreams {
  const invocation = shellInvocation(shell, command);
  return spawn(invocation.executable, invocation.args, {
    cwd,
    env: { ...process.env, ...(extraEnv ?? {}) },
    windowsHide: true,
    detached: process.platform !== 'win32',
    stdio: ['pipe', 'pipe', 'pipe'],
  });
}

async function killProcessTree(record: ManagedProcess): Promise<void> {
  if (record.child.exitCode !== null || record.child.signalCode !== null) return;
  const pid = record.child.pid;
  if (!pid) return;

  if (process.platform === 'win32') {
    await new Promise<void>((resolve) => {
      const killer = spawn('taskkill', ['/PID', String(pid), '/T', '/F'], {
        windowsHide: true,
        stdio: 'ignore',
      });
      killer.once('close', () => resolve());
      killer.once('error', () => {
        record.child.kill('SIGTERM');
        resolve();
      });
    });
  } else {
    try {
      process.kill(-pid, 'SIGTERM');
    } catch {
      record.child.kill('SIGTERM');
    }
  }
}

function pruneProcesses(): void {
  const now = Date.now();
  for (const [id, record] of processes) {
    if (record.exitedAt && now - record.exitedAt.getTime() > 60 * 60 * 1000) processes.delete(id);
  }

  if (processes.size <= 100) return;
  const finished = [...processes.values()]
    .filter((record) => record.exitedAt)
    .sort((a, b) => (a.exitedAt?.getTime() ?? 0) - (b.exitedAt?.getTime() ?? 0));
  while (processes.size > 100 && finished.length > 0) {
    const record = finished.shift();
    if (record) processes.delete(record.id);
  }
}

async function runForeground(
  command: string,
  cwd: string,
  shell: ShellKind,
  env: Record<string, string> | undefined,
  timeoutMs: number,
): Promise<Record<string, unknown>> {
  const child = createChild(command, cwd, shell, env);
  let stdout = '';
  let stderr = '';
  let stdoutDropped = 0;
  let stderrDropped = 0;
  let timedOut = false;

  const cap = config.maxCommandOutputChars;
  const append = (current: string, chunk: Buffer | string): { value: string; dropped: number } => {
    let value = current + (typeof chunk === 'string' ? chunk : chunk.toString('utf8'));
    let dropped = 0;
    if (value.length > cap) {
      dropped = value.length - cap;
      value = value.slice(dropped);
    }
    return { value, dropped };
  };

  child.stdout.on('data', (chunk: Buffer) => {
    const result = append(stdout, chunk);
    stdout = result.value;
    stdoutDropped += result.dropped;
  });
  child.stderr.on('data', (chunk: Buffer) => {
    const result = append(stderr, chunk);
    stderr = result.value;
    stderrDropped += result.dropped;
  });

  const timer = setTimeout(() => {
    timedOut = true;
    const record: ManagedProcess = {
      id: 'foreground-timeout',
      command,
      cwd,
      shell,
      child,
      startedAt: new Date(),
      exitedAt: null,
      exitCode: null,
      signal: null,
      output: '',
      droppedChars: 0,
    };
    void killProcessTree(record);
  }, timeoutMs);
  timer.unref();

  const result = await new Promise<{ exitCode: number | null; signal: NodeJS.Signals | null }>((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (exitCode, signal) => resolve({ exitCode, signal }));
  }).finally(() => clearTimeout(timer));

  return {
    command,
    cwd,
    shell,
    exit_code: result.exitCode,
    signal: result.signal,
    timed_out: timedOut,
    stdout,
    stderr,
    stdout_dropped_chars: stdoutDropped,
    stderr_dropped_chars: stderrDropped,
  };
}

function runBackground(
  command: string,
  cwd: string,
  shell: ShellKind,
  env: Record<string, string> | undefined,
): ManagedProcess {
  pruneProcesses();
  const child = createChild(command, cwd, shell, env);
  const record: ManagedProcess = {
    id: randomUUID(),
    command,
    cwd,
    shell,
    child,
    startedAt: new Date(),
    exitedAt: null,
    exitCode: null,
    signal: null,
    output: '',
    droppedChars: 0,
  };

  child.stdout.on('data', (chunk: Buffer) => appendOutput(record, 'stdout', chunk));
  child.stderr.on('data', (chunk: Buffer) => appendOutput(record, 'stderr', chunk));
  child.once('error', (error) => appendOutput(record, 'stderr', `process error: ${error.message}\n`));
  child.once('close', (exitCode, signal) => {
    record.exitCode = exitCode;
    record.signal = signal;
    record.exitedAt = new Date();
  });

  processes.set(record.id, record);
  return record;
}

function requireShellEnabled(): void {
  if (!config.enableShell) {
    throw new Error('Shell tools are disabled. Set CHATGPTX_ENABLE_SHELL=true to enable them.');
  }
}

export function registerShellTools(server: McpServer): void {
  server.registerTool(
    'run_command',
    {
      title: 'Run local command',
      description:
        'Execute an arbitrary local shell command as the OS user running ChatGPTX. This is intentionally powerful: filesystem root restrictions do NOT sandbox commands. Auto uses PowerShell on Windows and /bin/sh on Unix. Use background=true for long-running processes.',
      inputSchema: z.object({
        command: z.string().min(1),
        cwd: z.string().optional(),
        shell: z.enum(['auto', 'powershell', 'cmd', 'bash', 'sh']).default('auto'),
        background: z.boolean().default(false),
        timeout_ms: z.number().int().min(100).max(60 * 60 * 1000).optional(),
        env: z.record(z.string(), z.string()).optional(),
      }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: true },
    },
    async ({ command, cwd: inputCwd, shell, background, timeout_ms, env }) => {
      try {
        requireShellEnabled();
        const cwd = await resolveCwd(inputCwd);
        if (background) {
          const record = runBackground(command, cwd, shell, env);
          return textResult({
            process_id: record.id,
            pid: record.child.pid ?? null,
            command,
            cwd,
            shell,
            started_at: record.startedAt.toISOString(),
            status: 'running',
          });
        }

        const result = await runForeground(command, cwd, shell, env, timeout_ms ?? config.defaultCommandTimeoutMs);
        return textResult(result);
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'process_output',
    {
      title: 'Read managed process output',
      description: 'Read buffered stdout/stderr and status from a background process started by run_command.',
      inputSchema: z.object({
        process_id: z.string().min(1),
        max_chars: z.number().int().min(100).max(1_000_000).default(200_000),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ process_id, max_chars }) => {
      try {
        requireShellEnabled();
        const record = processes.get(process_id);
        if (!record) throw new Error(`Unknown process_id: ${process_id}`);
        const output = truncateText(record.output, max_chars);
        return textResult({
          process_id,
          pid: record.child.pid ?? null,
          command: record.command,
          cwd: record.cwd,
          running: record.exitedAt === null,
          exit_code: record.exitCode,
          signal: record.signal,
          started_at: record.startedAt.toISOString(),
          exited_at: record.exitedAt?.toISOString() ?? null,
          dropped_prefix_chars: record.droppedChars,
          output_truncated_for_response: output.truncated,
          output: output.text,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'process_list',
    {
      title: 'List managed processes',
      description: 'List background processes started through this ChatGPTX server instance.',
      inputSchema: z.object({}),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async () => {
      try {
        requireShellEnabled();
        pruneProcesses();
        return textResult({
          processes: [...processes.values()].map((record) => ({
            process_id: record.id,
            pid: record.child.pid ?? null,
            command: record.command,
            cwd: record.cwd,
            running: record.exitedAt === null,
            exit_code: record.exitCode,
            signal: record.signal,
            started_at: record.startedAt.toISOString(),
            exited_at: record.exitedAt?.toISOString() ?? null,
          })),
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'process_stdin',
    {
      title: 'Write managed process stdin',
      description: 'Write data to stdin of a running background process. This is a pipe, not a pseudo-terminal.',
      inputSchema: z.object({
        process_id: z.string().min(1),
        data: z.string().default(''),
        end: z.boolean().default(false),
      }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
    },
    async ({ process_id, data, end }) => {
      try {
        requireShellEnabled();
        const record = processes.get(process_id);
        if (!record) throw new Error(`Unknown process_id: ${process_id}`);
        if (record.exitedAt) throw new Error(`Process has already exited: ${process_id}`);
        if (data) record.child.stdin.write(data);
        if (end) record.child.stdin.end();
        return textResult({ process_id, bytes_written: Buffer.byteLength(data), stdin_ended: end });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'process_terminate',
    {
      title: 'Terminate managed process',
      description: 'Terminate a background process started by run_command, including its process tree where supported.',
      inputSchema: z.object({ process_id: z.string().min(1) }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: true, openWorldHint: false },
    },
    async ({ process_id }) => {
      try {
        requireShellEnabled();
        const record = processes.get(process_id);
        if (!record) throw new Error(`Unknown process_id: ${process_id}`);
        await killProcessTree(record);
        return textResult({ process_id, termination_requested: true });
      } catch (error) {
        return errorResult(error);
      }
    },
  );
}
