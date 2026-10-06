import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { toolchainEnv } from './toolchain-env.mjs';

const args = process.argv.slice(2);
const MAC_BUNDLE_ID = 'com.chatgptx.local';
const LOCAL_SIGNING_PATH = path.resolve('.chatx-local-signing.json');

function runText(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', ...options });
  if ((result.status ?? 1) !== 0) {
    throw new Error(command + ' failed: ' + (result.stderr || result.stdout || 'unknown error'));
  }
  return String(result.stdout ?? '') + String(result.stderr ?? '');
}

function loadLocalMacSigning() {
  if (!fs.existsSync(LOCAL_SIGNING_PATH)) return null;
  const config = JSON.parse(fs.readFileSync(LOCAL_SIGNING_PATH, 'utf8'));
  for (const key of ['identity', 'certificateSha1', 'keychain', 'home']) {
    if (typeof config[key] !== 'string' || !config[key].trim()) {
      throw new Error(`Local macOS signing config is missing ${key}.`);
    }
  }
  config.certificateSha1 = config.certificateSha1.replace(/\s+/g, '').toUpperCase();
  if (!/^[0-9A-F]{40}$/.test(config.certificateSha1)) throw new Error('Local macOS signing certificateSha1 must be 40 hex characters.');
  if (!fs.existsSync(config.keychain)) throw new Error(`Local macOS signing keychain does not exist: ${config.keychain}`);
  if (!fs.existsSync(config.home)) throw new Error(`Local macOS signing home does not exist: ${config.home}`);
  const identities = runText('/usr/bin/security', ['find-identity', '-v', '-p', 'codesigning', config.keychain]);
  if (!identities.includes(config.certificateSha1) || !identities.includes(`\"${config.identity}\"`)) {
    throw new Error(`Local macOS signing identity was not found in ${config.keychain}.`);
  }
  return config;
}

function verifyMacBundle(appPath, localSigning = null) {
  const signature = runText('/usr/bin/codesign', ['-dv', '--verbose=4', appPath]);
  const signingIdentifier = signature.match(/^Identifier=(.+)$/m)?.[1]?.trim();
  if (signingIdentifier !== MAC_BUNDLE_ID) {
    throw new Error('macOS codesign identifier mismatch: expected ' + MAC_BUNDLE_ID + ', got ' + (signingIdentifier ?? 'missing'));
  }

  const plistPath = path.join(appPath, 'Contents', 'Info.plist');
  const bundleIdentifier = runText('/usr/bin/plutil', ['-extract', 'CFBundleIdentifier', 'raw', '-o', '-', plistPath]).trim();
  if (bundleIdentifier !== MAC_BUNDLE_ID) {
    throw new Error('macOS CFBundleIdentifier mismatch: expected ' + MAC_BUNDLE_ID + ', got ' + bundleIdentifier);
  }
  const localNetworkUsage = runText('/usr/bin/plutil', ['-extract', 'NSLocalNetworkUsageDescription', 'raw', '-o', '-', plistPath]).trim();
  if (!localNetworkUsage) throw new Error('macOS bundle is missing NSLocalNetworkUsageDescription.');

  runText('/usr/bin/codesign', ['--verify', '--deep', '--strict', '--verbose=2', appPath]);
  if (localSigning) {
    if (!signature.includes(`Authority=${localSigning.identity}`)) {
      throw new Error(`macOS bundle is not signed by ${localSigning.identity}.`);
    }
    const requirement = runText('/usr/bin/codesign', ['-dr', '-', appPath]).toLowerCase();
    const expectedRoot = localSigning.certificateSha1.toLowerCase();
    if (!requirement.includes(`certificate root = h\"${expectedRoot}\"`)) {
      throw new Error('macOS designated requirement does not contain the expected local signing certificate.');
    }
  }
}

if (process.platform === 'win32') {
  const result = spawnSync(process.execPath, [path.resolve('scripts/build-installer.mjs'), ...args], {
    stdio: 'inherit',
    env: toolchainEnv(),
    windowsHide: true,
  });
  process.exit(result.status ?? 1);
}

if (process.platform === 'darwin' && process.arch === 'arm64') {
  if (args.includes('--publish')) {
    throw new Error('macOS publish signing/notarization is not configured yet. Use desktop:installer for a local M1 build.');
  }
  const cli = path.resolve('node_modules', '@tauri-apps', 'cli', 'tauri.js');
  const localSigning = loadLocalMacSigning();
  const tauriArgs = [cli, 'build', '--bundles', 'app,dmg'];
  let buildEnv = toolchainEnv();
  if (localSigning) {
    tauriArgs.push('--config', JSON.stringify({ bundle: { macOS: { signingIdentity: localSigning.identity } } }));
    buildEnv = toolchainEnv({}, localSigning.home);


    console.log(`Using local macOS signing identity: ${localSigning.identity}`);
  }
  const result = spawnSync(process.execPath, tauriArgs, {
    stdio: 'inherit',
    env: buildEnv,
  });
  if ((result.status ?? 1) !== 0) process.exit(result.status ?? 1);

  const appPath = path.resolve('src-tauri', 'target', 'release', 'bundle', 'macos', 'ChatX.app');
  verifyMacBundle(appPath, localSigning);
  console.log('macOS bundle identity verified:', MAC_BUNDLE_ID);
  process.exit(0);
}

throw new Error(`ChatX packaging is not configured for ${process.platform}-${process.arch}.`);
