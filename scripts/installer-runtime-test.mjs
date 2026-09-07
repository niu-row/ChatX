import fs from 'node:fs';
import path from 'node:path';

const resourceDir = path.resolve('src-tauri', 'resources');
const required = [
  'node.exe',
  'tunnel-client.exe',
  'desktop-commander-launcher.mjs',
  'runtime-manifest.json',
  'tunnel-client-LICENSE.txt',
  'tunnel-client-NOTICE.txt',
  'DesktopCommander-LICENSE.txt',
];
for (const name of required) {
  const file = path.join(resourceDir, name);
  if (!fs.existsSync(file)) throw new Error(`missing prepared runtime resource: ${file}`);
}

const dcPackage = path.join(resourceDir, 'desktop-commander', 'node_modules', '@wonderwhy-er', 'desktop-commander', 'package.json');
const dcEntry = path.join(resourceDir, 'desktop-commander', 'node_modules', '@wonderwhy-er', 'desktop-commander', 'dist', 'index.js');
const ripgrep = path.join(resourceDir, 'desktop-commander', 'node_modules', '@vscode', 'ripgrep', 'bin', 'rg.exe');
for (const file of [dcPackage, dcEntry, ripgrep]) {
  if (!fs.existsSync(file)) throw new Error(`Desktop Commander runtime resource missing: ${file}`);
}

const manifest = JSON.parse(fs.readFileSync(path.join(resourceDir, 'runtime-manifest.json'), 'utf8'));
const locked = JSON.parse(fs.readFileSync('runtime-lock.json', 'utf8'));
const installed = JSON.parse(fs.readFileSync(dcPackage, 'utf8'));
if (manifest.schemaVersion !== 2) throw new Error('runtime manifest schema must be 2.');
if (installed.version !== locked.desktopCommander.version) throw new Error(`Desktop Commander version mismatch: ${installed.version}`);
if (manifest.desktopCommander?.version !== installed.version) throw new Error('runtime manifest Desktop Commander version mismatch.');
if (!manifest.desktopCommander?.entry?.endsWith('/dist/index.js')) throw new Error('runtime manifest Desktop Commander entry is invalid.');
if (manifest.desktopCommander?.launcher !== 'desktop-commander-launcher.mjs') throw new Error('runtime manifest launcher is invalid.');
if (manifest.desktopCommander?.telemetryDisabledByEnv !== true) throw new Error('Desktop Commander telemetry kill-switch must be enabled by the ChatX launcher.');
if (!manifest.desktopCommander?.ripgrep?.endsWith('/rg.exe')) throw new Error('runtime manifest ripgrep path is invalid.');
if (!/^[0-9a-f]{64}$/i.test(manifest.desktopCommander?.ripgrepSha256 || '')) throw new Error('runtime manifest must record ripgrep SHA-256.');

console.log(`installer runtime checks passed: Desktop Commander ${installed.version} + bundled ripgrep`);
