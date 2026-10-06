import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { toolchainEnv } from './toolchain-env.mjs';

const cli = path.resolve(
  'node_modules',
  '@tauri-apps',
  'cli',
  'tauri.js',
);
const args = process.argv.slice(2);
const result = spawnSync(process.execPath, [cli, ...args], {
  cwd: process.cwd(),
  env: toolchainEnv(),
  stdio: 'inherit',
  windowsHide: process.platform === 'win32',
});
if (result.error) throw result.error;
process.exit(result.status ?? 1);
