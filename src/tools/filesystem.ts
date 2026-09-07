import fs from 'node:fs/promises';
import path from 'node:path';
import { safeMove } from '../utils/safe-move.js';
import { spawn } from 'node:child_process';
import type { McpServer } from '@modelcontextprotocol/server';
import * as z from 'zod/v4';
import { config } from '../config.js';
import { requirePermission } from '../settings.js';
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

type DirectoryListing = {
  entries: Array<Record<string, unknown>>;
  reachedEntryLimit: boolean;
};

async function directoryEntries(
  root: string,
  recursive: boolean,
  maxDepth: number,
  excludedDirectories: Set<string>,
  maxEntries: number,
  includeMetadata: boolean,
  offset: number,
): Promise<DirectoryListing> {
  const output: Array<Record<string, unknown>> = [];
  let reachedEntryLimit = false;
  let visitedEntries = 0;

  async function visit(current: string, depth: number): Promise<void> {
    if (output.length >= maxEntries) {
      reachedEntryLimit = true;
      return;
    }

    const entries = await fs.readdir(current, { withFileTypes: true });
    entries.sort((a, b) => a.name.localeCompare(b.name));

    const metadataConcurrency = 32;
    for (let batchStart = 0; batchStart < entries.length; batchStart += metadataConcurrency) {
      if (output.length >= maxEntries) {
        reachedEntryLimit = true;
        break;
      }
      const batch = entries.slice(batchStart, batchStart + metadataConcurrency);
      const metadata = includeMetadata
        ? await Promise.all(batch.map((entry) => fs.lstat(path.join(current, entry.name))))
        : [];

      for (let batchIndex = 0; batchIndex < batch.length; batchIndex += 1) {
        if (output.length >= maxEntries) {
          reachedEntryLimit = true;
          break;
        }
        const entry = batch[batchIndex];
        if (!entry) continue;
        const fullPath = path.join(current, entry.name);
        const relative = path.relative(root, fullPath) || '.';
        const type = entry.isDirectory()
          ? 'directory'
          : entry.isFile()
            ? 'file'
            : entry.isSymbolicLink()
              ? 'symlink'
              : 'other';
        const item: Record<string, unknown> = { path: relative, type };

        const stat = metadata[batchIndex];
        if (stat) {
          item.size = stat.size;
          item.modified_at = stat.mtime.toISOString();
        }
        if (entry.isDirectory() && excludedDirectories.has(entry.name)) item.excluded = true;
        if (visitedEntries >= offset) output.push(item);
        visitedEntries += 1;

        if (
          recursive &&
          entry.isDirectory() &&
          !excludedDirectories.has(entry.name) &&
          depth < maxDepth
        ) {
          await assertPathAllowed(fullPath);
          await visit(fullPath, depth + 1);
        }
      }
    }
  }

  await visit(root, 0);
  return { entries: output, reachedEntryLimit };
}

type SearchWalkState = {
  scannedFiles: number;
  reachedFileScanLimit: boolean;
};

async function* walkSearchFiles(
  current: string,
  extensions: Set<string> | null,
  excludedDirectories: Set<string>,
  state: SearchWalkState,
): AsyncGenerator<string> {
  if (state.scannedFiles >= config.maxSearchFiles) {
    state.reachedFileScanLimit = true;
    return;
  }

  const entries = await fs.readdir(current, { withFileTypes: true });
  entries.sort((a, b) => a.name.localeCompare(b.name));

  for (const entry of entries) {
    if (state.scannedFiles >= config.maxSearchFiles) {
      state.reachedFileScanLimit = true;
      return;
    }

    const fullPath = path.join(current, entry.name);
    if (entry.isDirectory()) {
      if (excludedDirectories.has(entry.name)) continue;
      await assertPathAllowed(fullPath);
      yield* walkSearchFiles(fullPath, extensions, excludedDirectories, state);
      continue;
    }

    if (!entry.isFile()) continue;
    if (extensions && !extensions.has(path.extname(entry.name).toLowerCase())) continue;
    state.scannedFiles += 1;
    yield fullPath;
  }
}

let ripgrepAvailable: boolean | null = null;

async function searchWithRipgrep(options: {
  root: string;
  query: string;
  regex: boolean;
  caseSensitive: boolean;
  extensions?: string[];
  excludedDirectories: string[];
  maxResults: number;
}): Promise<Record<string, unknown> | null> {
  if (ripgrepAvailable === false) return null;
  const executable = process.env.CHATGPTX_RG_PATH?.trim() || 'rg';
  const args = [
    '--json',
    '--hidden',
    '--no-ignore',
    '--color=never',
    '--max-filesize',
    String(config.maxFileBytes),
  ];
  if (!options.regex) args.push('--fixed-strings');
  if (!options.caseSensitive) args.push('--ignore-case');
  for (const directory of options.excludedDirectories) {
    args.push('--glob', `!**/${directory}/**`);
  }
  for (const extension of options.extensions ?? []) {
    const normalized = extension.startsWith('.') ? extension.slice(1) : extension;
    if (normalized) args.push('--glob', `*.${normalized}`);
  }
  args.push('--', options.query, '.');

  return await new Promise((resolve, reject) => {
    const child = spawn(executable, args, {
      cwd: options.root,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let pending = '';
    let stderr = '';
    let scannedFiles = 0;
    let stoppedAfterResultLimit = false;
    let spawnFailed = false;
    const results: Array<Record<string, unknown>> = [];

    const consume = (line: string) => {
      if (!line || stoppedAfterResultLimit) return;
      try {
        const event = JSON.parse(line) as {
          type?: string;
          data?: {
            path?: { text?: string };
            lines?: { text?: string };
            line_number?: number;
            submatches?: Array<{ start?: number }>;
          };
        };
        if (event.type === 'begin') scannedFiles += 1;
        if (event.type !== 'match' || !event.data) return;
        const pathText = event.data.path?.text ?? '';
        const text = (event.data.lines?.text ?? '').replace(/\r?\n$/, '');
        const byteOffset = event.data.submatches?.[0]?.start ?? 0;
        const column = Buffer.from(text).subarray(0, byteOffset).toString('utf8').length + 1;
        results.push({
          path: path.relative(options.root, path.resolve(options.root, pathText)),
          line: event.data.line_number ?? 1,
          column,
          text: text.length > 4_000 ? `${text.slice(0, 4_000)}…` : text,
        });
        if (results.length >= options.maxResults) {
          stoppedAfterResultLimit = true;
          child.kill();
        }
      } catch {
        // Ignore a partial or future ripgrep JSON event and continue parsing.
      }
    };

    child.stdout.on('data', (chunk: Buffer) => {
      pending += chunk.toString('utf8');
      const lines = pending.split(/\r?\n/);
      pending = lines.pop() ?? '';
      for (const line of lines) {
        consume(line);
        if (stoppedAfterResultLimit) break;
      }
    });
    child.stderr.on('data', (chunk: Buffer) => {
      stderr += chunk.toString('utf8');
      if (stderr.length > 20_000) stderr = stderr.slice(-20_000);
    });
    child.once('error', (error) => {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
        spawnFailed = true;
        ripgrepAvailable = false;
        resolve(null);
      } else {
        reject(error);
      }
    });
    child.once('close', (code) => {
      if (spawnFailed) return;
      if (pending) consume(pending);
      ripgrepAvailable = true;
      if (!stoppedAfterResultLimit && code !== 0 && code !== 1) {
        if (options.regex && code === 2) {
          resolve(null);
          return;
        }
        reject(new Error(`ripgrep failed with exit code ${code}: ${stderr.trim()}`));
        return;
      }
      resolve({
        search_engine: 'ripgrep',
        scanned_files: scannedFiles,
        reached_file_scan_limit: false,
        stopped_after_result_limit: stoppedAfterResultLimit,
        skipped_large: 0,
        skipped_binary: 0,
        skipped_unreadable: 0,
        result_count: results.length,
        reached_result_limit: results.length >= options.maxResults,
        results,
      });
    });
  });
}

function isProbablyBinary(buffer: Buffer): boolean {
  const sample = buffer.subarray(0, Math.min(buffer.length, 8192));
  return sample.includes(0);
}

const DEFAULT_READ_RESPONSE_BYTES = 256 * 1024;

type FileReadRequest = {
  path: string;
  encoding: 'utf8' | 'base64';
  offset: number;
  length?: number;
  max_response_bytes: number;
  start_line?: number;
  end_line?: number;
};

async function readFilePayload(
  request: FileReadRequest,
  reserveBytes?: (bytes: number) => void,
): Promise<Record<string, unknown>> {
  const resolved = await assertExistingPath(request.path);
  const stat = await fs.stat(resolved);
  if (!stat.isFile()) throw new Error(`Not a file: ${resolved}`);

  const remainingBytes = Math.max(0, stat.size - request.offset);
  const lineSlicing = request.start_line !== undefined || request.end_line !== undefined;
  const desiredLength = request.length ?? remainingBytes;
  const requestedLength = lineSlicing
    ? desiredLength
    : Math.min(desiredLength, request.max_response_bytes);
  if (requestedLength > config.maxFileBytes) {
    throw new Error(
      `Requested read is ${requestedLength} bytes; limit is ${config.maxFileBytes}. Use offset/length to read the file in chunks.`,
    );
  }
  reserveBytes?.(requestedLength);

  const buffer = await readChunk(
    resolved,
    request.offset,
    Math.min(requestedLength, Math.max(0, stat.size - request.offset)),
  );
  let content = decodeBuffer(buffer, request.encoding);
  let lineRange: { start: number; end: number } | null = null;

  if (request.start_line !== undefined || request.end_line !== undefined) {
    if (request.encoding !== 'utf8') throw new Error('start_line/end_line are only valid with utf8 encoding.');
    if (request.offset !== 0 || request.length !== undefined) {
      throw new Error('Line slicing cannot be combined with byte offset/length.');
    }
    const lines = content.split(/\r?\n/);
    const start = request.start_line ?? 1;
    const end = request.end_line ?? lines.length;
    if (end < start) throw new Error('end_line must be greater than or equal to start_line.');
    content = lines.slice(start - 1, end).join('\n');
    lineRange = { start, end: Math.min(end, lines.length) };
  }

  return {
    path: resolved,
    encoding: request.encoding,
    file_size: stat.size,
    offset: request.offset,
    bytes_read: buffer.length,
    truncated: !lineSlicing && request.offset + buffer.length < stat.size,
    next_offset: !lineSlicing && request.offset + buffer.length < stat.size
      ? request.offset + buffer.length
      : null,
    line_range: lineRange,
    content,
  };
}

export function registerFilesystemTools(server: McpServer): void {
  server.registerTool(
    'fs_list',
    {
      title: 'List directory',
      description:
        'List files and directories inside an allowed local path with exclusions, metadata control, and a bounded result size.',
      inputSchema: z.object({
        path: z.string().describe('Directory path. Relative paths are resolved from the MCP server working directory.'),
        recursive: z.boolean().default(false),
        max_depth: z.number().int().min(0).max(20).default(3),
        exclude_directories: z
          .array(z.string())
          .default(['.git', 'node_modules', 'dist', 'build', '.next', '.cache', 'target', 'vendor']),
        max_entries: z.number().int().min(1).max(100_000).default(2_000),
        offset: z.number().int().min(0).max(10_000_000).default(0),
        include_metadata: z.boolean().default(true),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ path: inputPath, recursive, max_depth, exclude_directories, max_entries, offset, include_metadata }) => {
      try {
        requirePermission('filesystemRead', 'Filesystem read tools');
        const resolved = await assertExistingPath(inputPath);
        const stat = await fs.lstat(resolved);
        if (!stat.isDirectory()) throw new Error(`Not a directory: ${resolved}`);
        const listing = await directoryEntries(
          resolved,
          recursive,
          max_depth,
          new Set(exclude_directories),
          max_entries,
          include_metadata,
          offset,
        );
        return textResult({
          root: resolved,
          entry_count: listing.entries.length,
          offset,
          reached_entry_limit: listing.reachedEntryLimit,
          next_offset: listing.reachedEntryLimit ? offset + listing.entries.length : null,
          entries: listing.entries,
        });
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
        requirePermission('filesystemRead', 'Filesystem read tools');
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
        max_response_bytes: z.number().int().min(1_024).max(config.maxFileBytes).default(DEFAULT_READ_RESPONSE_BYTES),
        start_line: z.number().int().positive().optional(),
        end_line: z.number().int().positive().optional(),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ path: inputPath, encoding, offset, length, max_response_bytes, start_line, end_line }) => {
      try {
        requirePermission('filesystemRead', 'Filesystem read tools');
        return textResult(
          await readFilePayload({ path: inputPath, encoding, offset, length, max_response_bytes, start_line, end_line }),
        );
      } catch (error) {
        return errorResult(error);
      }
    },
  );

  server.registerTool(
    'fs_read_many',
    {
      title: 'Read multiple files',
      description:
        'Read up to 100 allowed local files concurrently in one MCP call. Individual failures are returned beside successful reads.',
      inputSchema: z.object({
        files: z
          .array(
            z.object({
              path: z.string(),
              encoding: z.enum(['utf8', 'base64']).default('utf8'),
              offset: z.number().int().min(0).default(0),
              length: z.number().int().positive().optional(),
              max_response_bytes: z.number().int().min(1_024).max(config.maxFileBytes).default(DEFAULT_READ_RESPONSE_BYTES),
              start_line: z.number().int().positive().optional(),
              end_line: z.number().int().positive().optional(),
            }),
          )
          .min(1)
          .max(100),
        concurrency: z.number().int().min(1).max(32).default(8),
        max_total_bytes: z
          .number()
          .int()
          .min(1_024)
          .max(64 * 1024 * 1024)
          .default(Math.min(config.maxFileBytes, 64 * 1024 * 1024)),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ files, concurrency, max_total_bytes }) => {
      try {
        requirePermission('filesystemRead', 'Filesystem read tools');
        const results = new Array<Record<string, unknown>>(files.length);
        let nextIndex = 0;
        let reservedBytes = 0;
        const reserveBytes = (bytes: number) => {
          if (reservedBytes + bytes > max_total_bytes) {
            throw new Error(`Batch read would exceed max_total_bytes (${max_total_bytes}).`);
          }
          reservedBytes += bytes;
        };

        async function worker(): Promise<void> {
          for (;;) {
            const index = nextIndex++;
            if (index >= files.length) return;
            const request = files[index];
            if (!request) return;
            try {
              results[index] = { ok: true, ...(await readFilePayload(request, reserveBytes)) };
            } catch (error) {
              results[index] = {
                ok: false,
                path: request.path,
                error: error instanceof Error ? error.message : String(error),
              };
            }
          }
        }

        await Promise.all(
          Array.from({ length: Math.min(concurrency, files.length) }, () => worker()),
        );
        return textResult({
          requested: files.length,
          succeeded: results.filter((result) => result.ok === true).length,
          failed: results.filter((result) => result.ok === false).length,
          total_bytes_read: results.reduce(
            (total, result) => total + (typeof result.bytes_read === 'number' ? result.bytes_read : 0),
            0,
          ),
          results,
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
        requirePermission('filesystemWrite', 'Filesystem write tools');
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
        requirePermission('filesystemWrite', 'Filesystem write tools');
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
        requirePermission('filesystemWrite', 'Filesystem write tools');
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
        requirePermission('filesystemWrite', 'Filesystem write tools');
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
        requirePermission('filesystemWrite', 'Filesystem write tools');
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
        requirePermission('filesystemWrite', 'Filesystem write tools');
        const src = await assertExistingPath(source);
        const dst = await assertPathAllowed(destination);
        const result = await safeMove(src, dst, overwrite, create_parents);
        return textResult({ source: src, destination: dst, ...result });
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
        requirePermission('filesystemWrite', 'Filesystem write tools');
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
        prefer_ripgrep: z.boolean().default(true),
      }),
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    },
    async ({ root, query, regex, case_sensitive, extensions, exclude_directories, max_results, prefer_ripgrep }) => {
      try {
        requirePermission('filesystemRead', 'Filesystem read tools');
        const resolvedRoot = await assertExistingPath(root);
        const stat = await fs.stat(resolvedRoot);
        if (!stat.isDirectory()) throw new Error(`Not a directory: ${resolvedRoot}`);

        if (prefer_ripgrep) {
          const fastResult = await searchWithRipgrep({
            root: resolvedRoot,
            query,
            regex,
            caseSensitive: case_sensitive,
            extensions,
            excludedDirectories: exclude_directories,
            maxResults: max_results,
          });
          if (fastResult) return textResult({ root: resolvedRoot, ...fastResult });
        }

        const normalizedExtensions =
          extensions && extensions.length > 0
            ? new Set(extensions.map((ext) => (ext.startsWith('.') ? ext.toLowerCase() : `.${ext.toLowerCase()}`)))
            : null;
        const walkState: SearchWalkState = { scannedFiles: 0, reachedFileScanLimit: false };
        const iterator = walkSearchFiles(
          resolvedRoot,
          normalizedExtensions,
          new Set(exclude_directories),
          walkState,
        )[Symbol.asyncIterator]();
        const flags = case_sensitive ? 'g' : 'gi';
        if (regex) void new RegExp(query, flags);
        const literalNeedle = case_sensitive ? query : query.toLowerCase();
        const results: Array<Record<string, unknown>> = [];
        let skippedLarge = 0;
        let skippedBinary = 0;
        let skippedUnreadable = 0;
        let traversalDone = false;

        while (!traversalDone && results.length < max_results) {
          const batch: string[] = [];
          for (let index = 0; index < 12; index += 1) {
            const next = await iterator.next();
            if (next.done) {
              traversalDone = true;
              break;
            }
            batch.push(next.value);
          }

          const searched = await Promise.all(
            batch.map(async (filePath) => {
              try {
                const fileStat = await fs.stat(filePath);
                if (fileStat.size > config.maxFileBytes) return { skipped: 'large' as const, matches: [] };
                const buffer = await fs.readFile(filePath);
                if (isProbablyBinary(buffer)) return { skipped: 'binary' as const, matches: [] };

                const matcher = regex ? new RegExp(query, flags) : null;
                const matches: Array<Record<string, unknown>> = [];
                const lines = buffer.toString('utf8').split(/\r?\n/);
                for (let index = 0; index < lines.length; index += 1) {
                  const line = lines[index] ?? '';
                  let column = -1;
                  if (matcher) {
                    matcher.lastIndex = 0;
                    const match = matcher.exec(line);
                    if (!match) continue;
                    column = match.index ?? 0;
                  } else {
                    const comparableLine = case_sensitive ? line : line.toLowerCase();
                    column = comparableLine.indexOf(literalNeedle);
                    if (column === -1) continue;
                  }
                  matches.push({
                    path: path.relative(resolvedRoot, filePath),
                    line: index + 1,
                    column: column + 1,
                    text: line.length > 4_000 ? `${line.slice(0, 4_000)}…` : line,
                  });
                  if (matches.length >= max_results) break;
                }
                return { skipped: null, matches };
              } catch {
                return { skipped: 'unreadable' as const, matches: [] };
              }
            }),
          );

          for (const file of searched) {
            if (file.skipped === 'large') skippedLarge += 1;
            else if (file.skipped === 'binary') skippedBinary += 1;
            else if (file.skipped === 'unreadable') skippedUnreadable += 1;
            for (const match of file.matches) {
              if (results.length >= max_results) break;
              results.push(match);
            }
          }
        }

        if (!traversalDone) await iterator.return?.(undefined);
        return textResult({
          root: resolvedRoot,
          search_engine: 'javascript',
          scanned_files: walkState.scannedFiles,
          reached_file_scan_limit: walkState.reachedFileScanLimit,
          stopped_after_result_limit: !traversalDone && results.length >= max_results,
          skipped_large: skippedLarge,
          skipped_binary: skippedBinary,
          skipped_unreadable: skippedUnreadable,
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
