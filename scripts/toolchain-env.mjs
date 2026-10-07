import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { resolveWindowsHostPath } from './desktop-host-environment.mjs';

export const realHome =
  process.env.CHATX_REAL_HOME?.trim() || os.userInfo().homedir;
export const inheritedHome =
  process.env.HOME || process.env.USERPROFILE || '';
export const isWindows = process.platform === 'win32';
export const realHostPath = isWindows
  ? resolveWindowsHostPath(process.env)
  : (process.env.PATH || '');

export function locateExecutable(name, env = process.env) {
  if (isWindows) {
    const result = spawnSync('where.exe', [name], {
      encoding: 'utf8',
      windowsHide: true,
      env: { ...env, PATH: realHostPath },
    });
    if (result.status !== 0) return null;
    const candidate = result.stdout.split(/\r?\n/).map((value) => value.trim()).find(Boolean);
    return candidate && fs.existsSync(candidate) ? candidate : null;
  }
  const result = spawnSync('/usr/bin/env', ['sh', '-lc', `command -v ${name}`], {
    encoding: 'utf8',
    env,
  });
  if (result.status !== 0) return null;
  const candidate = result.stdout.trim().split(/\r?\n/)[0];
  return candidate && fs.existsSync(candidate) ? candidate : null;
}

export function toolchainEnv(extra = {}, home = realHome) {
  const cargoHome = path.join(home, '.cargo');
  const rustupHome = path.join(home, '.rustup');
  const cargoBin = path.join(cargoHome, 'bin');
  return {
    ...process.env,
    HOME: home,
    USERPROFILE: home,
    CARGO_HOME: cargoHome,
    RUSTUP_HOME: rustupHome,
    PATH: [cargoBin, realHostPath]
      .filter(Boolean)
      .join(path.delimiter),
    ...extra,
  };
}
