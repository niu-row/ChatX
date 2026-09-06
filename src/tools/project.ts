import fs from 'node:fs/promises';
import path from 'node:path';
import type { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { getRuntimeSettings, requirePermission } from '../settings.js';
import { assertExistingPath, assertPathAllowed } from '../security/path-policy.js';
import { runGit } from './git.js';
import { errorResult, textResult } from '../utils/results.js';

const DEFAULT_EXCLUDED_DIRECTORIES = [
  '.git',
  'node_modules',
  'dist',
  'build',
  '.next',
  '.cache',
  'target',
  'vendor',
];

const DEFAULT_KEY_FILES = [
  'package.json',
  'pyproject.toml',
  'Cargo.toml',
  'go.mod',
  'README.md',
  'tsconfig.json',
];

function resolveProjectFile(root: string, relativePath: string): string {
  if (path.isAbsolute(relativePath)) throw new Error(`Key file path must be relative: ${relativePath}`);
  const resolved = path.resolve(root, relativePath);
  const relative = path.relative(root, resolved);
  if (relative === '..' || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
    throw new Error(`Key file path escapes the project root: ${relativePath}`);
  }
  return resolved;
}

async function collectProjectTree(
  root: string,
  maxDepth: number,
  maxEntries: number,
  excludedDirectories: Set<string>,
): Promise<{ entries: Array<Record<string, unknown>>; reachedEntryLimit: boolean }> {
  const entries: Array<Record<string, unknown>> = [];
  let reachedEntryLimit = false;

  async function visit(current: string, depth: number): Promise<void> {
    if (entries.length >= maxEntries) {
      reachedEntryLimit = true;
      return;
    }

    const children = await fs.readdir(current, { withFileTypes: true });
    children.sort((a, b) => a.name.localeCompare(b.name));
    for (const child of children) {
      if (entries.length >= maxEntries) {
        reachedEntryLimit = true;
        return;
      }

      const fullPath = path.join(current, child.name);
      const relative = path.relative(root, fullPath);
      const type = child.isDirectory()
        ? 'directory'
        : child.isFile()
          ? 'file'
          : child.isSymbolicLink()
            ? 'symlink'
            : 'other';
      const excluded = child.isDirectory() && excludedDirectories.has(child.name);
      entries.push({ path: relative, type, ...(excluded ? { excluded: true } : {}) });

      if (child.isDirectory() && !excluded && depth < maxDepth) {
        await assertPathAllowed(fullPath);
        await visit(fullPath, depth + 1);
      }
    }
  }

  await visit(root, 0);
  return { entries, reachedEntryLimit };
}

async function readKeyFiles(
  root: string,
  keyFiles: string[],
  maxFileBytes: number,
  maxTotalBytes: number,
): Promise<Array<Record<string, unknown>>> {
  const output = new Array<Record<string, unknown>>(keyFiles.length);
  let totalBytes = 0;
  let nextIndex = 0;

  async function worker(): Promise<void> {
    for (;;) {
      const index = nextIndex++;
      if (index >= keyFiles.length) return;
      const relativePath = keyFiles[index];
      if (!relativePath) return;
      try {
        const candidate = resolveProjectFile(root, relativePath);
        const resolved = await assertExistingPath(candidate);
        const stat = await fs.stat(resolved);
        if (!stat.isFile()) {
          output[index] = { path: relativePath, status: 'not_file' };
          continue;
        }

        const bytesToRead = Math.min(stat.size, maxFileBytes);
        if (totalBytes + bytesToRead > maxTotalBytes) {
          output[index] = { path: relativePath, status: 'total_limit' };
          continue;
        }
        totalBytes += bytesToRead;

        const handle = await fs.open(resolved, 'r');
        let buffer: Buffer;
        try {
          buffer = Buffer.alloc(bytesToRead);
          const read = await handle.read(buffer, 0, bytesToRead, 0);
          buffer = buffer.subarray(0, read.bytesRead);
        } finally {
          await handle.close();
        }

        if (buffer.subarray(0, Math.min(buffer.length, 8192)).includes(0)) {
          output[index] = { path: relativePath, status: 'binary', size: stat.size };
          continue;
        }
        output[index] = {
          path: relativePath,
          status: 'ok',
          size: stat.size,
          truncated: stat.size > buffer.length,
          content: buffer.toString('utf8'),
        };
      } catch (error) {
        const code = error && typeof error === 'object' && 'code' in error ? String(error.code) : '';
        output[index] = code === 'ENOENT'
          ? { path: relativePath, status: 'missing' }
          : {
              path: relativePath,
              status: 'error',
              error: error instanceof Error ? error.message : String(error),
            };
      }
    }
  }

  await Promise.all(
    Array.from({ length: Math.min(8, keyFiles.length) }, () => worker()),
  );
  return output.filter((entry): entry is Record<string, unknown> => Boolean(entry));
}

async function isGitRepository(root: string): Promise<boolean> {
  try {
    await fs.access(path.join(root, '.git'));
    return true;
  } catch {
    const probe = await runGit(root, ['rev-parse', '--is-inside-work-tree']);
    return probe.exit_code === 0 && probe.stdout.trim() === 'true';
  }
}

export function registerProjectTools(server: McpServer): void {
  server.registerTool(
    'fs_project_snapshot',
    {
      title: 'Project snapshot',
      description:
        'Return a bounded project tree, common configuration files, and optional Git status in one MCP call.',
      inputSchema: z.object({
        root: z.string(),
        max_depth: z.number().int().min(0).max(10).default(3),
        max_entries: z.number().int().min(1).max(10_000).default(500),
        exclude_directories: z.array(z.string()).default(DEFAULT_EXCLUDED_DIRECTORIES),
        key_files: z.array(z.string()).max(20).default(DEFAULT_KEY_FILES),
        max_file_bytes: z.number().int().min(1_024).max(256 * 1024).default(64 * 1024),
        max_total_bytes: z.number().int().min(1_024).max(1024 * 1024).default(256 * 1024),
        include_git: z.boolean().default(true),
        git_log_count: z.number().int().min(1).max(50).default(5),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({
      root,
      max_depth,
      max_entries,
      exclude_directories,
      key_files,
      max_file_bytes,
      max_total_bytes,
      include_git,
      git_log_count,
    }) => {
      try {
        requirePermission('filesystemRead', 'Filesystem read tools');
        const resolvedRoot = await assertExistingPath(root);
        const stat = await fs.stat(resolvedRoot);
        if (!stat.isDirectory()) throw new Error(`Project root is not a directory: ${resolvedRoot}`);

        const [tree, files] = await Promise.all([
          collectProjectTree(resolvedRoot, max_depth, max_entries, new Set(exclude_directories)),
          readKeyFiles(resolvedRoot, key_files, max_file_bytes, max_total_bytes),
        ]);

        let git: Record<string, unknown> | null = null;
        if (include_git) {
          if (!getRuntimeSettings().permissions.gitRead) {
            git = { available: false, error: 'Git read tools are disabled.' };
          } else if (!(await isGitRepository(resolvedRoot))) {
            git = { available: false, error: 'Project path is not inside a Git working tree.' };
          } else {
            const [status, diffStat, log] = await Promise.all([
              runGit(resolvedRoot, ['status', '--porcelain=v1', '--branch']),
              runGit(resolvedRoot, ['diff', '--stat']),
              runGit(resolvedRoot, [
                'log',
                `--max-count=${git_log_count}`,
                '--date=iso-strict',
                '--pretty=format:%H%x09%an%x09%ad%x09%s',
              ]),
            ]);
            git = { available: status.exit_code === 0, status, diff_stat: diffStat, log };
          }
        }

        return textResult({
          root: resolvedRoot,
          tree: {
            entry_count: tree.entries.length,
            reached_entry_limit: tree.reachedEntryLimit,
            entries: tree.entries,
          },
          key_files: files,
          git,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );
}
