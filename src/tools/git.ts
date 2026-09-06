import { spawn } from 'node:child_process';
import fs from 'node:fs/promises';
import path from 'node:path';
import type { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { config } from '../config.js';
import { requirePermission, settingsDirectoryPath } from '../settings.js';
import { assertExistingPath } from '../security/path-policy.js';
import { errorResult, textResult } from '../utils/results.js';

async function resolveRepo(inputPath: string): Promise<string> {
  const resolved = await assertExistingPath(inputPath);
  const stat = await fs.stat(resolved);
  if (!stat.isDirectory()) throw new Error(`Repository path is not a directory: ${resolved}`);
  return resolved;
}

async function runGit(
  cwd: string,
  args: string[],
  timeoutMs = config.defaultCommandTimeoutMs,
): Promise<{ exit_code: number | null; signal: NodeJS.Signals | null; stdout: string; stderr: string }> {
  return await new Promise((resolve, reject) => {
    const child = spawn('git', args, {
      cwd,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: process.env,
    });

    let stdout = '';
    let stderr = '';
    const cap = config.maxCommandOutputChars;
    const append = (current: string, chunk: Buffer): string => {
      const value = current + chunk.toString('utf8');
      return value.length > cap ? value.slice(value.length - cap) : value;
    };

    child.stdout.on('data', (chunk: Buffer) => {
      stdout = append(stdout, chunk);
    });
    child.stderr.on('data', (chunk: Buffer) => {
      stderr = append(stderr, chunk);
    });

    const timer = setTimeout(() => child.kill('SIGTERM'), timeoutMs);
    timer.unref();

    child.once('error', reject);
    child.once('close', (exitCode, signal) => {
      clearTimeout(timer);
      resolve({ exit_code: exitCode, signal, stdout, stderr });
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

async function disabledHooksDirectory(): Promise<string> {
  const directory = path.join(settingsDirectoryPath(), 'disabled-git-hooks');
  await fs.mkdir(directory, { recursive: true });
  return directory;
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
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ repo, staged, ref, paths, stat }) => {
      try {
        requirePermission('gitRead', 'Git read tools');
        const cwd = await resolveRepo(repo);
        const args = ['diff'];
        if (staged) args.push('--cached');
        if (stat) args.push('--stat');
        if (ref) args.push(validateRefName(ref, 'Git ref'));
        if (paths && paths.length > 0) args.push('--', ...paths);
        const result = await runGit(cwd, args);
        return textResult({ repo: cwd, ...result });
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
        const result = await runGit(cwd, ['add', '--', ...paths]);
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
        const result = await runGit(cwd, ['restore', '--staged', '--', ...paths]);
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
        const hooks = await disabledHooksDirectory();
        const args = [
          '-c', `core.hooksPath=${hooks}`,
          '-c', 'commit.gpgSign=false',
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
        const cwd = await resolveRepo(repo);
        const result = await runGit(cwd, args, timeout_ms ?? config.defaultCommandTimeoutMs);
        return textResult({ repo: cwd, args, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );
}
