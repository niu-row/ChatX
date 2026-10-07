import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

const resourceDir = process.env.CHATX_RUNTIME_RESOURCE_DIR?.trim()
  ? path.resolve(process.env.CHATX_RUNTIME_RESOURCE_DIR.trim())
  : path.resolve('src-tauri', 'resources');
const node = path.join(resourceDir, process.platform === 'win32' ? 'node.exe' : 'node');
const launcher = path.join(resourceDir, 'desktop-commander-launcher.mjs');
const entry = path.join(resourceDir, 'desktop-commander', 'dist', 'index.js');
const helper = path.join(resourceDir, 'desktop-host-environment.mjs');
for (const file of [node, launcher, entry, helper]) {
  if (!fs.existsSync(file)) throw new Error(`missing prepared developer environment resource: ${file}`);
}

const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'chatx-dev-env-smoke-'));
const isolatedHome = path.join(temp, 'dc-home');
const realHome = os.userInfo().homedir;
const probe = path.join(temp, 'probe.mjs');
fs.writeFileSync(probe, `
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

function locate(name) {
  if (process.platform === 'win32') {
    const r = spawnSync('where.exe', [name], { encoding: 'utf8', windowsHide: true });
    return r.status === 0 ? r.stdout.split(/\\r?\\n/).find(Boolean)?.trim() : null;
  }
  const r = spawnSync('/usr/bin/env', ['sh', '-lc', 'command -v ' + name], { encoding: 'utf8' });
  return r.status === 0 ? r.stdout.trim().split(/\\r?\\n/)[0] : null;
}

function run(label, executable, args) {
  if (!executable || !fs.existsSync(executable)) return { label, available: false };
  const needsShell = process.platform === 'win32' && /\\.(cmd|bat)$/i.test(executable);
  const escapedExecutable = executable.replaceAll("'", "''");
  const shellCommand = "& '" + escapedExecutable + "' " + args.join(' ');
  const r = needsShell
    ? spawnSync('powershell.exe', ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', shellCommand], { encoding: 'utf8', timeout: 10000, windowsHide: true })
    : spawnSync(executable, args, { encoding: 'utf8', timeout: 10000, windowsHide: true });
  return {
    label,
    available: true,
    executable,
    status: r.status,
    signal: r.signal,
    error: r.error?.message,
    output: (r.stdout || r.stderr || '').trim(),
  };
}

const specs = [];
if (process.platform === 'win32') {
  const systemRoot = process.env.SystemRoot || process.env.WINDIR || 'C:/Windows';
  const systemSsh = path.join(systemRoot, 'System32', 'OpenSSH', 'ssh.exe');
  specs.push(run('windows-system-ssh', fs.existsSync(systemSsh) ? systemSsh : null, ['-V']));
  specs.push(run('git', locate('git.exe'), ['--version']));
  specs.push(run('npm', locate('npm.cmd'), ['--version']));
  specs.push(run('cargo', locate('cargo.exe'), ['--version']));
} else {
  specs.push(run('ssh', locate('ssh'), ['-V']));
  specs.push(run('git', locate('git'), ['--version']));
  specs.push(run('npm', locate('npm'), ['--version']));
  specs.push(run('cargo', locate('cargo'), ['--version']));
}
console.log(JSON.stringify({
  env: {
    HOME: process.env.HOME,
    USERPROFILE: process.env.USERPROFILE,
    ProgramData: process.env.ProgramData,
    COMPUTERNAME: process.env.COMPUTERNAME,
  },
  specs,
}));
`);

const inheritedNames = process.platform === 'win32'
  ? ['APPDATA', 'HOMEDRIVE', 'HOMEPATH', 'LOCALAPPDATA', 'PATH', 'PROCESSOR_ARCHITECTURE', 'SYSTEMDRIVE', 'SYSTEMROOT', 'TEMP', 'USERNAME', 'USERPROFILE', 'PROGRAMFILES']
  : ['HOME', 'LOGNAME', 'PATH', 'SHELL', 'TERM', 'USER'];
const transportEnv = Object.fromEntries(inheritedNames.map((name) => [name, process.env[name]]).filter(([, value]) => typeof value === 'string'));
transportEnv.CHATX_REAL_HOME = realHome;

const client = new Client({ name: 'chatx-developer-environment-smoke', version: '1.0.0' });
const transport = new StdioClientTransport({ command: node, args: [launcher, isolatedHome, entry, '--no-onboarding'], env: transportEnv });
const textOf = (result) => (result.content ?? []).filter((item) => item.type === 'text').map((item) => item.text).join('\n');

try {
  await client.connect(transport);
  const escapedNode = `\"${node.replace(/\"/g, '\\\"')}\"`;
  const escapedProbe = `\"${probe.replace(/\"/g, '\\\"')}\"`;
  const command = process.platform === 'win32' ? `& ${escapedNode} ${escapedProbe}` : `${escapedNode} ${escapedProbe}`;
  const result = await client.callTool({ name: 'start_process', arguments: { command, timeout_ms: 20000 } });
  if (result.isError) throw new Error(`start_process failed:\n${textOf(result)}`);
  const output = textOf(result);
  const jsonLine = output.split(/\r?\n/).find((line) => line.trim().startsWith('{') && line.trim().endsWith('}'));
  assert.ok(jsonLine, `developer environment probe returned no JSON:\n${output}`);
  const report = JSON.parse(jsonLine);
  assert.equal(path.resolve(report.env.HOME), path.resolve(realHome), 'developer command HOME is not the real account home');
  if (process.platform === 'win32') {
    const systemRoot = process.env.SystemRoot || process.env.WINDIR || 'C:\\Windows';
    const systemDrive = process.env.SystemDrive || path.win32.parse(systemRoot).root.replace(/[\\/]$/, '') || 'C:';
    assert.equal(report.env.ProgramData?.toLowerCase(), `${systemDrive}\\ProgramData`.toLowerCase(), 'ProgramData was not restored for developer commands');
    assert.equal(report.env.COMPUTERNAME?.toLowerCase(), os.hostname().toLowerCase(), 'COMPUTERNAME was not restored for developer commands');
  }
  const available = report.specs.filter((spec) => spec.available);
  assert.ok(available.length > 0, 'no developer tools were available for environment smoke testing');
  for (const spec of available) {
    assert.equal(spec.status, 0, `${spec.label} failed in ChatX developer environment: ${JSON.stringify(spec)}`);
    assert.ok(spec.output, `${spec.label} produced no version output`);
  }
  console.log(`developer environment smoke passed: ${available.map((spec) => spec.label).join(', ')}`);
} finally {
  try { await client.close(); } catch {}
  fs.rmSync(temp, { recursive: true, force: true });
}
