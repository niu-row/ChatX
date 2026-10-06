import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(scriptDir, '..');
const realHome = process.env.CHATX_REAL_HOME?.trim() || os.userInfo().homedir;
const inheritedHome = process.env.HOME || process.env.USERPROFILE || '';
const isWindows = process.platform === 'win32';

function exists(file) {
  try { return fs.existsSync(file); } catch { return false; }
}

function executable(file) {
  if (!exists(file)) return false;
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return true;
  } catch {
    return isWindows && exists(file);
  }
}

function firstExisting(candidates, predicate = exists) {
  return candidates.find((candidate) => candidate && predicate(candidate)) || null;
}
function rustTool(name) {
  const exe = isWindows ? `${name}.exe` : name;
  const direct = firstExisting([
    process.env[`CHATX_${name.toUpperCase()}_PATH`],
    path.join(realHome, '.cargo', 'bin', exe),
  ], executable);
  if (direct) return direct;

  const toolchains = path.join(realHome, '.rustup', 'toolchains');
  if (!exists(toolchains)) return null;
  for (const entry of fs.readdirSync(toolchains)) {
    const candidate = path.join(toolchains, entry, 'bin', exe);
    if (executable(candidate)) return candidate;
  }
  return null;
}

function javaHome() {
  const candidates = [
    process.env.CHATX_JAVA_HOME,
    process.env.JAVA_HOME,
    '/Applications/Android Studio.app/Contents/jbr/Contents/Home',
    path.join(realHome, 'Applications', 'Android Studio.app', 'Contents', 'jbr', 'Contents', 'Home'),
    isWindows ? path.join(process.env.ProgramFiles || 'C:\\Program Files', 'Android', 'Android Studio', 'jbr') : '',
  ];
  return firstExisting(candidates, (home) => executable(path.join(home, 'bin', isWindows ? 'java.exe' : 'java')));
}
function androidSdk() {
  const localAppData = process.env.LOCALAPPDATA || '';
  return firstExisting([
    process.env.CHATX_ANDROID_HOME,
    process.env.ANDROID_HOME,
    process.env.ANDROID_SDK_ROOT,
    path.join(realHome, 'Library', 'Android', 'sdk'),
    localAppData ? path.join(localAppData, 'Android', 'Sdk') : '',
  ]);
}

function wrapperVersion() {
  const file = path.join(root, 'android-monitor', 'gradle', 'wrapper', 'gradle-wrapper.properties');
  if (!exists(file)) return null;
  const text = fs.readFileSync(file, 'utf8');
  return text.match(/gradle-([0-9.]+)-bin\.zip/)?.[1] || null;
}

function scanForGradle(dir, version) {
  if (!exists(dir)) return null;
  const wanted = isWindows ? 'gradle.bat' : 'gradle';
  const queue = [{ dir, depth: 0 }];
  while (queue.length) {
    const item = queue.shift();
    if (item.depth > 4) continue;
    for (const entry of fs.readdirSync(item.dir, { withFileTypes: true })) {
      const full = path.join(item.dir, entry.name);
      if (entry.isDirectory()) queue.push({ dir: full, depth: item.depth + 1 });
      else if (entry.name === wanted && full.includes(`gradle-${version}`) && executable(full)) return full;
    }
  }
  return null;
}
function cachedGradle(version) {
  if (!version) return null;
  const dist = path.join(realHome, '.gradle', 'wrapper', 'dists', `gradle-${version}-bin`);
  const managed = path.join(realHome, '.gradle', 'chatx-toolchains', `gradle-${version}`);
  return scanForGradle(managed, version) || scanForGradle(dist, version);
}

function cachedGradleZip(version) {
  if (!version) return null;
  return firstExisting([
    path.join(realHome, '.gradle', 'wrapper', 'dists', `gradle-${version}-bin`, 'manual', `gradle-${version}-bin.zip`),
  ]);
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd || root,
    env: options.env || process.env,
    stdio: options.stdio || 'inherit',
    encoding: options.encoding,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
  return result;
}

function baseEnv(extra = {}) {
  const cargoDir = path.join(realHome, '.cargo', 'bin');
  const sep = path.delimiter;
  return {
    ...process.env,
    HOME: realHome,
    USERPROFILE: realHome,
    PATH: [cargoDir, process.env.PATH || ''].filter(Boolean).join(sep),
    ...extra,
  };
}
function ensureGradle(version) {
  let gradle = cachedGradle(version);
  if (gradle) return gradle;
  const archive = cachedGradleZip(version);
  if (!archive) return null;

  const destination = path.join(realHome, '.gradle', 'chatx-toolchains', `gradle-${version}`);
  fs.mkdirSync(destination, { recursive: true });
  if (isWindows) {
    run('powershell.exe', [
      '-NoProfile', '-NonInteractive', '-Command',
      'Expand-Archive -LiteralPath $env:CHATX_GRADLE_ZIP -DestinationPath $env:CHATX_GRADLE_DEST -Force',
    ], { env: baseEnv({ CHATX_GRADLE_ZIP: archive, CHATX_GRADLE_DEST: destination }) });
  } else {
    run('/usr/bin/unzip', ['-q', '-o', archive, '-d', destination], { env: baseEnv() });
  }
  gradle = cachedGradle(version);
  return gradle;
}

function printDoctor() {
  const version = wrapperVersion();
  const cargo = rustTool('cargo');
  const rustc = rustTool('rustc');
  const jdk = javaHome();
  const sdk = androidSdk();
  const gradle = cachedGradle(version);
  console.log(JSON.stringify({
    inheritedHome,
    realHome,
    homeWasIsolated: inheritedHome !== realHome,
    cargo,
    rustc,
    javaHome: jdk,
    androidSdk: sdk,
    gradleVersion: version,
    gradle,
    cachedGradleZip: cachedGradleZip(version),
  }, null, 2));
}
function runCargoManifest(mode, manifestPath) {
  const cargo = rustTool('cargo');
  if (!cargo) throw new Error(`Rust cargo not found under real home: ${realHome}`);
  run(cargo, [mode, '--manifest-path', manifestPath], { env: baseEnv() });
}

function runRust(mode) {
  runCargoManifest(mode, 'src-tauri/Cargo.toml');
}

function runRelayTests() {
  runCargoManifest('test', 'relay-protocol/Cargo.toml');
  runCargoManifest('test', 'relay-server/Cargo.toml');
}

function runBatch(command, args, options = {}) {
  run('cmd.exe', ['/d', '/c', 'call', command, ...args], options);
}

function runAndroidTests() {
  const jdk = javaHome();
  const sdk = androidSdk();
  if (!jdk) throw new Error('Android Studio JDK / JAVA_HOME not found.');
  if (!sdk) throw new Error(`Android SDK not found under real home: ${realHome}`);
  const version = wrapperVersion();
  const gradle = ensureGradle(version);
  const env = baseEnv({ JAVA_HOME: jdk, ANDROID_HOME: sdk, ANDROID_SDK_ROOT: sdk });
  if (gradle) {
    if (isWindows && gradle.toLowerCase().endsWith('.bat')) {
      runBatch(gradle, ['lintDebug', 'test', '--no-daemon'], { cwd: path.join(root, 'android-monitor'), env });
    } else {
      run(gradle, ['lintDebug', 'test', '--no-daemon'], { cwd: path.join(root, 'android-monitor'), env });
    }
    return;
  }
  const wrapper = path.join(root, 'android-monitor', isWindows ? 'gradlew.bat' : 'gradlew');
  const options = { cwd: path.join(root, 'android-monitor'), env };
  if (isWindows) runBatch(wrapper, ['lintDebug', 'test', '--no-daemon'], options);
  else run(wrapper, ['lintDebug', 'test', '--no-daemon'], options);
}

function runNpmTests() {
  const npmExecPath = process.env.npm_execpath?.trim();
  if (npmExecPath && exists(npmExecPath)) {
    run(process.execPath, [npmExecPath, 'test'], { env: baseEnv() });
    return;
  }
  if (isWindows) {
    run('cmd.exe', ['/d', '/s', '/c', 'npm test'], { env: baseEnv() });
    return;
  }
  run('npm', ['test'], { env: baseEnv() });
}

function runGitDiffCheck() {
  run('git', ['diff', '--check'], { env: baseEnv() });
}
const command = process.argv[2] || 'doctor';

try {
  switch (command) {
    case 'doctor':
      printDoctor();
      break;
    case 'rust-check':
      runRust('check');
      break;
    case 'rust-test':
      runRust('test');
      break;
    case 'android-test':
      runAndroidTests();
      break;
    case 'relay-test':
      runRelayTests();
      break;
    case 'local-full':
      runNpmTests();
      runRust('test');
      runRelayTests();
      runAndroidTests();
      runGitDiffCheck();
      break;
    default:
      throw new Error(`Unknown dev-toolchain command: ${command}`);
  }
} catch (error) {
  console.error(`dev-toolchain: ${error?.message || error}`);
  process.exit(1);
}
