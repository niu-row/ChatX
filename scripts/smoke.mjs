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
  const authorized = await localPost('/api/settings', { preset: 'developer', shell: true });
  if (!authorized.ok) throw new Error('Failed to grant isolated smoke test permissions.');

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
    'fs_read_many',
    'fs_write',
    'fs_append',
    'fs_edit',
    'fs_mkdir',
    'fs_delete',
    'fs_move',
    'fs_copy',
    'fs_search',
    'fs_project_snapshot',
    'run_command',
    'run_process',
    'execution_output',
    'process_output',
    'process_list',
    'process_stdin',
    'process_terminate',
    'git_status',
    'git_diff',
    'git_diff_summary',
    'git_log',
    'git_inspect',
    'git_stage',
    'git_unstage',
    'git_create_branch',
    'git_commit',
  ];
  const missing = expected.filter((name) => !toolNames.has(name));
  if (missing.length > 0) throw new Error(`Missing tools: ${missing.join(', ')}`);

  const infoRaw = await callRaw(client, 'server_info');
  const infoText = textOf(infoRaw);
  if (/\n\s+"/.test(infoText)) throw new Error(`MCP JSON response is not compact: ${infoText}`);
  const info = jsonOf(infoRaw, 'server_info');
  if (info.name !== 'chatx') throw new Error(`Unexpected server info: ${JSON.stringify(info)}`);

  const invocationResponse = await fetch(`${baseUrl}/api/invocations`);
  const invocationPayload = await invocationResponse.json();
  if (!invocationResponse.ok || !invocationPayload.entries?.some((entry) => entry.tool === 'server_info' && entry.status === 'ok')) {
    throw new Error(`Invocation log did not record server_info: ${JSON.stringify(invocationPayload)}`);
  }
  if (invocationPayload.entries?.some((entry) => 'arguments' in entry || 'input' in entry)) {
    throw new Error('Invocation log must not record tool arguments.');
  }
  const infoInvocation = invocationPayload.entries?.find((entry) => entry.tool === 'server_info');
  if (typeof infoInvocation?.resultBytes !== 'number' || !Array.isArray(invocationPayload.summary)) {
    throw new Error(`Invocation performance metrics are missing: ${JSON.stringify(invocationPayload)}`);
  }

  await call(client, 'fs_write', { path: smokeFile, content: 'hello\nsecond line\n' });
  await call(client, 'fs_edit', { path: smokeFile, old_text: 'hello', new_text: 'world' });
  const read = await call(client, 'fs_read', { path: smokeFile });
  if (!read.content.includes('world')) throw new Error(`fs_read did not return edited content: ${read.content}`);

  const batchRead = await call(client, 'fs_read_many', {
    files: [
      { path: smokeFile, start_line: 1, end_line: 1 },
      { path: path.join(tempRoot, 'missing.txt') },
    ],
    concurrency: 2,
  });
  if (batchRead.requested !== 2 || batchRead.succeeded !== 1 || batchRead.failed !== 1) {
    throw new Error(`fs_read_many returned unexpected counts: ${JSON.stringify(batchRead)}`);
  }
  if (batchRead.results?.[0]?.content !== 'world' || batchRead.results?.[1]?.ok !== false) {
    throw new Error(`fs_read_many returned unexpected results: ${JSON.stringify(batchRead)}`);
  }
  const largeFile = path.join(tempRoot, 'large.txt');
  await fs.writeFile(largeFile, 'x'.repeat(4_096), 'utf8');
  const boundedRead = await call(client, 'fs_read', { path: largeFile, max_response_bytes: 1_024 });
  if (boundedRead.bytes_read !== 1_024 || boundedRead.next_offset !== 1_024 || boundedRead.truncated !== true) {
    throw new Error(`fs_read response budget failed: ${JSON.stringify(boundedRead)}`);
  }

  const excludedRoot = path.join(tempRoot, 'node_modules', 'hidden');
  await fs.mkdir(excludedRoot, { recursive: true });
  await fs.writeFile(path.join(excludedRoot, 'ignored.txt'), 'world\n', 'utf8');
  const listing = await call(client, 'fs_list', {
    path: tempRoot,
    recursive: true,
    include_metadata: false,
    max_entries: 100,
  });
  if (listing.entries.some((entry) => /node_modules[\\/]/.test(entry.path))) {
    throw new Error(`fs_list descended into an excluded directory: ${JSON.stringify(listing)}`);
  }
  const nodeModules = listing.entries.find((entry) => entry.path === 'node_modules');
  if (!nodeModules?.excluded || 'size' in nodeModules) {
    throw new Error(`fs_list exclusion or metadata control failed: ${JSON.stringify(nodeModules)}`);
  }
  const limitedListing = await call(client, 'fs_list', { path: tempRoot, max_entries: 1 });
  if (limitedListing.entry_count !== 1 || limitedListing.reached_entry_limit !== true || limitedListing.next_offset !== 1) {
    throw new Error(`fs_list result limit failed: ${JSON.stringify(limitedListing)}`);
  }
  const nextListing = await call(client, 'fs_list', { path: tempRoot, max_entries: 1, offset: limitedListing.next_offset });
  if (nextListing.entry_count !== 1 || nextListing.entries[0]?.path === limitedListing.entries[0]?.path) {
    throw new Error(`fs_list pagination failed: ${JSON.stringify(nextListing)}`);
  }

  const search = await call(client, 'fs_search', { root: tempRoot, query: 'world' });
  if (search.result_count < 1) throw new Error('fs_search did not find the edited text.');
  if (!['ripgrep', 'javascript'].includes(search.search_engine)) {
    throw new Error(`fs_search did not report its engine: ${JSON.stringify(search)}`);
  }
  if (search.results.some((entry) => /node_modules[\\/]/.test(entry.path))) {
    throw new Error(`fs_search descended into an excluded directory: ${JSON.stringify(search)}`);
  }
  const regexSearch = await call(client, 'fs_search', { root: tempRoot, query: 'w.rld', regex: true });
  if (regexSearch.result_count < 1 || (search.search_engine === 'ripgrep' && regexSearch.search_engine !== 'ripgrep')) {
    throw new Error(`fs_search regex fast path failed: ${JSON.stringify(regexSearch)}`);
  }
  const fallbackSearch = await call(client, 'fs_search', {
    root: tempRoot,
    query: 'world',
    prefer_ripgrep: false,
  });
  if (fallbackSearch.search_engine !== 'javascript' || fallbackSearch.result_count < 1) {
    throw new Error(`fs_search JavaScript fallback failed: ${JSON.stringify(fallbackSearch)}`);
  }

  const shell = await call(client, 'run_command', {
    command: `node -e "process.stdout.write('shell-ok')"`,
    cwd: tempRoot,
  });
  if (shell.exit_code !== 0 || !shell.stdout.includes('shell-ok')) {
    throw new Error(`run_command failed: ${JSON.stringify(shell)}`);
  }

  const directProcess = await call(client, 'run_process', {
    executable: process.execPath,
    args: ['-e', "process.stdout.write('process-ok')"],
    cwd: tempRoot,
  });
  if (directProcess.exit_code !== 0 || !directProcess.stdout.includes('process-ok')) {
    throw new Error(`run_process failed: ${JSON.stringify(directProcess)}`);
  }
  const pagedProcess = await call(client, 'run_process', {
    executable: process.execPath,
    args: ['-e', "process.stdout.write('a'.repeat(2500))"],
    cwd: tempRoot,
    max_output_chars: 1_000,
  });
  const pagedProcessNext = await call(client, 'execution_output', {
    execution_id: pagedProcess.execution_id,
    stdout_offset: pagedProcess.stdout_next_offset,
    max_chars: 1_000,
  });
  if (pagedProcess.stdout.length !== 1_000 || pagedProcess.stdout_next_offset !== 1_000 || pagedProcessNext.stdout_offset !== 1_000) {
    throw new Error(`run_process output pagination failed: ${JSON.stringify({ pagedProcess, pagedProcessNext })}`);
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

  const inspected = await call(client, 'git_inspect', { repo: gitRepo, max_count: 5 });
  if (
    inspected.status?.exit_code !== 0 ||
    inspected.log?.exit_code !== 0 ||
    !inspected.log?.stdout?.includes('initial smoke commit')
  ) {
    throw new Error(`git_inspect failed: ${JSON.stringify(inspected)}`);
  }

  const snapshot = await call(client, 'fs_project_snapshot', {
    root: gitRepo,
    key_files: ['tracked.txt'],
    max_depth: 2,
    max_entries: 100,
  });
  if (
    snapshot.tree?.entry_count < 1 ||
    snapshot.key_files?.[0]?.content !== 'one\n' ||
    snapshot.git?.status?.exit_code !== 0
  ) {
    throw new Error(`fs_project_snapshot failed: ${JSON.stringify(snapshot)}`);
  }
  const nonGitSnapshot = await call(client, 'fs_project_snapshot', {
    root: path.join(tempRoot, 'node_modules'),
    key_files: [],
    include_git: true,
  });
  if (nonGitSnapshot.git?.available !== false || !nonGitSnapshot.git?.error?.includes('not inside')) {
    throw new Error(`fs_project_snapshot non-Git detection failed: ${JSON.stringify(nonGitSnapshot)}`);
  }

  await fs.writeFile(tracked, 'two\n', 'utf8');
  const diffSummary = await call(client, 'git_diff_summary', { repo: gitRepo });
  if (diffSummary.file_count !== 1 || diffSummary.files?.[0]?.path !== 'tracked.txt') {
    throw new Error(`git_diff_summary failed: ${JSON.stringify(diffSummary)}`);
  }
  const boundedDiff = await call(client, 'git_diff', { repo: gitRepo, max_chars: 1_024 });
  if (boundedDiff.output_offset !== 0 || typeof boundedDiff.next_offset === 'undefined') {
    throw new Error(`git_diff response metadata missing: ${JSON.stringify(boundedDiff)}`);
  }
  await call(client, 'git_stage', { repo: gitRepo, paths: ['tracked.txt'] });
  const unstaged = await call(client, 'git_unstage', { repo: gitRepo, paths: ['tracked.txt'] });
  if (unstaged.exit_code !== 0) throw new Error(`git_unstage failed: ${JSON.stringify(unstaged)}`);
  const branch = await call(client, 'git_create_branch', { repo: gitRepo, name: 'smoke-branch' });
  if (branch.exit_code !== 0) throw new Error(`git_create_branch failed: ${JSON.stringify(branch)}`);

  if (toolNames.has('git_run')) throw new Error('git_run must not be advertised while Advanced Git is disabled.');

  const presetResponse = await localPost('/api/settings', { preset: 'developer' });
  const presetBody = await presetResponse.json();
  if (!presetResponse.ok || presetBody.status?.policy?.permissionPreset !== 'developer') {
    throw new Error(`Developer preset failed: ${JSON.stringify(presetBody)}`);
  }
  if (presetBody.status.policy.permissions.shell !== false || presetBody.status.policy.permissions.gitAdvanced !== false) {
    throw new Error(`Developer preset permissions are unsafe: ${JSON.stringify(presetBody.status.policy.permissions)}`);
  }
  const developerTools = new Set((await client.listTools()).tools.map((tool) => tool.name));
  if (developerTools.has('run_command') || developerTools.has('run_process') || developerTools.has('git_run')) {
    throw new Error(`Disabled tools are still advertised: ${JSON.stringify([...developerTools])}`);
  }
  if (!developerTools.has('fs_write') || !developerTools.has('git_commit')) {
    throw new Error(`Developer tools were unexpectedly hidden: ${JSON.stringify([...developerTools])}`);
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
