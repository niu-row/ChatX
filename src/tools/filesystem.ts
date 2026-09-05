import fs from 'node:fs/promises';
import path from 'node:path';
import type { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { config } from '../config.js';
import { assertExistingPath, assertPathAllowed } from '../security/path-policy.js';
import { errorResult, textResult } from '../utils/results.js';

function countOccurrences(haystack: string, needle: string): number {
  if (!needle) return 0;
  let count = 0;
  let position = 0;
  while ((position = haystack.indexOf(needle, position)) !== -1) {
    count += 1;
    position += needle.length;
  }
  return count;
}

async function readChunk(filePath: string, offset: number, length: number): Promise<Buffer> {
  const handle = await fs.open(filePath, 'r');
  try {
    const buffer = Buffer.alloc(length);
    const { bytesRead } = await handle.read(buffer, 0, length, offset);
    return buffer.subarray(0, bytesRead);
  } finally {
    await handle.close();
  }
}

function decodeBuffer(buffer: Buffer, encoding: 'utf8' | 'base64'): string {
  return encoding === 'base64' ? buffer.toString('base64') : buffer.toString('utf8');
}

async function copyWithFallback(source: string, destination: string, recursive: boolean): Promise<void> {
  await fs.cp(source, destination, { recursive, force: true, errorOnExist: false });
}

async function moveWithFallback(source: string, destination: string): Promise<void> {
  try {
    await fs.rename(source, destination);
  } catch (error) {
    const code = error && typeof error === 'object' && 'code' in error ? String(error.code) : '';
    if (code !== 'EXDEV') throw error;
    const stat = await fs.lstat(source);
    await copyWithFallback(source, destination, stat.isDirectory());
    await fs.rm(source, { recursive: stat.isDirectory(), force: false });
  }
}

async function directoryEntries(
  root: string,
  recursive: boolean,
  maxDepth: number,
): Promise<Array<Record<string, unknown>>> {
  const output: Array<Record<string, unknown>> = [];

  async function visit(current: string, depth: number): Promise<void> {
    const entries = await fs.readdir(current, { withFileTypes: true });
    entries.sort((a, b) => a.name.localeCompare(b.name));

    for (const entry of entries) {
      const fullPath = path.join(current, entry.name);
      const relative = path.relative(root, fullPath) || '.';
      const stat = await fs.lstat(fullPath);
      const type = entry.isDirectory()
        ? 'directory'
        : entry.isFile()
          ? 'file'
          : entry.isSymbolicLink()
            ? 'symlink'
            : 'other';

      output.push({
        path: relative,
        type,
        size: stat.size,
        modified_at: stat.mtime.toISOString(),
      });

      if (recursive && entry.isDirectory() && depth < maxDepth) {
        await assertPathAllowed(fullPath);
        await visit(fullPath, depth + 1);
      }
    }
  }

  await visit(root, 0);
  return output;
}

async function collectSearchFiles(
  root: string,
  extensions: string[] | undefined,
  excludedDirectories: Set<string>,
): Promise<string[]> {
  const files: string[] = [];
  const normalizedExtensions = extensions?.map((ext) => (ext.startsWith('.') ? ext.toLowerCase() : `.${ext.toLowerCase()}`));

  async function visit(current: string): Promise<void> {
    if (files.length >= config.maxSearchFiles) return;
    const entries = await fs.readdir(current, { withFileTypes: true });

    for (const entry of entries) {
      if (files.length >= config.maxSearchFiles) break;
      const fullPath = path.join(current, entry.name);

      if (entry.isDirectory()) {
        if (excludedDirectories.has(entry.name)) continue;
        await assertPathAllowed(fullPath);
        await visit(fullPath);
        continue;
      }

      if (!entry.isFile()) continue;
      if (normalizedExtensions && normalizedExtensions.length > 0) {
        if (!normalizedExtensions.includes(path.extname(entry.name).toLowerCase())) continue;
      }
      files.push(fullPath);
    }
  }

  await visit(root);
  return files;
}

function isProbablyBinary(buffer: Buffer): boolean {
  const sample = buffer.subarray(0, Math.min(buffer.length, 8192));
  return sample.includes(0);
}

export function registerFilesystemTools(server: McpServer): void {
  server.registerTool(
    'fs_list',
    {
      title: 'List directory',
      description: 'List files and directories inside an allowed local path. Can recurse to a bounded depth.',
      inputSchema: z.object({
        path: z.string().describe('Directory path. Relative paths are resolved from the MCP server working directory.'),
        recursive: z.boolean().default(false),
        max_depth: z.number().int().min(0).max(20).default(3),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ path: inputPath, recursive, max_depth }) => {
      try {
        const resolved = await assertExistingPath(inputPath);
        const stat = await fs.lstat(resolved);
        if (!stat.isDirectory()) throw new Error(`Not a directory: ${resolved}`);
        const entries = await directoryEntries(resolved, recursive, max_depth);
        return textResult({ root: resolved, entries });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_stat',
    {
      title: 'Inspect path',
      description: 'Return metadata for a local file, directory, or symbolic link.',
      inputSchema: z.object({ path: z.string() }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ path: inputPath }) => {
      try {
        const resolved = await assertExistingPath(inputPath);
        const stat = await fs.lstat(resolved);
        let symlink_target: string | null = null;
        if (stat.isSymbolicLink()) symlink_target = await fs.readlink(resolved);
        return textResult({
          path: resolved,
          type: stat.isDirectory() ? 'directory' : stat.isFile() ? 'file' : stat.isSymbolicLink() ? 'symlink' : 'other',
          size: stat.size,
          mode: stat.mode,
          created_at: stat.birthtime.toISOString(),
          modified_at: stat.mtime.toISOString(),
          accessed_at: stat.atime.toISOString(),
          symlink_target,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_read',
    {
      title: 'Read file',
      description:
        'Read a local file as UTF-8 or base64. Supports byte ranges and optional 1-based line slicing for UTF-8 text.',
      inputSchema: z.object({
        path: z.string(),
        encoding: z.enum(['utf8', 'base64']).default('utf8'),
        offset: z.number().int().min(0).default(0),
        length: z.number().int().positive().optional(),
        start_line: z.number().int().positive().optional(),
        end_line: z.number().int().positive().optional(),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ path: inputPath, encoding, offset, length, start_line, end_line }) => {
      try {
        const resolved = await assertExistingPath(inputPath);
        const stat = await fs.stat(resolved);
        if (!stat.isFile()) throw new Error(`Not a file: ${resolved}`);

        const requestedLength = length ?? Math.max(0, stat.size - offset);
        if (requestedLength > config.maxFileBytes) {
          throw new Error(
            `Requested read is ${requestedLength} bytes; limit is ${config.maxFileBytes}. Use offset/length to read the file in chunks.`,
          );
        }

        const buffer = await readChunk(resolved, offset, Math.min(requestedLength, Math.max(0, stat.size - offset)));
        let content = decodeBuffer(buffer, encoding);
        let line_range: { start: number; end: number } | null = null;

        if (start_line !== undefined || end_line !== undefined) {
          if (encoding !== 'utf8') throw new Error('start_line/end_line are only valid with utf8 encoding.');
          if (offset !== 0 || length !== undefined) {
            throw new Error('Line slicing cannot be combined with byte offset/length.');
          }
          const lines = content.split(/\r?\n/);
          const start = start_line ?? 1;
          const end = end_line ?? lines.length;
          if (end < start) throw new Error('end_line must be greater than or equal to start_line.');
          content = lines.slice(start - 1, end).join('\n');
          line_range = { start, end: Math.min(end, lines.length) };
        }

        return textResult({
          path: resolved,
          encoding,
          file_size: stat.size,
          offset,
          bytes_read: buffer.length,
          line_range,
          content,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_write',
    {
      title: 'Write file',
      description: 'Create or replace a local file. Parent directories can be created automatically.',
      inputSchema: z.object({
        path: z.string(),
        content: z.string(),
        encoding: z.enum(['utf8', 'base64']).default('utf8'),
        overwrite: z.boolean().default(true),
        create_parents: z.boolean().default(true),
      }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: true, openWorldHint: false },
    },
    async ({ path: inputPath, content, encoding, overwrite, create_parents }) => {
      try {
        const resolved = await assertPathAllowed(inputPath);
        if (create_parents) await fs.mkdir(path.dirname(resolved), { recursive: true });
        const data = encoding === 'base64' ? Buffer.from(content, 'base64') : content;
        await fs.writeFile(resolved, data, { flag: overwrite ? 'w' : 'wx' });
        const stat = await fs.stat(resolved);
        return textResult({ path: resolved, bytes_written: stat.size });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_append',
    {
      title: 'Append file',
      description: 'Append UTF-8 or base64 content to a local file, creating it if necessary.',
      inputSchema: z.object({
        path: z.string(),
        content: z.string(),
        encoding: z.enum(['utf8', 'base64']).default('utf8'),
        create_parents: z.boolean().default(true),
      }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
    },
    async ({ path: inputPath, content, encoding, create_parents }) => {
      try {
        const resolved = await assertPathAllowed(inputPath);
        if (create_parents) await fs.mkdir(path.dirname(resolved), { recursive: true });
        const data = encoding === 'base64' ? Buffer.from(content, 'base64') : content;
        await fs.appendFile(resolved, data);
        const stat = await fs.stat(resolved);
        return textResult({ path: resolved, size: stat.size });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_edit',
    {
      title: 'Edit file by exact replacement',
      description:
        'Modify a UTF-8 file by replacing an exact text fragment. By default exactly one occurrence is replaced; set replace_all to replace every occurrence.',
      inputSchema: z.object({
        path: z.string(),
        old_text: z.string().min(1),
        new_text: z.string(),
        replace_all: z.boolean().default(false),
        expected_replacements: z.number().int().min(0).optional(),
      }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
    },
    async ({ path: inputPath, old_text, new_text, replace_all, expected_replacements }) => {
      try {
        const resolved = await assertExistingPath(inputPath);
        const stat = await fs.stat(resolved);
        if (!stat.isFile()) throw new Error(`Not a file: ${resolved}`);
        if (stat.size > config.maxFileBytes) throw new Error(`File exceeds edit limit of ${config.maxFileBytes} bytes.`);

        const original = await fs.readFile(resolved, 'utf8');
        const occurrences = countOccurrences(original, old_text);
        if (occurrences === 0) throw new Error('old_text was not found; file was not changed.');
        if (!replace_all && occurrences > 1 && expected_replacements === undefined) {
          throw new Error(`old_text occurs ${occurrences} times. Provide a more specific old_text or set replace_all=true.`);
        }

        const replacements = replace_all ? occurrences : 1;
        if (expected_replacements !== undefined && replacements !== expected_replacements) {
          throw new Error(
            `Replacement count mismatch: would replace ${replacements}, expected ${expected_replacements}. File was not changed.`,
          );
        }

        const updated = replace_all ? original.split(old_text).join(new_text) : original.replace(old_text, new_text);
        await fs.writeFile(resolved, updated, 'utf8');
        return textResult({ path: resolved, replacements, bytes_before: stat.size, bytes_after: Buffer.byteLength(updated) });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_mkdir',
    {
      title: 'Create directory',
      description: 'Create a local directory, optionally including missing parent directories.',
      inputSchema: z.object({ path: z.string(), recursive: z.boolean().default(true) }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ path: inputPath, recursive }) => {
      try {
        const resolved = await assertPathAllowed(inputPath);
        await fs.mkdir(resolved, { recursive });
        return textResult({ path: resolved, created: true });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_delete',
    {
      title: 'Delete path',
      description: 'Delete a local file or directory. Recursive deletion must be explicitly requested for non-empty directories.',
      inputSchema: z.object({
        path: z.string(),
        recursive: z.boolean().default(false),
        force: z.boolean().default(false),
      }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: true, openWorldHint: false },
    },
    async ({ path: inputPath, recursive, force }) => {
      try {
        const resolved = await assertPathAllowed(inputPath);
        await fs.rm(resolved, { recursive, force });
        return textResult({ path: resolved, deleted: true });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_move',
    {
      title: 'Move path',
      description: 'Move or rename a local file or directory. Cross-device moves fall back to copy then delete.',
      inputSchema: z.object({
        source: z.string(),
        destination: z.string(),
        overwrite: z.boolean().default(false),
        create_parents: z.boolean().default(true),
      }),
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
    },
    async ({ source, destination, overwrite, create_parents }) => {
      try {
        const src = await assertExistingPath(source);
        const dst = await assertPathAllowed(destination);
        if (create_parents) await fs.mkdir(path.dirname(dst), { recursive: true });

        try {
          await fs.lstat(dst);
          if (!overwrite) throw new Error(`Destination exists: ${dst}`);
          await fs.rm(dst, { recursive: true, force: true });
        } catch (error) {
          const code = error && typeof error === 'object' && 'code' in error ? String(error.code) : '';
          if (code !== 'ENOENT' && !(error instanceof Error && error.message.startsWith('Destination exists:'))) throw error;
          if (error instanceof Error && error.message.startsWith('Destination exists:')) throw error;
        }

        await moveWithFallback(src, dst);
        return textResult({ source: src, destination: dst, moved: true });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_copy',
    {
      title: 'Copy path',
      description: 'Copy a local file or directory.',
      inputSchema: z.object({
        source: z.string(),
        destination: z.string(),
        recursive: z.boolean().default(true),
        overwrite: z.boolean().default(false),
        create_parents: z.boolean().default(true),
      }),
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ source, destination, recursive, overwrite, create_parents }) => {
      try {
        const src = await assertExistingPath(source);
        const dst = await assertPathAllowed(destination);
        if (create_parents) await fs.mkdir(path.dirname(dst), { recursive: true });
        await fs.cp(src, dst, { recursive, force: overwrite, errorOnExist: !overwrite });
        return textResult({ source: src, destination: dst, copied: true });
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_search',
    {
      title: 'Search file contents',
      description:
        'Recursively search text files under a local directory for a literal string or regular expression. Common dependency/build directories are excluded by default.',
      inputSchema: z.object({
        root: z.string(),
        query: z.string().min(1),
        regex: z.boolean().default(false),
        case_sensitive: z.boolean().default(false),
        extensions: z.array(z.string()).optional(),
        exclude_directories: z
          .array(z.string())
          .default(['.git', 'node_modules', 'dist', 'build', '.next', '.cache', 'target', 'vendor']),
        max_results: z.number().int().min(1).max(5000).default(200),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ root, query, regex, case_sensitive, extensions, exclude_directories, max_results }) => {
      try {
        const resolvedRoot = await assertExistingPath(root);
        const stat = await fs.stat(resolvedRoot);
        if (!stat.isDirectory()) throw new Error(`Not a directory: ${resolvedRoot}`);

        const files = await collectSearchFiles(resolvedRoot, extensions, new Set(exclude_directories));
        const flags = case_sensitive ? 'g' : 'gi';
        const matcher = regex ? new RegExp(query, flags) : null;
        const literalNeedle = case_sensitive ? query : query.toLowerCase();
        const results: Array<Record<string, unknown>> = [];
        let skipped_large = 0;
        let skipped_binary = 0;

        for (const filePath of files) {
          if (results.length >= max_results) break;
          const fileStat = await fs.stat(filePath);
          if (fileStat.size > config.maxFileBytes) {
            skipped_large += 1;
            continue;
          }

          const buffer = await fs.readFile(filePath);
          if (isProbablyBinary(buffer)) {
            skipped_binary += 1;
            continue;
          }

          const lines = buffer.toString('utf8').split(/\r?\n/);
          for (let index = 0; index < lines.length && results.length < max_results; index += 1) {
            const line = lines[index] ?? '';
            if (matcher) {
              matcher.lastIndex = 0;
              const match = matcher.exec(line);
              if (!match) continue;
              results.push({
                path: path.relative(resolvedRoot, filePath),
                line: index + 1,
                column: (match.index ?? 0) + 1,
                text: line,
              });
            } else {
              const comparableLine = case_sensitive ? line : line.toLowerCase();
              const column = comparableLine.indexOf(literalNeedle);
              if (column === -1) continue;
              results.push({
                path: path.relative(resolvedRoot, filePath),
                line: index + 1,
                column: column + 1,
                text: line,
              });
            }
          }
        }

        return textResult({
          root: resolvedRoot,
          scanned_files: files.length,
          reached_file_scan_limit: files.length >= config.maxSearchFiles,
          skipped_large,
          skipped_binary,
          result_count: results.length,
          reached_result_limit: results.length >= max_results,
          results,
        });
      } catch (error) {
        return errorResult(error);
      }
    },
  );
}
