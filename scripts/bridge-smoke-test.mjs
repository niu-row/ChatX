import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));
const resourceDir = process.env.CHATX_RUNTIME_RESOURCE_DIR?.trim()
  ? path.resolve(process.env.CHATX_RUNTIME_RESOURCE_DIR.trim())
  : path.resolve('src-tauri', 'resources');
const node = path.join(resourceDir, process.platform === 'win32' ? 'node.exe' : 'node');
const launcher = path.join(resourceDir, 'desktop-commander-launcher.mjs');
const entry = path.join(resourceDir, 'desktop-commander', 'dist', 'index.js');
for (const file of [node, launcher, entry]) {
  if (!fs.existsSync(file)) throw new Error(`missing prepared bridge resource: ${file}`);
}

const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'chatx-dc-smoke-'));
const home = path.join(temp, 'dc-home');
const hostHome = process.env.CHATX_BRIDGE_HOST_HOME?.trim() || path.join(temp, 'host-home');
fs.mkdirSync(hostHome, { recursive: true });
const homeProbeScript = path.join(temp, 'home-probe.mjs');
fs.writeFileSync(homeProbeScript, "console.log(JSON.stringify({home:process.env.HOME,userProfile:process.env.USERPROFILE,realHome:process.env.CHATX_REAL_HOME,programData:process.env.ProgramData,allUsersProfile:process.env.ALLUSERSPROFILE,computerName:process.env.COMPUTERNAME,os:process.env.OS,publicDir:process.env.PUBLIC}));\n");
const sample = path.join(temp, 'sample.txt');
const client = new Client({
  name: 'chatx-bridge-smoke',
  version: pkg.version,
});
const inheritedNames = process.platform === 'win32'
  ? [
      'APPDATA', 'HOMEDRIVE', 'HOMEPATH', 'LOCALAPPDATA', 'PATH',
      'PROCESSOR_ARCHITECTURE', 'SYSTEMDRIVE', 'SYSTEMROOT', 'TEMP',
      'USERNAME', 'USERPROFILE', 'PROGRAMFILES',
    ]
  : ['HOME', 'LOGNAME', 'PATH', 'SHELL', 'TERM', 'USER'];
const transportEnv = Object.fromEntries(
  inheritedNames
    .map((name) => [name, process.env[name]])
    .filter(([, value]) => typeof value === 'string'),
);
transportEnv.CHATX_TUNNEL_RUNTIME_KEY = 'chatx-secret-smoke';
transportEnv.CHATX_REAL_HOME = hostHome;
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

async function assertLauncherExitsOnClosedStdin() {
  const lifecycleHome = path.join(temp, 'lifecycle-home');
  fs.mkdirSync(lifecycleHome, { recursive: true });
  const child = spawn(node, [launcher, lifecycleHome, entry, '--no-onboarding'], {
    env: transportEnv,
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
  });
  let stdout = '';
  let stderr = '';
  child.stdout?.on('data', (chunk) => { stdout += chunk.toString(); });
  child.stderr?.on('data', (chunk) => { stderr += chunk.toString(); });
  await once(child, 'spawn');
  child.stdin.write(`${JSON.stringify({
    jsonrpc: '2.0',
    id: 1,
    method: 'initialize',
    params: {
      protocolVersion: '2024-11-05',
      capabilities: {},
      clientInfo: { name: 'chatx-lifecycle-smoke', version: pkg.version },
    },
  })}\n`);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`Desktop Commander launcher did not initialize for stdin-close smoke. stdout: ${stdout}; stderr: ${stderr}`));
    }, 8000);
    const poll = setInterval(() => {
      if (/"id"\s*:\s*1/.test(stdout)) {
        clearInterval(poll);
        clearTimeout(timer);
        resolve();
      }
    }, 25);
  });
  child.stdin.end();
  const [code, signal] = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`Desktop Commander launcher did not exit after initialized stdin closed. stderr: ${stderr}`));
    }, 3000);
    child.once('exit', (exitCode, exitSignal) => {
      clearTimeout(timer);
      resolve([exitCode, exitSignal]);
    });
    child.once('error', (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
  assert.equal(code, 0, `Desktop Commander launcher stdin-close exit failed (signal=${signal}, stderr=${stderr})`);
}

try {
  await assertLauncherExitsOnClosedStdin();
  await client.connect(transport);
  const listed = await client.listTools();
  assert.equal(listed.tools.length, 26, 'Removing UI must preserve the tool set');
  for (const tool of listed.tools) {
    for (const key of ['ui', 'ui/resourceUri', 'openai/outputTemplate', 'openai/widgetAccessible']) {
      assert.equal(tool._meta?.[key], undefined, `${tool.name} still advertises UI: ${key}`);
    }
  }
  assert.deepEqual((await client.listResources()).resources, []);
  assert.deepEqual((await client.listResourceTemplates()).resourceTemplates, []);
  for (const uri of ['ui://desktop-commander/file-preview', 'ui://desktop-commander/config-editor']) {
    await assert.rejects(client.readResource({ uri }), /ChatX does not expose UI resources/);
  }
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

  const escapedHomeProbeScript = `"${homeProbeScript.replace(/"/g, '\\"')}"`;
  const homeProbeCommand = process.platform === 'win32'
    ? `& ${escapedNode} ${escapedHomeProbeScript}`
    : `${escapedNode} ${escapedHomeProbeScript}`;
  const homeProbe = await call('start_process', {
    command: homeProbeCommand,
    timeout_ms: 5000,
  });
  const homeProbeText = textOf(homeProbe);
  const homeProbeLine = homeProbeText.split(/\r?\n/).find((line) => line.trim().startsWith('{') && line.trim().endsWith('}'));
  assert.ok(homeProbeLine, `Desktop Commander child environment probe returned no JSON:\n${homeProbeText}`);
  const childEnv = JSON.parse(homeProbeLine);
  assert.equal(path.resolve(childEnv.home), path.resolve(hostHome), 'Desktop Commander child process did not inherit the real account HOME');
  assert.equal(path.resolve(childEnv.userProfile), path.resolve(hostHome), 'Desktop Commander child process did not inherit the real account USERPROFILE');
  assert.equal(path.resolve(childEnv.realHome), path.resolve(hostHome), 'CHATX_REAL_HOME did not match the real account home');
  assert.notEqual(path.resolve(childEnv.home), path.resolve(home), 'Desktop Commander child process still inherited the isolated Desktop Commander home');
  assert.equal(process.env.HOME === home, false, 'Bridge smoke process unexpectedly mutated its own HOME');
  if (process.platform === 'win32') {
    const systemRoot = process.env.SystemRoot || process.env.WINDIR || 'C:\\Windows';
    const systemDrive = process.env.SystemDrive || path.win32.parse(systemRoot).root.replace(/[\\/]$/, '') || 'C:';
    const expectedProgramData = `${systemDrive}\\ProgramData`;
    assert.equal(childEnv.programData?.toLowerCase(), expectedProgramData.toLowerCase(), 'ProgramData was not restored for Desktop Commander child processes');
    assert.equal(childEnv.allUsersProfile?.toLowerCase(), expectedProgramData.toLowerCase(), 'ALLUSERSPROFILE was not restored for Desktop Commander child processes');
    assert.equal(childEnv.computerName?.toLowerCase(), os.hostname().toLowerCase(), 'COMPUTERNAME was not restored for Desktop Commander child processes');
    assert.equal(childEnv.os, 'Windows_NT', 'OS was not restored for Desktop Commander child processes');
    assert.equal(childEnv.publicDir?.toLowerCase(), path.win32.join(path.win32.dirname(hostHome), 'Public').toLowerCase(), 'PUBLIC was not restored for Desktop Commander child processes');
  }

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
