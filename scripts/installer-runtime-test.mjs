import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';

if (process.platform !== 'win32') {
  console.log('installer runtime test skipped: Windows only');
  process.exit(0);
}
const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'chatx installer test-'));
const installed = path.join(temp, 'installed');
const unrelated = path.join(temp, 'unrelated');
const children = [];
const script = path.resolve('src-tauri/windows/stop-runtime.ps1');
function cleanupRuntime(directory) {
  return spawnSync('powershell.exe', ['-NoLogo', '-NoProfile', '-NonInteractive',
    '-ExecutionPolicy', 'Bypass', '-File', script, '-InstallDir', directory],
    { encoding: 'utf8', windowsHide: true, timeout: 25000 });
}
async function launch(executable) {
  const child = spawn(executable, ['-e', 'console.log("ready"); setInterval(()=>{},1000)'],
    { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  children.push(child);
  await Promise.race([
    once(child.stdout, 'data'),
    once(child, 'error').then(([error]) => { throw error; }),
    once(child, 'exit').then(([code]) => { throw new Error('Early exit: ' + code); }),
  ]);
  return child;
}
try {
  await fs.mkdir(installed);
  await fs.mkdir(unrelated);
  for (const directory of [installed, unrelated]) {
    for (const name of ['node.exe', 'tunnel-client.exe']) {
      await fs.copyFile(process.execPath, path.join(directory, name));
    }
  }
  // Independent children model runtimes orphaned by forced desktop termination.
  const owned = await Promise.all(['node.exe', 'tunnel-client.exe'].map(name =>
    launch(path.join(installed, name))));
  const others = await Promise.all(['node.exe', 'tunnel-client.exe'].map(name =>
    launch(path.join(unrelated, name))));
  const exited = owned.map(child => once(child, 'exit'));
  const result = cleanupRuntime(installed);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  await Promise.all(exited);
  assert.ok(others.every(child => child.exitCode === null && !child.killed),
    'Same-named runtimes in another directory must survive');
  for (const name of ['node.exe', 'tunnel-client.exe']) {
    // A rename alone does not prove an executable is unlocked on Windows.
    const handle = await fs.open(path.join(installed, name), 'r+');
    await handle.close();
  }
  assert.equal(cleanupRuntime(installed).status, 0, 'Cleanup must be idempotent');
  assert.equal(cleanupRuntime(path.join(temp, 'fresh-install')).status, 0);
  console.log('installer runtime test ok: orphan cleanup, unlocked binaries, unrelated processes, repeated/fresh install');
} finally {
  await Promise.all(children.map(async child => {
    if (child.exitCode === null && child.signalCode === null) {
      const exited = once(child, 'exit');
      child.kill();
      await exited;
    }
  }));
  await fs.rm(temp, { recursive: true, force: true });
}
