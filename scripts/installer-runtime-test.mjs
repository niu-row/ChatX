import crypto from 'node:crypto';
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

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

const dcPackage = path.join(resourceDir, 'desktop-commander', 'node_modules', '@wonderwhy-er', 'desktop-commander', 'package.json');
const dcEntry = path.join(resourceDir, 'desktop-commander', 'node_modules', '@wonderwhy-er', 'desktop-commander', 'dist', 'index.js');
for (const file of [dcPackage, dcEntry]) {
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

const ripgrepRelative = manifest.desktopCommander?.ripgrep;
if (typeof ripgrepRelative !== 'string' || !ripgrepRelative.endsWith('/rg.exe')) {
  throw new Error('runtime manifest ripgrep path is invalid.');
}
const ripgrep = path.resolve(resourceDir, ...ripgrepRelative.split('/'));
const relativeCheck = path.relative(resourceDir, ripgrep);
if (!relativeCheck || relativeCheck === '..' || relativeCheck.startsWith(`..${path.sep}`)) {
  throw new Error(`runtime manifest ripgrep path escapes resources: ${ripgrepRelative}`);
}
if (!fs.existsSync(ripgrep)) throw new Error(`Desktop Commander ripgrep binary is missing: ${ripgrep}`);
const expectedRipgrepHash = manifest.desktopCommander?.ripgrepSha256 || '';
if (!/^[0-9a-f]{64}$/i.test(expectedRipgrepHash)) throw new Error('runtime manifest must record ripgrep SHA-256.');
if (sha256(ripgrep) !== expectedRipgrepHash) throw new Error('bundled ripgrep SHA-256 does not match runtime manifest.');

console.log(`installer runtime checks passed: Desktop Commander ${installed.version} + bundled ripgrep`);
