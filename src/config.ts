import path from 'node:path';

function envBool(name: string, fallback: boolean): boolean {
  const value = process.env[name];
  if (value === undefined || value.trim() === '') return fallback;
  return ['1', 'true', 'yes', 'on'].includes(value.trim().toLowerCase());
}

function envInt(name: string, fallback: number, min: number, max: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const value = Number.parseInt(raw, 10);
  if (!Number.isFinite(value)) return fallback;
  return Math.min(max, Math.max(min, value));
}

function parseRoots(): string[] {
  const raw = process.env.CHATGPTX_ROOTS?.trim();
  if (!raw) return [path.resolve(process.cwd())];
  return raw
    .split(path.delimiter)
    .map((item) => item.trim())
    .filter(Boolean)
    .map((item) => path.resolve(item));
}

export const config = Object.freeze({
  host: process.env.CHATGPTX_HOST?.trim() || '127.0.0.1',
  port: envInt('CHATGPTX_PORT', 3210, 1, 65535),
  roots: parseRoots(),
  settingsDir: path.resolve(process.env.CHATGPTX_SETTINGS_DIR?.trim() || path.join(process.cwd(), '.chatgptx')),
  fullAccess: envBool('CHATGPTX_FULL_ACCESS', false),
  enableShell: envBool('CHATGPTX_ENABLE_SHELL', false),
  authToken: process.env.CHATGPTX_AUTH_TOKEN?.trim() || null,
  maxFileBytes: envInt('CHATGPTX_MAX_FILE_BYTES', 10 * 1024 * 1024, 1024, 256 * 1024 * 1024),
  maxCommandOutputChars: envInt('CHATGPTX_MAX_COMMAND_OUTPUT_CHARS', 200_000, 1_000, 5_000_000),
  maxProcessBufferChars: envInt('CHATGPTX_MAX_PROCESS_BUFFER_CHARS', 1_000_000, 10_000, 20_000_000),
  defaultCommandTimeoutMs: envInt('CHATGPTX_DEFAULT_COMMAND_TIMEOUT_MS', 120_000, 1_000, 60 * 60 * 1000),
  maxSearchFiles: envInt('CHATGPTX_MAX_SEARCH_FILES', 10_000, 100, 1_000_000),
});

export type AppConfig = typeof config;
