import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { settingsDirectoryPath } from '../settings.js';

const FILE_NAME = 'runtime-key.dpapi';

export type CredentialStoreInfo = {
  supported: boolean;
  provider: 'windows-dpapi' | 'none';
  saved: boolean;
  path: string | null;
};

function credentialPath(): string {
  return path.join(settingsDirectoryPath(), FILE_NAME);
}

function powershell(script: string, input = ''): string {
  const result = spawnSync(
    'powershell.exe',
    ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', script],
    {
      input,
      encoding: 'utf8',
      windowsHide: true,
      timeout: 10_000,
      maxBuffer: 1024 * 1024,
    },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const message = (result.stderr || result.stdout || `PowerShell exited with code ${result.status}`).trim();
    throw new Error(`Windows DPAPI operation failed: ${message}`);
  }
  return result.stdout.trim();
}

export function credentialStoreInfo(): CredentialStoreInfo {
  const supported = process.platform === 'win32';
  const file = credentialPath();
  return {
    supported,
    provider: supported ? 'windows-dpapi' : 'none',
    saved: supported && fs.existsSync(file),
    path: supported ? file : null,
  };
}

export function saveRuntimeKey(apiKey: string): void {
  if (process.platform !== 'win32') {
    throw new Error('Secure Runtime Key persistence is currently supported only on Windows via DPAPI.');
  }
  const value = apiKey.trim();
  if (!value) throw new Error('Runtime API Key is empty.');
  const script = [
    'Add-Type -AssemblyName System.Security',
    '$plain = [Console]::In.ReadToEnd()',
    '$bytes = [Text.Encoding]::UTF8.GetBytes($plain)',
    '$protected = [Security.Cryptography.ProtectedData]::Protect($bytes, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)',
    '[Console]::Out.Write([Convert]::ToBase64String($protected))',
  ].join('; ');
  const encrypted = powershell(script, value);
  const file = credentialPath();
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${encrypted}\n`, { encoding: 'utf8', mode: 0o600 });
}

export function loadRuntimeKey(): string | null {
  if (process.platform !== 'win32') return null;
  const file = credentialPath();
  if (!fs.existsSync(file)) return null;
  const encoded = fs.readFileSync(file, 'utf8').trim();
  if (!encoded) return null;
  const script = [
    'Add-Type -AssemblyName System.Security',
    '$encoded = [Console]::In.ReadToEnd().Trim()',
    '$protected = [Convert]::FromBase64String($encoded)',
    '$bytes = [Security.Cryptography.ProtectedData]::Unprotect($protected, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)',
    '[Console]::Out.Write([Text.Encoding]::UTF8.GetString($bytes))',
  ].join('; ');
  const value = powershell(script, encoded).trim();
  return value || null;
}

export function clearRuntimeKey(): boolean {
  if (process.platform !== 'win32') return false;
  const file = credentialPath();
  if (!fs.existsSync(file)) return false;
  fs.rmSync(file, { force: true });
  return true;
}
