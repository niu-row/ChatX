import { spawn } from 'node:child_process';
import path from 'node:path';

if (process.platform !== 'win32') {
  throw new Error('The NSIS installer build is configured for Windows.');
}

const tauriCli = path.resolve('node_modules', '@tauri-apps', 'cli', 'tauri.js');
const maxAttempts = 3;
const retryableLock = /(?:os error 32|another program is using this file|being used by another process|process cannot access the file|另一个程序正在使用此文件|文件.*(?:占用|使用))/i;

function runBundle() {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [tauriCli, 'build', '--bundles', 'nsis'], {
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

let lastResult = null;
for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
  lastResult = await runBundle();
  if (lastResult.code === 0) process.exit(0);
  const retryable = retryableLock.test(lastResult.output);
  if (!retryable || attempt === maxAttempts) break;
  const delayMs = attempt * 1_500;
  console.error(`[chatx] Tauri/NSIS hit a transient Windows file lock; retrying bundle (${attempt + 1}/${maxAttempts}).`);
  await new Promise((resolve) => setTimeout(resolve, delayMs));
}

const signal = lastResult?.signal ? ` (signal ${lastResult.signal})` : '';
throw new Error(`Tauri NSIS bundle failed with exit code ${lastResult?.code ?? 'unknown'}${signal}.`);
