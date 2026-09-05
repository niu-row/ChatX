import { spawn } from 'node:child_process';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';

const port = 33000 + Math.floor(Math.random() * 10000);
const baseUrl = `http://127.0.0.1:${port}`;
const tempRoot = await fs.mkdtemp(path.join(os.tmpdir(), 'chatgptx-smoke-'));
const smokeFile = path.join(tempRoot, 'smoke.txt');
let serverLogs = '';

const server = spawn(process.execPath, ['dist/index.js'], {
  cwd: process.cwd(),
  env: {
    ...process.env,
    CHATGPTX_HOST: '127.0.0.1',
    CHATGPTX_PORT: String(port),
    CHATGPTX_ROOTS: [tempRoot, process.cwd()].join(path.delimiter),
    CHATGPTX_FULL_ACCESS: 'false',
    CHATGPTX_ENABLE_SHELL: 'true',
  },
  stdio: ['ignore', 'pipe', 'pipe'],
});

server.stdout.on('data', (chunk) => {
  serverLogs += chunk.toString('utf8');
});
server.stderr.on('data', (chunk) => {
  serverLogs += chunk.toString('utf8');
});

async function waitForHealth() {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (server.exitCode !== null) throw new Error(`Server exited early (${server.exitCode}).\n${serverLogs}`);
    try {
      const response = await fetch(`${baseUrl}/healthz`);
      if (response.ok) return;
    } catch {
      // Server is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`Timed out waiting for server health.\n${serverLogs}`);
}

function textOf(result) {
  return (result.content ?? [])
    .filter((item) => item.type === 'text')
    .map((item) => item.text)
    .join('\n');
}

function jsonOf(result, toolName) {
  if (result.isError) throw new Error(`${toolName} failed: ${textOf(result)}`);
  const text = textOf(result);
  try {
    return JSON.parse(text);
  } catch {
    throw new Error(`${toolName} returned non-JSON text: ${text}`);
  }
}

async function call(client, name, args = {}) {
  const result = await client.callTool({ name, arguments: args });
  return jsonOf(result, name);
}

let client;
try {
  await waitForHealth();

  client = new Client(
    { name: 'chatgptx-smoke', version: '0.1.0' },
    { versionNegotiation: { mode: 'auto' } },
  );
  await client.connect(new StreamableHTTPClientTransport(new URL(`${baseUrl}/mcp`)));

  const listed = await client.listTools();
  const toolNames = new Set(listed.tools.map((tool) => tool.name));
  const expected = [
    'server_info',
    'fs_list',
    'fs_stat',
    'fs_read',
    'fs_write',
    'fs_append',
    'fs_edit',
    'fs_mkdir',
    'fs_delete',
    'fs_move',
    'fs_copy',
    'fs_search',
    'run_command',
    'process_output',
    'process_list',
    'process_stdin',
    'process_terminate',
    'git_status',
    'git_diff',
    'git_log',
    'git_run',
  ];
  const missing = expected.filter((name) => !toolNames.has(name));
  if (missing.length > 0) throw new Error(`Missing tools: ${missing.join(', ')}`);

  const info = await call(client, 'server_info');
  if (info.name !== 'chatgptx') throw new Error(`Unexpected server info: ${JSON.stringify(info)}`);

  await call(client, 'fs_write', { path: smokeFile, content: 'hello\nsecond line\n' });
  await call(client, 'fs_edit', { path: smokeFile, old_text: 'hello', new_text: 'world' });

  const read = await call(client, 'fs_read', { path: smokeFile });
  if (!read.content.includes('world')) throw new Error(`fs_read did not return edited content: ${read.content}`);

  const search = await call(client, 'fs_search', { root: tempRoot, query: 'world' });
  if (search.result_count < 1) throw new Error('fs_search did not find the edited text.');

  const shell = await call(client, 'run_command', {
    command: `node -e "process.stdout.write('shell-ok')"`,
    cwd: tempRoot,
  });
  if (shell.exit_code !== 0 || !shell.stdout.includes('shell-ok')) {
    throw new Error(`run_command failed: ${JSON.stringify(shell)}`);
  }

  const background = await call(client, 'run_command', {
    command: `node -e "setTimeout(() => {}, 30000)"`,
    cwd: tempRoot,
    background: true,
  });
  if (!background.process_id) throw new Error('Background run_command did not return process_id.');
  await call(client, 'process_output', { process_id: background.process_id });
  await call(client, 'process_terminate', { process_id: background.process_id });

  const git = await call(client, 'git_status', { repo: process.cwd() });
  if (git.exit_code !== 0) throw new Error(`git_status failed: ${JSON.stringify(git)}`);

  await call(client, 'fs_delete', { path: smokeFile });

  console.log(`smoke ok: ${toolNames.size} tools over MCP`);
} finally {
  if (client) await client.close().catch(() => {});
  server.kill('SIGTERM');
  await new Promise((resolve) => setTimeout(resolve, 250));
  if (server.exitCode === null) server.kill('SIGKILL');
  await fs.rm(tempRoot, { recursive: true, force: true }).catch(() => {});
}
