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

const dcRoot = path.join(resourceDir, 'desktop-commander');
const dcPackage = path.join(dcRoot, 'package.json');
const dcBundleManifest = path.join(dcRoot, 'manifest.json');
const dcEntry = path.join(dcRoot, 'dist', 'index.js');
const dcLicense = path.join(dcRoot, 'LICENSE');
for (const file of [dcPackage, dcBundleManifest, dcEntry, dcLicense]) {
  if (!fs.existsSync(file)) throw new Error(`Desktop Commander runtime resource missing: ${file}`);
}

const manifest = JSON.parse(fs.readFileSync(path.join(resourceDir, 'runtime-manifest.json'), 'utf8'));
const locked = JSON.parse(fs.readFileSync('runtime-lock.json', 'utf8'));
const installed = JSON.parse(fs.readFileSync(dcPackage, 'utf8'));
const bundleManifest = JSON.parse(fs.readFileSync(dcBundleManifest, 'utf8'));

if (manifest.schemaVersion !== 3) throw new Error('runtime manifest schema must be 3.');
if (manifest.runtimePolicy !== 'locked-github-release') throw new Error('runtime manifest policy must be locked-github-release.');
if (locked.schemaVersion !== 3) throw new Error('runtime lock schema must be 3.');
if (installed.version !== locked.desktopCommander.version) throw new Error(`Desktop Commander version mismatch: ${installed.version}`);
if (bundleManifest.version !== locked.desktopCommander.version) throw new Error(`Desktop Commander MCPB manifest version mismatch: ${bundleManifest.version}`);
if (manifest.desktopCommander?.version !== installed.version) throw new Error('runtime manifest Desktop Commander version mismatch.');
if (manifest.desktopCommander?.source !== 'github-release-mcpb') throw new Error('runtime manifest Desktop Commander source must be github-release-mcpb.');
if (manifest.desktopCommander?.sourceCommit !== locked.desktopCommander.sourceCommit) throw new Error('runtime manifest Desktop Commander source commit mismatch.');
if (manifest.desktopCommander?.releaseTag !== locked.desktopCommander.releaseTag) throw new Error('runtime manifest Desktop Commander release tag mismatch.');
if (manifest.desktopCommander?.releaseAsset !== locked.desktopCommander.releaseAsset) throw new Error('runtime manifest Desktop Commander release asset mismatch.');
if (manifest.desktopCommander?.releaseSize !== locked.desktopCommander.size) throw new Error('runtime manifest Desktop Commander release size mismatch.');
if (manifest.desktopCommander?.releaseSha256 !== locked.desktopCommander.sha256) throw new Error('runtime manifest Desktop Commander release SHA-256 mismatch.');
if (manifest.desktopCommander?.bundleManifestSha256 !== sha256(dcBundleManifest)) throw new Error('runtime manifest Desktop Commander bundle manifest SHA-256 mismatch.');
if (manifest.desktopCommander?.entry !== 'desktop-commander/dist/index.js') throw new Error('runtime manifest Desktop Commander entry is invalid.');
if (manifest.desktopCommander?.launcher !== 'desktop-commander-launcher.mjs') throw new Error('runtime manifest launcher is invalid.');
if (manifest.desktopCommander?.telemetryDisabledByEnv !== true) throw new Error('Desktop Commander telemetry kill-switch must be enabled by the ChatX launcher.');
if (manifest.desktopCommander?.runtimeKeyStrippedByLauncher !== true) throw new Error('Desktop Commander launcher must strip the Tunnel Runtime Key.');

const bundledLicense = path.join(resourceDir, 'DesktopCommander-LICENSE.txt');
if (sha256(bundledLicense) !== sha256(dcLicense)) throw new Error('Desktop Commander bundled LICENSE does not match the locked MCPB asset.');

const ripgrepRelative = manifest.desktopCommander?.ripgrep;
if (typeof ripgrepRelative !== 'string' || !/rg-[^/]+\.exe$/i.test(ripgrepRelative)) {
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

console.log(`installer runtime checks passed: Desktop Commander ${installed.version} locked GitHub MCPB + bundled ripgrep`);
