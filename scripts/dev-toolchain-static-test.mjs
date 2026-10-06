import assert from 'node:assert/strict';
import fs from 'node:fs';

const script = fs.readFileSync('scripts/dev-toolchain.mjs', 'utf8');
const toolchainEnv = fs.readFileSync('scripts/toolchain-env.mjs', 'utf8');
const tauriTool = fs.readFileSync('scripts/tauri-tool.mjs', 'utf8');
const packageBuilder = fs.readFileSync('scripts/build-package.mjs', 'utf8');
const installer = fs.readFileSync('scripts/build-installer.mjs', 'utf8');
const prepare = fs.readFileSync('scripts/prepare-desktop-bundle.mjs', 'utf8');
const gradleWrapper = fs.readFileSync('android-monitor/gradle/wrapper/gradle-wrapper.properties', 'utf8');
const agents = fs.readFileSync('AGENTS.md', 'utf8');
const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));

for (const needle of [
  'inheritedHome,',
  'isWindows,',
  'realHome,',
  'toolchainEnv,',
  "run(cargo, [mode, '--locked', '--manifest-path', manifestPath]",
  'Android Studio.app',
  "path.join(realHome, 'Library', 'Android', 'sdk')",
  'cachedGradleZip',
  "gradle.toLowerCase().endsWith('.bat')",
  "run('cmd.exe', ['/d', '/c', 'call', command, ...args]",
  "if (isWindows) runBatch(wrapper",
  "['lintDebug', 'test', '--no-daemon']",
  "path.join(process.env.ProgramFiles || 'C:\\\\Program Files'",
  "run('git', ['diff', '--check']",
  "process.env.npm_execpath?.trim()",
  "run(process.execPath, [npmExecPath, 'test']",
  "run('cmd.exe', ['/d', '/s', '/c', 'npm test']",
  "runCargoManifest('test', 'relay-protocol/Cargo.toml')",
  "runCargoManifest('test', 'relay-server/Cargo.toml')",
  "case 'relay-test'",
  "case 'local-full'",
]) {
  assert.ok(script.includes(needle), `dev-toolchain.mjs is missing: ${needle}`);
}

for (const needle of [
  'os.userInfo().homedir',
  "CARGO_HOME: cargoHome",
  "RUSTUP_HOME: rustupHome",
  "path.join(cargoHome, 'bin')",
]) {
  assert.ok(toolchainEnv.includes(needle), `toolchain-env.mjs is missing: ${needle}`);
}
for (const needle of ['toolchainEnv()', "'@tauri-apps'", "'cli'", "'tauri.js'"]) {
  assert.ok(tauriTool.includes(needle), `tauri-tool.mjs is missing: ${needle}`);
}
assert.ok(packageBuilder.includes('toolchainEnv'), 'build-package.mjs must use shared toolchain env');
assert.ok(installer.includes('toolchainEnv'), 'build-installer.mjs must use shared toolchain env');
assert.ok(prepare.includes("'.chatx-cache', 'runtime'"), 'desktop runtime downloads must use the persistent verified cache');
assert.ok(prepare.includes('validateCachedDownload'), 'desktop runtime cache must verify locked bytes');
assert.match(gradleWrapper, /^distributionSha256Sum=[0-9a-f]{64}$/m, 'Gradle wrapper distribution must be SHA-256 pinned');

for (const needle of [
  'Critical environment rule: inherited HOME may be fake',
  'npm run dev:doctor',
  'npm run test:local-full',
  'Tunnel health semantics',
  'healthy == true',
  'Tunnel proxy semantics',
  'Relay ownership and lifecycle',
]) {
  assert.ok(agents.includes(needle), `AGENTS.md is missing: ${needle}`);
}

for (const name of [
  'dev:doctor',
  'test:rust:check',
  'test:rust',
  'test:android:gradle',
  'test:local-full',
]) {
  assert.ok(pkg.scripts?.[name], `package.json is missing script: ${name}`);
}

console.log('dev toolchain isolation checks passed');
