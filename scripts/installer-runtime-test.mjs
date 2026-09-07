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
const powershell32 = path.join(process.env.WINDIR ?? 'C:\\Windows', 'SysWOW64', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
const installerPowerShell = await fs.access(powershell32).then(() => powershell32, () => 'powershell.exe');
function cleanupRuntime(directory, timeoutSeconds = 20) {
  return spawnSync(installerPowerShell, ['-NoLogo', '-NoProfile', '-NonInteractive',
    '-ExecutionPolicy', 'Bypass', '-File', script, '-InstallDir', directory,
    '-TimeoutSeconds', String(timeoutSeconds)],
    { encoding: 'utf8', windowsHide: true, timeout: Math.max(10000, (timeoutSeconds + 8) * 1000) });
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
async function lockFile(file) {
  const command = '$s=[IO.File]::Open($env:CHATX_LOCK_FILE,[IO.FileMode]::Open,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None);' +
    '[Console]::Out.WriteLine("ready"); Start-Sleep -Seconds 30; $s.Dispose()';
  const child = spawn('powershell.exe', ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', command], {
    env: { ...process.env, CHATX_LOCK_FILE: file },
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  });
  children.push(child);
  await Promise.race([
    once(child.stdout, 'data'),
    once(child, 'error').then(([error]) => { throw error; }),
    once(child, 'exit').then(([code]) => { throw new Error('Locker exited early: ' + code); }),
  ]);
  return child;
}
try {
  await fs.mkdir(installed);
  await fs.mkdir(unrelated);
  const runtimeNames = ['chatx-desktop.exe', 'node.exe', 'tunnel-client.exe'];
  for (const directory of [installed, unrelated]) {
    for (const name of runtimeNames) {
      await fs.copyFile(process.execPath, path.join(directory, name));
    }
  }
  // Independent children model the tray desktop plus runtimes orphaned by forced termination.
  const owned = await Promise.all(runtimeNames.map(name => launch(path.join(installed, name))));
  const others = await Promise.all(runtimeNames.map(name => launch(path.join(unrelated, name))));
  const exited = owned.map(child => once(child, 'exit'));
  const result = cleanupRuntime(installed);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  await Promise.all(exited);
  assert.ok(others.every(child => child.exitCode === null && !child.killed),
    'Same-named runtimes in another directory must survive');
  for (const name of runtimeNames) {
    // A rename alone does not prove an executable is unlocked on Windows.
    const handle = await fs.open(path.join(installed, name), 'r+');
    await handle.close();
  }

  // A non-runtime process holding a replaceable resource must be reported, not killed.
  const backendResource = path.join(installed, 'chatgptx-backend.mjs');
  await fs.writeFile(backendResource, 'locked resource');
  const locker = await lockFile(backendResource);
  const lockedResult = cleanupRuntime(installed, 2);
  assert.equal(lockedResult.status, 1, 'Cleanup must fail while an unrelated process holds a runtime resource');
  assert.match(lockedResult.stdout + lockedResult.stderr, /chatgptx-backend\.mjs/i);
  const lockerExited = once(locker, 'exit');
  locker.kill();
  await lockerExited;

  assert.equal(cleanupRuntime(installed).status, 0, 'Cleanup must be idempotent after locks are released');
  assert.equal(cleanupRuntime(path.join(temp, 'fresh-install')).status, 0);
  console.log('installer runtime test ok: tray/runtime cleanup, exact-path isolation, lock reporting, repeated/fresh install');
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
