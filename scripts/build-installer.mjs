import fs from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import path from 'node:path';

if (process.platform !== 'win32') {
  throw new Error('The NSIS installer build is configured for Windows.');
}

const args = new Set(process.argv.slice(2));
const publish = args.has('--publish');
const requireSigning = publish || args.has('--require-signing') || process.env.CHATX_REQUIRE_SIGNING === '1';
const certificateThumbprint = process.env.CHATX_WINDOWS_CERT_THUMBPRINT?.replace(/\s+/g, '') || '';
const timestampUrl = process.env.CHATX_WINDOWS_TIMESTAMP_URL?.trim() || '';
const tauriCli = path.resolve('node_modules', '@tauri-apps', 'cli', 'tauri.js');
const maxAttempts = 3;
const retryableLock = /(?:os error 32|another program is using this file|being used by another process|process cannot access the file|另一个程序正在使用此文件|文件.*(?:占用|使用))/i;
const signingConfig = path.resolve('src-tauri', '.chatx-signing.conf.json');

if (requireSigning && !certificateThumbprint) {
  throw new Error(
    publish
      ? 'Publishing a ChatX release requires CHATX_WINDOWS_CERT_THUMBPRINT. Import the Authenticode certificate first.'
      : 'Signed release requires CHATX_WINDOWS_CERT_THUMBPRINT. Import the Authenticode certificate first.',
  );
}
if (certificateThumbprint && !timestampUrl) {
  throw new Error('CHATX_WINDOWS_TIMESTAMP_URL is required when Authenticode signing is enabled.');
}

if (certificateThumbprint) {
  fs.writeFileSync(signingConfig, `${JSON.stringify({
    bundle: {
      windows: {
        certificateThumbprint,
        digestAlgorithm: 'sha256',
        timestampUrl,
      },
    },
  }, null, 2)}\n`, 'utf8');
}

function runBundle() {
  return new Promise((resolve, reject) => {
    const cliArgs = [tauriCli, 'build', '--bundles', 'nsis'];
    if (certificateThumbprint) cliArgs.push('--config', signingConfig);
    const child = spawn(process.execPath, cliArgs, {
      cwd: process.cwd(),
      env: process.env,
      windowsHide: true,
      stdio: ['inherit', 'pipe', 'pipe'],
    });
    let output = '';
    const append = (stream, chunk) => {
      stream.write(chunk);
      output += chunk.toString('utf8');
      if (output.length > 500_000) output = output.slice(-500_000);
    };
    child.stdout.on('data', (chunk) => append(process.stdout, chunk));
    child.stderr.on('data', (chunk) => append(process.stderr, chunk));
    child.once('error', reject);
    child.once('close', (code, signal) => resolve({ code, signal, output }));
  });
}

function verifyAuthenticode(file) {
  const command = `$s=Get-AuthenticodeSignature -LiteralPath $env:CHATX_SIGNED_FILE; if($s.Status -ne 'Valid'){Write-Error ('Invalid Authenticode signature: '+$s.Status+' '+$s.StatusMessage); exit 1}; Write-Output $s.SignerCertificate.Thumbprint`;
  const result = spawnSync('powershell.exe', ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', command], {
    env: { ...process.env, CHATX_SIGNED_FILE: file },
    encoding: 'utf8',
    windowsHide: true,
  });
  if (result.status !== 0) throw new Error(result.stderr || result.stdout || `Signature verification failed: ${file}`);
  return result.stdout.trim();
}

let lastResult = null;
try {
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    lastResult = await runBundle();
    if (lastResult.code === 0) break;
    const retryable = retryableLock.test(lastResult.output);
    if (!retryable || attempt === maxAttempts) break;
    const delayMs = attempt * 1_500;
    console.error(`[chatx] Tauri/NSIS hit a transient Windows file lock; retrying bundle (${attempt + 1}/${maxAttempts}).`);
    await new Promise((resolve) => setTimeout(resolve, delayMs));
  }

  if (lastResult?.code !== 0) {
    const signal = lastResult?.signal ? ` (signal ${lastResult.signal})` : '';
    throw new Error(`Tauri NSIS bundle failed with exit code ${lastResult?.code ?? 'unknown'}${signal}.`);
  }

  const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));
  const appExe = path.resolve('src-tauri', 'target', 'release', 'chatx-desktop.exe');
  const installer = path.resolve('src-tauri', 'target', 'release', 'bundle', 'nsis', `ChatX_${pkg.version}_x64-setup.exe`);
  if (!fs.existsSync(installer)) throw new Error(`Expected NSIS installer was not created: ${installer}`);

  if (requireSigning) {
    const appSigner = verifyAuthenticode(appExe);
    const installerSigner = verifyAuthenticode(installer);
    console.log(`[chatx] Authenticode valid: app=${appSigner}, installer=${installerSigner}`);
  } else {
    console.warn('[chatx] installer is unsigned; Windows may show Unknown Publisher / SmartScreen warnings.');
  }

  if (publish) {
    const releaseDir = path.resolve('release');
    fs.mkdirSync(releaseDir, { recursive: true });
    const destination = path.join(releaseDir, `ChatX-Setup-${pkg.version}.exe`);
    for (const entry of fs.readdirSync(releaseDir)) {
      if (/^ChatX-Setup-.*\.exe$/i.test(entry)) fs.rmSync(path.join(releaseDir, entry), { force: true });
    }
    fs.copyFileSync(installer, destination);
    console.log(`[chatx] published signed installer: ${destination}`);
  }
} finally {
  fs.rmSync(signingConfig, { force: true });
}
