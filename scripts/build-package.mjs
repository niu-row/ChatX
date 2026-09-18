import { spawnSync } from 'node:child_process';
import path from 'node:path';

const args = process.argv.slice(2);

if (process.platform === 'win32') {
  const result = spawnSync(process.execPath, [path.resolve('scripts/build-installer.mjs'), ...args], {
    stdio: 'inherit',
    env: process.env,
    windowsHide: true,
  });
  process.exit(result.status ?? 1);
}

if (process.platform === 'darwin' && process.arch === 'arm64') {
  if (args.includes('--publish')) {
    throw new Error('macOS publish signing/notarization is not configured yet. Use desktop:installer for a local M1 build.');
  }
  const cli = path.resolve('node_modules', '@tauri-apps', 'cli', 'tauri.js');
  const result = spawnSync(process.execPath, [cli, 'build', '--bundles', 'app,dmg'], {
    stdio: 'inherit',
    env: process.env,
  });
  process.exit(result.status ?? 1);
}

throw new Error(`ChatX packaging is not configured for ${process.platform}-${process.arch}.`);
