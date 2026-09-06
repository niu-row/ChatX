import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';

const port = 33000 + Math.floor(Math.random() * 10000);
const baseUrl = `http://127.0.0.1:${port}`;
const tempRoot = await fs.mkdtemp(path.join(os.tmpdir(), 'chatgptx-smoke-'));
const settingsDir = path.join(tempRoot, '.settings');
const smokeFile = path.join(tempRoot, 'smoke.txt');
const gitRepo = path.join(tempRoot, 'repo');
let serverLogs = '';

const server = spawn(process.execPath, ['dist/index.js'], {
  cwd: process.cwd(),
  env: {
    ...process.env,
    CHATGPTX_HOST: '127.0.0.1',
    CHATGPTX_PORT: String(port),
    CHATGPTX_ROOTS: [tempRoot, process.cwd()].join(path.delimiter),
    CHATGPTX_SETTINGS_DIR: settingsDir,
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
      // still starting
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

async function callRaw(client, name, args = {}) {
  return await client.callTool({ name, arguments: args });
}

async function call(client, name, args = {}) {
  return jsonOf(await callRaw(client, name, args), name);
}

function runGit(args, cwd = gitRepo) {
  const result = spawnSync('git', args, { cwd, encoding: 'utf8', windowsHide: true });
  if (result.status !== 0) throw new Error(`git ${args.join(' ')} failed:\n${result.stderr || result.stdout}`);
  return result.stdout.trim();
}

async function localPost(url, body) {
  return await fetch(`${baseUrl}${url}`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      origin: baseUrl,
      'sec-fetch-site': 'same-origin',
    },
    body: JSON.stringify(body ?? {}),
  });
}

let client;
try {
  await waitForHealth();

  const dashboard = await fetch(`${baseUrl}/`);
  if (!dashboard.ok || !(await dashboard.text()).includes('ChatX 本地控制台')) {
    throw new Error('Dashboard did not load.');
  }

  const tunnelStatusResponse = await fetch(`${baseUrl}/api/tunnel/status`);
  const tunnelStatus = await tunnelStatusResponse.json();
  if (!tunnelStatusResponse.ok || tunnelStatus.status?.service?.name !== 'chatx') {
    throw new Error(`Unexpected dashboard status: ${JSON.stringify(tunnelStatus)}`);
  }
  if (tunnelStatus.status?.settings?.version !== 2) {
    throw new Error(`Expected settings v2, got ${JSON.stringify(tunnelStatus.status?.settings)}`);
  }

  const diagnosticsResponse = await fetch(`${baseUrl}/api/diagnostics`);
  const diagnostics = await diagnosticsResponse.json();
  const localMcpCheck = diagnostics.checks?.find((check) => check.name === '本地 MCP');
  if (!diagnosticsResponse.ok || localMcpCheck?.status !== 'ok') {
    throw new Error(`Diagnostics failed: ${JSON.stringify(diagnostics)}`);
  }

  const crossSiteConnect = await fetch(`${baseUrl}/api/tunnel/connect`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ tunnelId: 'tunnel_12345678', apiKey: 'not-a-real-key' }),
  });
  if (crossSiteConnect.status !== 403) {
    throw new Error(`Dashboard accepted a connect request without a local Origin: ${crossSiteConnect.status}`);
  }

  const invalidLocalConnect = await localPost('/api/tunnel/connect', {
    tunnelId: 'bad',
    apiKey: 'not-a-real-key',
  });
  if (invalidLocalConnect.status !== 400) {
    throw new Error(`Dashboard local input validation returned ${invalidLocalConnect.status}, expected 400.`);
  }

  const nonJson = await fetch(`${baseUrl}/mcp`, {
    method: 'POST',
    headers: { accept: 'application/json, text/event-stream' },
    body: '{}',
  });
  if (nonJson.status !== 415) {
    throw new Error(`Non-JSON MCP POST returned ${nonJson.status}, expected 415.`);
  }
  await new Promise((resolve) => setTimeout(resolve, 50));
  if (serverLogs.includes('[chatx] MCP error: Error: Unsupported Media Type')) {
    throw new Error(`Non-JSON probe reached the MCP SDK error logger.\n${serverLogs}`);
  }

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
    'git_stage',
    'git_unstage',
    'git_create_branch',
    'git_commit',
    'git_run',
  ];
  const missing = expected.filter((name) => !toolNames.has(name));
  if (missing.length > 0) throw new Error(`Missing tools: ${missing.join(', ')}`);

  const info = await call(client, 'server_info');
  if (info.name !== 'chatx') throw new Error(`Unexpected server info: ${JSON.stringify(info)}`);

  const invocationResponse = await fetch(`${baseUrl}/api/invocations`);
  const invocationPayload = await invocationResponse.json();
  if (!invocationResponse.ok || !invocationPayload.entries?.some((entry) => entry.tool === 'server_info' && entry.status === 'ok')) {
    throw new Error(`Invocation log did not record server_info: ${JSON.stringify(invocationPayload)}`);
  }
  if (invocationPayload.entries?.some((entry) => 'arguments' in entry || 'input' in entry)) {
    throw new Error('Invocation log must not record tool arguments.');
  }

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

  await fs.mkdir(gitRepo, { recursive: true });
  runGit(['init']);
  runGit(['config', 'user.name', 'ChatGPTX Smoke']);
  runGit(['config', 'user.email', 'smoke@example.invalid']);
  const tracked = path.join(gitRepo, 'tracked.txt');
  await fs.writeFile(tracked, 'one\n', 'utf8');

  const staged = await call(client, 'git_stage', { repo: gitRepo, paths: ['tracked.txt'] });
  if (staged.exit_code !== 0) throw new Error(`git_stage failed: ${JSON.stringify(staged)}`);
  const committed = await call(client, 'git_commit', { repo: gitRepo, message: 'initial smoke commit' });
  if (committed.exit_code !== 0) throw new Error(`git_commit failed: ${JSON.stringify(committed)}`);

  const log = await call(client, 'git_log', { repo: gitRepo, max_count: 5 });
  if (!log.stdout.includes('initial smoke commit')) throw new Error(`git_log missing commit: ${JSON.stringify(log)}`);

  await fs.writeFile(tracked, 'two\n', 'utf8');
  await call(client, 'git_stage', { repo: gitRepo, paths: ['tracked.txt'] });
  const unstaged = await call(client, 'git_unstage', { repo: gitRepo, paths: ['tracked.txt'] });
  if (unstaged.exit_code !== 0) throw new Error(`git_unstage failed: ${JSON.stringify(unstaged)}`);
  const branch = await call(client, 'git_create_branch', { repo: gitRepo, name: 'smoke-branch' });
  if (branch.exit_code !== 0) throw new Error(`git_create_branch failed: ${JSON.stringify(branch)}`);

  const advanced = await callRaw(client, 'git_run', { repo: gitRepo, args: ['status'] });
  if (!advanced.isError || !textOf(advanced).includes('Advanced Git command execution is disabled')) {
    throw new Error(`git_run should be disabled by default: ${textOf(advanced)}`);
  }

  const presetResponse = await localPost('/api/settings', { preset: 'developer' });
  const presetBody = await presetResponse.json();
  if (!presetResponse.ok || presetBody.status?.policy?.permissionPreset !== 'developer') {
    throw new Error(`Developer preset failed: ${JSON.stringify(presetBody)}`);
  }
  if (presetBody.status.policy.permissions.shell !== false || presetBody.status.policy.permissions.gitAdvanced !== false) {
    throw new Error(`Developer preset permissions are unsafe: ${JSON.stringify(presetBody.status.policy.permissions)}`);
  }

  const rootsResponse = await localPost('/api/settings', { roots: [tempRoot, process.cwd()] });
  const rootsBody = await rootsResponse.json();
  if (!rootsResponse.ok || rootsBody.status?.policy?.roots?.length !== 2) {
    throw new Error(`Dynamic roots update failed: ${JSON.stringify(rootsBody)}`);
  }

  await call(client, 'fs_delete', { path: smokeFile });
  console.log(`smoke ok: ${toolNames.size} tools over MCP`);
} finally {
  if (client) await client.close().catch(() => {});
  server.kill('SIGTERM');
  await new Promise((resolve) => setTimeout(resolve, 300));
  if (server.exitCode === null) server.kill('SIGKILL');
  await fs.rm(tempRoot, { recursive: true, force: true }).catch(() => {});
}
