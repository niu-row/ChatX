import { spawn } from 'node:child_process';
import fs from 'node:fs/promises';
import type { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { config } from '../config.js';
import { requirePermission } from '../settings.js';
import { gitEnvironment, restrictedGitOptions, validateGitRepo, validateGitPaths, disabledFilterOptions } from '../security/git-policy.js';
import { assertExistingPath } from '../security/path-policy.js';
import { errorResult, textResult } from '../utils/results.js';

export async function resolveRepo(inputPath: string): Promise<string> {
  return validateGitRepo(inputPath, probeGit);
}

async function probeGit(cwd: string, args: string[]) {
  return executeGit(cwd, [...restrictedGitOptions, ...args], config.defaultCommandTimeoutMs, undefined, gitEnvironment());
}

export async function runGit(cwd: string, args: string[], timeoutMs = config.defaultCommandTimeoutMs,
  stdoutWindow?: { offset: number; maxChars: number }) {
  const filters = await disabledFilterOptions(cwd, probeGit);
  const safeArgs = [...args];
  if (safeArgs[0] === 'diff' || safeArgs[0] === 'log') {
    safeArgs.splice(1, 0, '--no-ext-diff', '--no-textconv');
  }
  if (safeArgs[0] === 'diff' || safeArgs[0] === 'status') safeArgs.splice(1, 0, '--ignore-submodules=all');
  return executeGit(cwd, [...restrictedGitOptions, ...filters, ...safeArgs], timeoutMs, stdoutWindow, gitEnvironment());
}

async function executeGit(
  cwd: string,
  args: string[],
  timeoutMs = config.defaultCommandTimeoutMs,
  stdoutWindow?: { offset: number; maxChars: number },
  env: NodeJS.ProcessEnv = process.env,
): Promise<{
  exit_code: number | null;
  signal: NodeJS.Signals | null;
  stdout: string;
  stderr: string;
  stdout_dropped_chars: number;
  stderr_dropped_chars: number;
  stdout_total_chars: number;
}> {
  return await new Promise((resolve, reject) => {
    const child = spawn('git', args, {
      cwd,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe'],
      env,
    });

    let stdout = '';
    let stderr = '';
    let stdoutDroppedChars = 0;
    let stderrDroppedChars = 0;
    let stdoutTotalChars = 0;
    const cap = config.maxCommandOutputChars;
    const append = (current: string, chunk: Buffer): { value: string; dropped: number } => {
      const incoming = chunk.toString('utf8');
      const remaining = Math.max(0, cap - current.length);
      return {
        value: current + incoming.slice(0, remaining),
        dropped: Math.max(0, incoming.length - remaining),
      };
    };

    child.stdout.on('data', (chunk: Buffer) => {
      const incoming = chunk.toString('utf8');
      const chunkStart = stdoutTotalChars;
      stdoutTotalChars += incoming.length;
      if (stdoutWindow) {
        const from = Math.max(0, stdoutWindow.offset - chunkStart);
        const to = Math.min(incoming.length, stdoutWindow.offset + stdoutWindow.maxChars - chunkStart);
        if (to > from) stdout += incoming.slice(from, to);
        stdoutDroppedChars = stdoutTotalChars - stdout.length;
      } else {
        const appended = append(stdout, chunk);
        stdout = appended.value;
        stdoutDroppedChars += appended.dropped;
      }
    });
    child.stderr.on('data', (chunk: Buffer) => {
      const appended = append(stderr, chunk);
      stderr = appended.value;
      stderrDroppedChars += appended.dropped;
    });

    const timer = setTimeout(() => child.kill('SIGTERM'), timeoutMs);
    timer.unref();

    child.once('error', reject);
    child.once('close', (exitCode, signal) => {
      clearTimeout(timer);
      resolve({
        exit_code: exitCode,
        signal,
        stdout,
        stderr,
        stdout_dropped_chars: stdoutDroppedChars,
        stderr_dropped_chars: stderrDroppedChars,
        stdout_total_chars: stdoutTotalChars,
      });
    });
  });
}

function validateRefName(value: string, label: string): string {
  const trimmed = value.trim();
  if (!trimmed || trimmed.startsWith('-') || /[\u0000-\u001f\u007f]/.test(trimmed)) {
    throw new Error(`${label} is invalid.`);
  }
  return trimmed;
}

export function registerGitTools(server: McpServer): void {
  server.registerTool(
    'git_status',
    {
      title: 'Git status',
      description: 'Show Git repository status including branch and working tree changes.',
      inputSchema: z.object({ repo: z.string() }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo }) => {
      try {
        requirePermission('gitRead', 'Git read tools');
        const cwd = await resolveRepo(repo);
        const result = await runGit(cwd, ['status', '--porcelain=v1', '--branch']);
        return textResult({ repo: cwd, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_diff',
    {
      title: 'Git diff',
      description: 'Show a Git diff for the working tree, staged changes, or against a supplied ref.',
      inputSchema: z.object({
        repo: z.string(),
        staged: z.boolean().default(false),
        ref: z.string().optional(),
        paths: z.array(z.string()).optional(),
        stat: z.boolean().default(false),
        offset: z.number().int().min(0).max(100_000_000).default(0),
        max_chars: z.number().int().min(1_024).max(config.maxCommandOutputChars).default(100_000),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, staged, ref, paths, stat, offset, max_chars }) => {
      try {
        requirePermission('gitRead', 'Git read tools');
        const cwd = await resolveRepo(repo);
        const args = ['diff'];
        if (staged) args.push('--cached');
        if (stat) args.push('--stat');
        if (ref) args.push(validateRefName(ref, 'Git ref'));
        if (paths && paths.length > 0) args.push('--', ...await validateGitPaths(cwd, paths));
        const result = await runGit(cwd, args, config.defaultCommandTimeoutMs, {
          offset,
          maxChars: max_chars,
        });
        const stdout = result.stdout;
        const knownTotalChars = result.stdout_total_chars;
        const nextOffset = offset + stdout.length < knownTotalChars ? offset + stdout.length : null;
        return textResult({
          repo: cwd,
          ...result,
          stdout,
          output_offset: offset,
          output_chars: stdout.length,
          total_chars: knownTotalChars,
          truncated: nextOffset !== null,
          next_offset: nextOffset,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_diff_summary',
    {
      title: 'Git diff summary',
      description: 'Return compact per-file additions and deletions without transferring full patch content.',
      inputSchema: z.object({
        repo: z.string(),
        staged: z.boolean().default(false),
        ref: z.string().optional(),
        paths: z.array(z.string()).optional(),
        max_files: z.number().int().min(1).max(10_000).default(1_000),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, staged, ref, paths, max_files }) => {
      try {
        requirePermission('gitRead', 'Git read tools');
        const cwd = await resolveRepo(repo);
        const args = ['diff', '--numstat'];
        if (staged) args.push('--cached');
        if (ref) args.push(validateRefName(ref, 'Git ref'));
        if (paths && paths.length > 0) args.push('--', ...await validateGitPaths(cwd, paths));
        const result = await runGit(cwd, args);
        const rows = result.stdout.split(/\r?\n/).filter(Boolean);
        const files = rows.slice(0, max_files).map((line) => {
          const [addedText = '-', deletedText = '-', ...fileParts] = line.split('\t');
          return {
            path: fileParts.join('\t'),
            additions: addedText === '-' ? null : Number(addedText),
            deletions: deletedText === '-' ? null : Number(deletedText),
            binary: addedText === '-' || deletedText === '-',
          };
        });
        return textResult({
          repo: cwd,
          exit_code: result.exit_code,
          signal: result.signal,
          stderr: result.stderr,
          file_count: files.length,
          total_additions: files.reduce((sum, file) => sum + (file.additions ?? 0), 0),
          total_deletions: files.reduce((sum, file) => sum + (file.deletions ?? 0), 0),
          reached_file_limit: rows.length > max_files || result.stdout_dropped_chars > 0,
          files,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_log',
    {
      title: 'Git log',
      description: 'Show recent Git commits in a compact machine-readable format.',
      inputSchema: z.object({
        repo: z.string(),
        max_count: z.number().int().min(1).max(500).default(30),
        ref: z.string().optional(),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, max_count, ref }) => {
      try {
        requirePermission('gitRead', 'Git read tools');
        const cwd = await resolveRepo(repo);
        const args = [
          'log',
          `--max-count=${max_count}`,
          '--date=iso-strict',
          '--pretty=format:%H%x09%an%x09%ad%x09%s',
        ];
        if (ref) args.push(validateRefName(ref, 'Git ref'));
        const result = await runGit(cwd, args);
        return textResult({ repo: cwd, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_inspect',
    {
      title: 'Inspect Git repository',
      description:
        'Return status, recent commits, and an optional diff summary concurrently in one read-only MCP call.',
      inputSchema: z.object({
        repo: z.string(),
        max_count: z.number().int().min(1).max(100).default(10),
        ref: z.string().optional(),
        include_diff_stat: z.boolean().default(true),
        paths: z.array(z.string()).max(500).optional(),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, max_count, ref, include_diff_stat, paths }) => {
      try {
        requirePermission('gitRead', 'Git read tools');
        const cwd = await resolveRepo(repo);
        const validatedRef = ref ? validateRefName(ref, 'Git ref') : null;
        const logArgs = [
          'log',
          `--max-count=${max_count}`,
          '--date=iso-strict',
          '--pretty=format:%H%x09%an%x09%ad%x09%s',
        ];
        if (validatedRef) logArgs.push(validatedRef);
        const diffArgs = ['diff', '--stat'];
        if (validatedRef) diffArgs.push(validatedRef);
        if (paths && paths.length > 0) diffArgs.push('--', ...await validateGitPaths(cwd, paths));

        const [status, log, diffStat] = await Promise.all([
          runGit(cwd, ['status', '--porcelain=v1', '--branch']),
          runGit(cwd, logArgs),
          include_diff_stat ? runGit(cwd, diffArgs) : Promise.resolve(null),
        ]);
        return textResult({
          repo: cwd,
          status,
          log,
          diff_stat: diffStat,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_stage',
    {
      title: 'Stage Git paths',
      description: 'Stage explicitly named paths with git add. Paths are passed after -- so they cannot be interpreted as Git options.',
      inputSchema: z.object({ repo: z.string(), paths: z.array(z.string().min(1)).min(1).max(500) }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, paths }) => {
      try {
        requirePermission('gitWrite', 'Git write tools');
        const cwd = await resolveRepo(repo);
        const result = await runGit(cwd, ['add', '--', ...await validateGitPaths(cwd, paths)]);
        return textResult({ repo: cwd, paths, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_unstage',
    {
      title: 'Unstage Git paths',
      description: 'Remove explicitly named paths from the index while keeping working-tree content intact.',
      inputSchema: z.object({ repo: z.string(), paths: z.array(z.string().min(1)).min(1).max(500) }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, paths }) => {
      try {
        requirePermission('gitWrite', 'Git write tools');
        const cwd = await resolveRepo(repo);
        const result = await runGit(cwd, ['restore', '--staged', '--', ...await validateGitPaths(cwd, paths)]);
        return textResult({ repo: cwd, paths, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_create_branch',
    {
      title: 'Create Git branch',
      description: 'Create a local branch without switching the working tree.',
      inputSchema: z.object({ repo: z.string(), name: z.string().min(1), start_point: z.string().optional() }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
    },
    async ({ repo, name, start_point }) => {
      try {
        requirePermission('gitWrite', 'Git write tools');
        const cwd = await resolveRepo(repo);
        const branch = validateRefName(name, 'Branch name');
        const args = ['branch', branch];
        if (start_point) args.push(validateRefName(start_point, 'Start point'));
        const result = await runGit(cwd, args);
        return textResult({ repo: cwd, branch, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_commit',
    {
      title: 'Commit staged Git changes',
      description: 'Create a local commit from already-staged changes. Repository hooks and GPG signing are disabled for this tool.',
      inputSchema: z.object({
        repo: z.string(),
        message: z.string().min(1).max(20_000),
        allow_empty: z.boolean().default(false),
      }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
    },
    async ({ repo, message, allow_empty }) => {
      try {
        requirePermission('gitWrite', 'Git write tools');
        const cwd = await resolveRepo(repo);
        const args = [
          'commit',
          '--no-verify',
          '--no-gpg-sign',
          '--message', message,
        ];
        if (allow_empty) args.push('--allow-empty');
        const result = await runGit(cwd, args);
        return textResult({ repo: cwd, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_run',
    {
      title: 'Run advanced Git command',
      description:
        'Escape hatch for arbitrary Git arguments. Disabled unless Advanced Git is explicitly enabled in the local console; also requires Git read and write permissions.',
      inputSchema: z.object({
        repo: z.string(),
        args: z.array(z.string()).min(1).max(200),
        timeout_ms: z.number().int().min(100).max(60 * 60 * 1000).optional(),
      }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: true },
    },
    async ({ repo, args, timeout_ms }) => {
      try {
        requirePermission('gitAdvanced', 'Advanced Git command execution');
        requirePermission('gitWrite', 'Git write tools');
        requirePermission('gitRead', 'Git read tools');
        const cwd = await assertExistingPath(repo);
        if (!(await fs.stat(cwd)).isDirectory()) throw new Error("Repository path must be a directory.");
        const result = await executeGit(cwd, args, timeout_ms ?? config.defaultCommandTimeoutMs);
        return textResult({ repo: cwd, args, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );
}
