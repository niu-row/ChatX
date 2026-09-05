import { spawn } from 'node:child_process';
import fs from 'node:fs/promises';
import type { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { config } from '../config.js';
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
        const cwd = await resolveRepo(repo);
        const args = ['diff'];
        if (staged) args.push('--cached');
        if (stat) args.push('--stat');
        if (ref) args.push(ref);
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
        const cwd = await resolveRepo(repo);
        const args = [
          'log',
          `--max-count=${max_count}`,
          '--date=iso-strict',
          '--pretty=format:%H%x09%an%x09%ad%x09%s',
        ];
        if (ref) args.push(ref);
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
      title: 'Run Git command',
      description:
        'Run arbitrary Git arguments in a repository without invoking a shell. This can modify the working tree, index, refs, or remotes depending on the arguments.',
      inputSchema: z.object({
        repo: z.string(),
        args: z.array(z.string()).min(1),
        timeout_ms: z.number().int().min(100).max(60 * 60 * 1000).optional(),
      }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: true },
    },
    async ({ repo, args, timeout_ms }) => {
      try {
        const cwd = await resolveRepo(repo);
        const result = await runGit(cwd, args, timeout_ms ?? config.defaultCommandTimeoutMs);
        return textResult({ repo: cwd, args, ...result });
      } catch (error) {
        return errorResult(error);
      }
    },
  );
}
