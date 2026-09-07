import { spawn, type ChildProcess } from 'node:child_process';
import fs from 'node:fs/promises';
import type { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { config } from '../config.js';
import { requirePermission } from '../settings.js';
import {
  gitEnvironment,
  restrictedGitOptions,
  validateGitRepo,
  validateGitPaths,
  disabledFilterOptions,
} from '../security/git-policy.js';
import { assertExistingPath } from '../security/path-policy.js';
import { errorResult, textResult } from '../utils/results.js';

type GitCommandResult = {
  exit_code: number | null;
  signal: NodeJS.Signals | null;
  timed_out: boolean;
  stdout: string;
  stderr: string;
  stdout_dropped_chars: number;
  stderr_dropped_chars: number;
  stdout_total_chars: number;
};

const gitCommandOutputSchema = z.looseObject({
  exit_code: z.number().int().nullable(),
  signal: z.string().nullable(),
  timed_out: z.boolean(),
  stdout: z.string(),
  stderr: z.string(),
  stdout_dropped_chars: z.number().int(),
  stderr_dropped_chars: z.number().int(),
  stdout_total_chars: z.number().int(),
});

const gitDiffSummarySchema = z.looseObject({
  file_count: z.number().int(),
  returned_additions: z.number().int(),
  returned_deletions: z.number().int(),
  totals_complete: z.boolean(),
  reached_file_limit: z.boolean(),
  files: z.array(z.looseObject({
    path: z.string(),
    additions: z.number().int().nullable(),
    deletions: z.number().int().nullable(),
    binary: z.boolean(),
  })),
});

async function killProcessTree(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const pid = child.pid;
  if (!pid) return;
  if (process.platform === 'win32') {
    await new Promise<void>((resolve) => {
      const killer = spawn('taskkill', ['/PID', String(pid), '/T', '/F'], {
        windowsHide: true,
        stdio: 'ignore',
      });
      killer.once('close', () => resolve());
      killer.once('error', () => {
        child.kill('SIGTERM');
        resolve();
      });
    });
    return;
  }
  try {
    process.kill(-pid, 'SIGTERM');
  } catch {
    child.kill('SIGTERM');
  }
}

async function executeGit(
  cwd: string,
  args: string[],
  timeoutMs = config.defaultCommandTimeoutMs,
  stdoutWindow?: { offset: number; maxChars: number },
  env: NodeJS.ProcessEnv = process.env,
): Promise<GitCommandResult> {
  return await new Promise((resolve, reject) => {
    const child = spawn('git', args, {
      cwd,
      windowsHide: true,
      detached: process.platform !== 'win32',
      stdio: ['ignore', 'pipe', 'pipe'],
      env,
    });
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');

    let stdout = '';
    let stderr = '';
    let stdoutDroppedChars = 0;
    let stderrDroppedChars = 0;
    let stdoutTotalChars = 0;
    let timedOut = false;
    const cap = config.maxCommandOutputChars;

    child.stdout.on('data', (incoming: string) => {
      const chunkStart = stdoutTotalChars;
      stdoutTotalChars += incoming.length;
      if (stdoutWindow) {
        const from = Math.max(0, stdoutWindow.offset - chunkStart);
        const to = Math.min(incoming.length, stdoutWindow.offset + stdoutWindow.maxChars - chunkStart);
        if (to > from) stdout += incoming.slice(from, to);
        stdoutDroppedChars = stdoutTotalChars - stdout.length;
      } else {
        const remaining = Math.max(0, cap - stdout.length);
        stdout += incoming.slice(0, remaining);
        stdoutDroppedChars += Math.max(0, incoming.length - remaining);
      }
    });
    child.stderr.on('data', (incoming: string) => {
      const remaining = Math.max(0, cap - stderr.length);
      stderr += incoming.slice(0, remaining);
      stderrDroppedChars += Math.max(0, incoming.length - remaining);
    });

    const timer = setTimeout(() => {
      timedOut = true;
      void killProcessTree(child);
    }, timeoutMs);
    timer.unref();

    child.once('error', reject);
    child.once('close', (exitCode, signal) => {
      clearTimeout(timer);
      resolve({
        exit_code: exitCode,
        signal,
        timed_out: timedOut,
        stdout,
        stderr,
        stdout_dropped_chars: stdoutDroppedChars,
        stderr_dropped_chars: stderrDroppedChars,
        stdout_total_chars: stdoutTotalChars,
      });
    });
  });
}

export async function resolveRepo(inputPath: string): Promise<string> {
  return validateGitRepo(inputPath, probeGit);
}

async function probeGit(cwd: string, args: string[]) {
  return executeGit(
    cwd,
    [...restrictedGitOptions, ...args],
    config.defaultCommandTimeoutMs,
    undefined,
    gitEnvironment(),
  );
}

export async function runGit(
  cwd: string,
  args: string[],
  timeoutMs = config.defaultCommandTimeoutMs,
  stdoutWindow?: { offset: number; maxChars: number },
): Promise<GitCommandResult> {
  const filters = await disabledFilterOptions(cwd, probeGit);
  const safeArgs = [...args];
  if (safeArgs[0] === 'diff' || safeArgs[0] === 'log') {
    safeArgs.splice(1, 0, '--no-ext-diff', '--no-textconv');
  }
  if (safeArgs[0] === 'diff' || safeArgs[0] === 'status') {
    safeArgs.splice(1, 0, '--ignore-submodules=all');
  }
  return executeGit(
    cwd,
    [...restrictedGitOptions, ...filters, ...safeArgs],
    timeoutMs,
    stdoutWindow,
    gitEnvironment(),
  );
}

function ensureGitSuccess(result: GitCommandResult, operation: string): GitCommandResult {
  if (result.timed_out) throw new Error(`${operation} timed out.`);
  if (result.exit_code !== 0) {
    const details = result.stderr.trim() || result.stdout.trim() || `exit code ${result.exit_code}`;
    throw new Error(`${operation} failed: ${details}`);
  }
  return result;
}

function validateRefName(value: string, label: string): string {
  const trimmed = value.trim();
  if (!trimmed || trimmed.startsWith('-') || /[\u0000-\u001f\u007f]/.test(trimmed)) {
    throw new Error(`${label} is invalid.`);
  }
  return trimmed;
}

async function hasHead(cwd: string): Promise<boolean> {
  const result = await probeGit(cwd, ['rev-parse', '--verify', 'HEAD']);
  return result.exit_code === 0;
}

function parseStatus(stdout: string): Record<string, unknown> {
  const lines = stdout.split(/\r?\n/).filter(Boolean);
  const branch = lines[0]?.startsWith('## ') ? lines.shift()!.slice(3) : null;
  return {
    branch,
    clean: lines.length === 0,
    changes: lines.map((line) => ({
      code: line.slice(0, 2),
      path: line.length > 3 ? line.slice(3) : '',
    })),
  };
}

function parseCommits(stdout: string): Array<Record<string, unknown>> {
  return stdout.split(/\r?\n/).filter(Boolean).map((line) => {
    const [hash = '', author = '', date = '', ...subjectParts] = line.split('\t');
    return { hash, author, date, subject: subjectParts.join('\t') };
  });
}

function parseDiffSummary(result: GitCommandResult, maxFiles: number): Record<string, unknown> {
  const rows = result.stdout.split(/\r?\n/).filter(Boolean);
  const files = rows.slice(0, maxFiles).map((line) => {
    const [addedText = '-', deletedText = '-', ...fileParts] = line.split('\t');
    return {
      path: fileParts.join('\t'),
      additions: addedText === '-' ? null : Number(addedText),
      deletions: deletedText === '-' ? null : Number(deletedText),
      binary: addedText === '-' || deletedText === '-',
    };
  });
  const reachedFileLimit = rows.length > maxFiles || result.stdout_dropped_chars > 0;
  return {
    file_count: files.length,
    returned_additions: files.reduce((sum, file) => sum + (file.additions ?? 0), 0),
    returned_deletions: files.reduce((sum, file) => sum + (file.deletions ?? 0), 0),
    totals_complete: !reachedFileLimit,
    reached_file_limit: reachedFileLimit,
    files,
  };
}

export function registerGitTools(server: McpServer): void {
  server.registerTool(
    'git_inspect',
    {
      title: 'Inspect Git repository',
      description:
        'Get a compact Git overview. Select status, recent commits, and/or diff_summary. Use git_diff only when the full patch is needed.',
      inputSchema: z.object({
        repo: z.string(),
        sections: z.array(z.enum(['status', 'log', 'diff_summary'])).min(1).max(3)
          .default(['status', 'log', 'diff_summary']),
        max_count: z.number().int().min(1).max(100).default(10),
        ref: z.string().optional(),
        staged: z.boolean().default(false),
        paths: z.array(z.string()).max(500).optional(),
        max_files: z.number().int().min(1).max(2_000).default(500),
      }).strict(),
      outputSchema: z.looseObject({
        repo: z.string(),
        status: z.looseObject({
          branch: z.string().nullable(),
          clean: z.boolean(),
          changes: z.array(z.looseObject({ code: z.string(), path: z.string() })),
        }).nullable(),
        commits: z.array(z.looseObject({
          hash: z.string(),
          author: z.string(),
          date: z.string(),
          subject: z.string(),
        })).nullable(),
        diff_summary: gitDiffSummarySchema.nullable(),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, sections, max_count, ref, staged, paths, max_files }) => {
      try {
        requirePermission('gitRead', 'Git read tools');
        const cwd = await resolveRepo(repo);
        const selected = new Set(sections);
        const validatedRef = ref ? validateRefName(ref, 'Git ref') : null;
        const validatedPaths = selected.has('diff_summary') && paths?.length
          ? await validateGitPaths(cwd, paths)
          : [];
        const headExists = selected.has('log') ? await hasHead(cwd) : true;

        const statusPromise = selected.has('status')
          ? runGit(cwd, ['status', '--porcelain=v1', '--branch'])
          : Promise.resolve(null);
        const logArgs = [
          'log',
          `--max-count=${max_count}`,
          '--date=iso-strict',
          '--pretty=format:%H%x09%an%x09%ad%x09%s',
        ];
        if (validatedRef) logArgs.push(validatedRef);
        const logPromise = selected.has('log') && headExists
          ? runGit(cwd, logArgs)
          : Promise.resolve(null);
        const diffArgs = ['diff', '--numstat'];
        if (staged) diffArgs.push('--cached');
        if (validatedRef) diffArgs.push(validatedRef);
        if (validatedPaths.length > 0) diffArgs.push('--', ...validatedPaths);
        const diffPromise = selected.has('diff_summary')
          ? runGit(cwd, diffArgs)
          : Promise.resolve(null);

        const [statusResult, logResult, diffResult] = await Promise.all([
          statusPromise,
          logPromise,
          diffPromise,
        ]);
        if (statusResult) ensureGitSuccess(statusResult, 'git status');
        if (logResult) ensureGitSuccess(logResult, 'git log');
        if (diffResult) ensureGitSuccess(diffResult, 'git diff summary');

        return textResult({
          repo: cwd,
          status: statusResult ? parseStatus(statusResult.stdout) : null,
          commits: selected.has('log') ? (logResult ? parseCommits(logResult.stdout) : []) : null,
          diff_summary: diffResult ? parseDiffSummary(diffResult, max_files) : null,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_diff',
    {
      title: 'Git diff',
      description: 'Read the full Git patch. Prefer git_inspect with diff_summary when only changed files and line counts are needed.',
      inputSchema: z.object({
        repo: z.string(),
        staged: z.boolean().default(false),
        ref: z.string().optional(),
        paths: z.array(z.string()).max(500).optional(),
        offset: z.number().int().min(0).max(100_000_000).default(0),
        max_chars: z.number().int().min(1_024).max(config.maxCommandOutputChars).default(100_000),
      }).strict(),
      outputSchema: gitCommandOutputSchema.extend({
        repo: z.string(),
        output_offset: z.number().int(),
        output_chars: z.number().int(),
        total_chars: z.number().int(),
        truncated: z.boolean(),
        next_offset: z.number().int().nullable(),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, staged, ref, paths, offset, max_chars }) => {
      try {
        requirePermission('gitRead', 'Git read tools');
        const cwd = await resolveRepo(repo);
        const args = ['diff'];
        if (staged) args.push('--cached');
        if (ref) args.push(validateRefName(ref, 'Git ref'));
        if (paths?.length) args.push('--', ...await validateGitPaths(cwd, paths));
        const result = ensureGitSuccess(
          await runGit(cwd, args, config.defaultCommandTimeoutMs, { offset, maxChars: max_chars }),
          'git diff',
        );
        const nextOffset = offset + result.stdout.length < result.stdout_total_chars
          ? offset + result.stdout.length
          : null;
        return textResult({
          repo: cwd,
          ...result,
          output_offset: offset,
          output_chars: result.stdout.length,
          total_chars: result.stdout_total_chars,
          truncated: nextOffset !== null,
          next_offset: nextOffset,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'git_index',
    {
      title: 'Update Git index',
      description: 'Stage or unstage explicitly named literal paths without switching branches or changing working-tree file content.',
      inputSchema: z.object({
        repo: z.string(),
        action: z.enum(['stage', 'unstage']),
        paths: z.array(z.string().min(1)).min(1).max(500),
      }),
      outputSchema: gitCommandOutputSchema.extend({
        repo: z.string(),
        action: z.enum(['stage', 'unstage']),
        paths: z.array(z.string()),
      }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, action, paths }) => {
      try {
        requirePermission('gitWrite', 'Git write tools');
        const cwd = await resolveRepo(repo);
        const validatedPaths = await validateGitPaths(cwd, paths);
        let result: GitCommandResult;
        if (action === 'stage') {
          result = await runGit(cwd, ['add', '--', ...validatedPaths]);
        } else if (await hasHead(cwd)) {
          result = await runGit(cwd, ['restore', '--staged', '--', ...validatedPaths]);
        } else {
          result = await runGit(cwd, ['rm', '--cached', '-r', '--ignore-unmatch', '--', ...validatedPaths]);
        }
        ensureGitSuccess(result, `git ${action}`);
        return textResult({ repo: cwd, action, paths: validatedPaths, ...result });
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
      outputSchema: gitCommandOutputSchema.extend({ repo: z.string(), branch: z.string() }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, name, start_point }) => {
      try {
        requirePermission('gitWrite', 'Git write tools');
        const cwd = await resolveRepo(repo);
        const branch = validateRefName(name, 'Branch name');
        ensureGitSuccess(await runGit(cwd, ['check-ref-format', '--branch', branch]), 'branch name validation');
        const args = ['branch', branch];
        if (start_point) args.push(validateRefName(start_point, 'Start point'));
        const result = ensureGitSuccess(await runGit(cwd, args), 'git branch');
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
      description: 'Create a local commit from already-staged changes. Repository hooks and GPG signing are disabled.',
      inputSchema: z.object({
        repo: z.string(),
        message: z.string().min(1).max(20_000),
        allow_empty: z.boolean().default(false),
      }),
      outputSchema: gitCommandOutputSchema.extend({ repo: z.string() }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
    },
    async ({ repo, message, allow_empty }) => {
      try {
        requirePermission('gitWrite', 'Git write tools');
        const cwd = await resolveRepo(repo);
        const args = ['commit', '--no-verify', '--no-gpg-sign', '--message', message];
        if (allow_empty) args.push('--allow-empty');
        const result = ensureGitSuccess(await runGit(cwd, args), 'git commit');
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
        'Escape hatch for arbitrary Git arguments. This is shell-equivalent because Git aliases/configuration can execute external programs. Requires Shell, Advanced Git, Git read, and Git write permissions.',
      inputSchema: z.object({
        repo: z.string(),
        args: z.array(z.string()).min(1).max(200),
        timeout_ms: z.number().int().min(100).max(60 * 60 * 1000).optional(),
      }),
      outputSchema: gitCommandOutputSchema.extend({ repo: z.string(), args: z.array(z.string()) }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: true },
    },
    async ({ repo, args, timeout_ms }) => {
      try {
        requirePermission('shell', 'Shell tools required by Advanced Git');
        requirePermission('gitAdvanced', 'Advanced Git command execution');
        requirePermission('gitWrite', 'Git write tools');
        requirePermission('gitRead', 'Git read tools');
        const cwd = await assertExistingPath(repo);
        if (!(await fs.stat(cwd)).isDirectory()) throw new Error('Repository path must be a directory.');
        const result = await executeGit(cwd, args, timeout_ms ?? config.defaultCommandTimeoutMs);
        return textResult({ repo: cwd, args, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );
}
