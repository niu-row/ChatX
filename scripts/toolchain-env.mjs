import os from 'node:os';
import path from 'node:path';

export const realHome =
  process.env.CHATX_REAL_HOME?.trim() || os.userInfo().homedir;
export const inheritedHome =
  process.env.HOME || process.env.USERPROFILE || '';
export const isWindows = process.platform === 'win32';

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
    PATH: [cargoBin, process.env.PATH || '']
      .filter(Boolean)
      .join(path.delimiter),
    ...extra,
  };
}
