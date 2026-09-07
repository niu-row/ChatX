import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import fs from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import type { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { config } from '../config.js';
import { requirePermission } from '../settings.js';
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

export async function terminateAllManagedProcesses(): Promise<number> {
  const running = [...processes.values()].filter((record) => record.exitedAt === null);
  await Promise.all(running.map((record) => killProcessTree(record)));
  return running.length;
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

type CompletedExecution = {
  id: string;
  cwd: string;
  completedAt: number;
  stdout: string;
  stderr: string;
  stdoutTotal: number;
  stderrTotal: number;
  metadata: Record<string, unknown>;
};
const executions = new Map<string, CompletedExecution>();
const EXECUTION_TTL_MS = 10 * 60 * 1000;
const EXECUTION_CACHE_CHARS = 10_000_000;

function pruneExecutions(): void {
  for (const [id, entry] of executions) {
    if (Date.now() - entry.completedAt > EXECUTION_TTL_MS) executions.delete(id);
  }
  let size = [...executions.values()].reduce((sum, entry) => sum + entry.stdout.length + entry.stderr.length, 0);
  for (const [id, entry] of executions) {
    if (size <= EXECUTION_CACHE_CHARS && executions.size <= 50) break;
    size -= entry.stdout.length + entry.stderr.length;
    executions.delete(id);
  }
}

function executionPage(entry: CompletedExecution, stdoutOffset: number, stderrOffset: number, maxChars: number) {
  const stdout = entry.stdout.slice(stdoutOffset, stdoutOffset + maxChars);
  const stderr = entry.stderr.slice(stderrOffset, stderrOffset + maxChars);
  return {
    ...entry.metadata,
    execution_id: entry.id,
    output_expires_at: new Date(entry.completedAt + EXECUTION_TTL_MS).toISOString(),
    stdout, stderr,
    stdout_offset: stdoutOffset,
    stdout_chars: stdout.length,
    stdout_total_chars: entry.stdoutTotal,
    stdout_stored_chars: entry.stdout.length,
    stdout_dropped_chars: entry.stdoutTotal - entry.stdout.length,
    stdout_truncated: stdoutOffset + stdout.length < entry.stdoutTotal,
    stdout_next_offset: stdoutOffset + stdout.length < entry.stdout.length ? stdoutOffset + stdout.length : null,
    stderr_offset: stderrOffset,
    stderr_total_chars: entry.stderrTotal,
    stderr_dropped_chars: entry.stderrTotal - entry.stderr.length,
    stderr_next_offset: stderrOffset + stderr.length < entry.stderr.length ? stderrOffset + stderr.length : null,
  };
}

type OutputWindow = { offset: number; maxChars: number };

async function collectForegroundOutput(
  child: ChildProcessWithoutNullStreams,
  descriptor: { command: string; cwd: string; shell: ShellKind },
  timeoutMs: number,
  window: OutputWindow,
): Promise<Record<string, unknown>> {
  let stdout = '';
  let stderr = '';
  let stdoutTotalChars = 0;
  let stderrTotalChars = 0;
  let timedOut = false;
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', (incoming: string) => {
    stdoutTotalChars += incoming.length;
    stdout += incoming.slice(0, Math.max(0, Math.min(config.maxProcessBufferChars, EXECUTION_CACHE_CHARS / 2) - stdout.length));
  });
  child.stderr.on('data', (incoming: string) => {
    stderrTotalChars += incoming.length;
    stderr += incoming.slice(0, Math.max(0, Math.min(config.maxProcessBufferChars, EXECUTION_CACHE_CHARS / 2) - stderr.length));
  });

  const timer = setTimeout(() => {
    timedOut = true;
    const record: ManagedProcess = {
      id: 'foreground-timeout',
      ...descriptor,
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

  const entry: CompletedExecution = {
    id: randomUUID(), cwd: descriptor.cwd, completedAt: Date.now(),
    stdout, stderr, stdoutTotal: stdoutTotalChars, stderrTotal: stderrTotalChars,
    metadata: { ...descriptor, exit_code: result.exitCode, signal: result.signal, timed_out: timedOut },
  };
  executions.set(entry.id, entry);
  pruneExecutions();
  return executionPage(entry, 0, 0, window.maxChars);
}

async function runForeground(
  command: string,
  cwd: string,
  shell: ShellKind,
  env: Record<string, string> | undefined,
  timeoutMs: number,
  window: OutputWindow,
): Promise<Record<string, unknown>> {
  const child = createChild(command, cwd, shell, env);
  return collectForegroundOutput(child, { command, cwd, shell }, timeoutMs, window);
}

async function runProcessForeground(
  executable: string,
  args: string[],
  cwd: string,
  env: Record<string, string> | undefined,
  timeoutMs: number,
  window: OutputWindow,
): Promise<Record<string, unknown>> {
  const child = spawn(executable, args, {
    cwd,
    env: { ...process.env, ...(env ?? {}) },
    windowsHide: true,
    detached: process.platform !== 'win32',
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const result = await collectForegroundOutput(
    child,
    { command: [executable, ...args].join(' '), cwd, shell: 'auto' },
    timeoutMs,
    window,
  );
  return { ...result, executable, args };
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
  requirePermission('shell', 'Shell tools');
}

export function registerShellTools(server: McpServer): void {
  server.registerTool(
    'run_command',
    {
      title: 'Run local command',
      description:
        'Execute an arbitrary local shell command as the OS user running ChatX. This is intentionally powerful: filesystem root restrictions do NOT sandbox commands. Auto uses PowerShell on Windows and /bin/sh on Unix. Use background=true for long-running processes. Foreground calls return execution_id; use execution_output to read subsequent pages without re-running the command.',
      inputSchema: z.object({
        command: z.string().min(1),
        cwd: z.string().optional(),
        shell: z.enum(['auto', 'powershell', 'cmd', 'bash', 'sh']).default('auto'),
        background: z.boolean().default(false),
        output_offset: z.number().int().min(0).max(100_000_000).default(0),
        max_output_chars: z.number().int().min(1_000).max(config.maxCommandOutputChars).default(config.maxCommandOutputChars),
        timeout_ms: z.number().int().min(100).max(60 * 60 * 1000).optional(),
        env: z.record(z.string(), z.string()).optional(),
      }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: true },
    },
    async ({ command, cwd: inputCwd, shell, background, timeout_ms, output_offset, max_output_chars, env }) => {
      try {
        requireShellEnabled();
        if (output_offset !== 0) throw new Error('Use execution_output with the previous execution_id to read more output; commands are never re-run for pagination.');
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

        const result = await runForeground(
          command,
          cwd,
          shell,
          env,
          timeout_ms ?? config.defaultCommandTimeoutMs,
          { offset: output_offset, maxChars: max_output_chars },
        );
        return textResult(result);
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'run_process',
    {
      title: 'Run local process',
      description:
        'Execute an executable with an argument array directly, without a command shell. Requires Shell permission and is not restricted by filesystem roots. Returns execution_id; use execution_output for subsequent output pages.',
      inputSchema: z.object({
        executable: z.string().min(1),
        args: z.array(z.string()).max(500).default([]),
        cwd: z.string().optional(),
        timeout_ms: z.number().int().min(100).max(60 * 60 * 1000).optional(),
        output_offset: z.number().int().min(0).max(100_000_000).default(0),
        max_output_chars: z.number().int().min(1_000).max(config.maxCommandOutputChars).default(config.maxCommandOutputChars),
        env: z.record(z.string(), z.string()).optional(),
      }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: true },
    },
    async ({ executable, args, cwd: inputCwd, timeout_ms, output_offset, max_output_chars, env }) => {
      try {
        requireShellEnabled();
        if (output_offset !== 0) throw new Error('Use execution_output with the previous execution_id to read more output; commands are never re-run for pagination.');
        const cwd = await resolveCwd(inputCwd);
        return textResult(
          await runProcessForeground(
            executable,
            args,
            cwd,
            env,
            timeout_ms ?? config.defaultCommandTimeoutMs,
            { offset: output_offset, maxChars: max_output_chars },
          ),
        );
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'execution_output',
    {
      title: 'Read completed execution output',
      description: 'Read cached stdout/stderr by execution_id without running the command again. Output expires after 10 minutes and may be evicted earlier at the cache limit.',
      inputSchema: z.object({
        execution_id: z.string().uuid(),
        stdout_offset: z.number().int().min(0).max(100_000_000).default(0),
        stderr_offset: z.number().int().min(0).max(100_000_000).default(0),
        max_chars: z.number().int().min(1_000).max(config.maxCommandOutputChars).default(config.maxCommandOutputChars),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ execution_id, stdout_offset, stderr_offset, max_chars }) => {
      try {
        requireShellEnabled();
        pruneExecutions();
        const entry = executions.get(execution_id);
        if (!entry) throw new Error('Unknown or expired execution_id. The command was not re-run.');
        await assertPathAllowed(entry.cwd);
        return textResult(executionPage(entry, stdout_offset, stderr_offset, max_chars));
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
      description: 'List background processes started through this ChatX server instance.',
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
