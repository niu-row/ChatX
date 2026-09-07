import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

const resourceDir = path.resolve('src-tauri', 'resources');
const node = path.join(resourceDir, 'node.exe');
const launcher = path.join(resourceDir, 'desktop-commander-launcher.mjs');
const entry = path.join(resourceDir, 'desktop-commander', 'node_modules', '@wonderwhy-er', 'desktop-commander', 'dist', 'index.js');
for (const file of [node, launcher, entry]) {
  if (!fs.existsSync(file)) throw new Error(`missing prepared bridge resource: ${file}`);
}

const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'chatx-dc-smoke-'));
const home = path.join(temp, 'dc-home');
const sample = path.join(temp, 'sample.txt');
const client = new Client({ name: 'chatx-bridge-smoke', version: '0.3.0' });
const transportEnv = Object.fromEntries(
  Object.entries(process.env).filter(([, value]) => typeof value === 'string'),
);
transportEnv.CHATX_TUNNEL_RUNTIME_KEY = 'chatx-secret-smoke';
const transport = new StdioClientTransport({
  command: node,
  args: [launcher, home, entry, '--no-onboarding'],
  env: transportEnv,
});

function textOf(result) {
  return (result.content ?? [])
    .filter((item) => item.type === 'text')
    .map((item) => item.text)
    .join('\n');
}

async function call(name, args = {}) {
  const result = await client.callTool({ name, arguments: args });
  if (result.isError) throw new Error(`${name} failed:\n${textOf(result)}`);
  return result;
}

try {
  await client.connect(transport);
  const listed = await client.listTools();
  const names = new Set((listed.tools ?? []).map((tool) => tool.name));
  for (const name of ['list_directory', 'read_file', 'write_file', 'start_search', 'get_more_search_results', 'start_process']) {
    assert.ok(names.has(name), `Desktop Commander tool missing: ${name}`);
  }

  const escapedNode = `"${node.replace(/"/g, '\\"')}"`;
  const secretProbe = await call('start_process', {
    command: `${escapedNode} -e "console.log(process.env.CHATX_TUNNEL_RUNTIME_KEY || 'CHATX_KEY_STRIPPED')"`,
    timeout_ms: 5000,
  });
  const secretProbeText = textOf(secretProbe);
  assert.match(secretProbeText, /CHATX_KEY_STRIPPED/, 'Desktop Commander child process did not confirm Runtime Key stripping');
  assert.doesNotMatch(secretProbeText, /chatx-secret-smoke/, 'Runtime API Key leaked into a Desktop Commander child process');

  await call('write_file', { path: sample, content: 'chatx-ripgrep-smoke\n' });
  const read = await call('read_file', { path: sample, offset: 0, length: 20 });
  assert.match(textOf(read), /chatx-ripgrep-smoke/, 'read_file did not return the written content');

  const search = await call('start_search', {
    path: temp,
    pattern: 'chatx-ripgrep-smoke',
    searchType: 'content',
    literalSearch: true,
    maxResults: 20,
  });
  const started = textOf(search);
  const sessionId = started.match(/session:\s*([^\s]+)/i)?.[1];
  assert.ok(sessionId, `start_search did not return a session id:\n${started}`);

  let found = /chatx-ripgrep-smoke/.test(started);
  for (let attempt = 0; attempt < 30 && !found; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 100));
    const page = await call('get_more_search_results', { sessionId, offset: 0, length: 100 });
    const text = textOf(page);
    if (/chatx-ripgrep-smoke/.test(text)) found = true;
    if (/Search completed/i.test(text) && !found) break;
  }
  assert.ok(found, 'Desktop Commander start_search completed without finding the smoke fixture; verify bundled ripgrep.');

  console.log(`bridge smoke passed: ${listed.tools?.length ?? 0} Desktop Commander tools exposed; Runtime Key isolated`);
} finally {
  try { await client.close(); } catch {}
  fs.rmSync(temp, { recursive: true, force: true });
}
