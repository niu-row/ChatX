import os from 'node:os';
import path from 'node:path';

export function resolveRealAccountHome(env = process.env, userInfo = os.userInfo()) {
  return path.resolve(env.CHATX_REAL_HOME?.trim() || userInfo.homedir);
}

function setDefault(env, name, value) {
  if (!env[name] && value) env[name] = value;
}

export function restoreSafeHostEnvironment({
  env = process.env,
  realHome = resolveRealAccountHome(env),
  platform = process.platform,
  arch = process.arch,
  userInfo = os.userInfo(),
  hostname = os.hostname(),
  cpuCount = os.cpus().length,
} = {}) {
  if (platform !== 'win32') return env;

  const systemRoot = env.SystemRoot || env.WINDIR || 'C:\\Windows';
  const root = path.win32.parse(systemRoot).root || 'C:\\';
  const systemDrive = env.SystemDrive || root.replace(/[\\/]$/, '') || 'C:';
  const programFiles = env.ProgramFiles || `${systemDrive}\\Program Files`;
  const programFilesX86 = env['ProgramFiles(x86)']
    || (arch === 'x64' ? `${systemDrive}\\Program Files (x86)` : programFiles);
  const usersRoot = path.win32.dirname(realHome);

  const defaults = {
    ProgramData: `${systemDrive}\\ProgramData`,
    ALLUSERSPROFILE: `${systemDrive}\\ProgramData`,
    COMPUTERNAME: hostname,
    OS: 'Windows_NT',
    PUBLIC: path.win32.join(usersRoot, 'Public'),
    ProgramW6432: programFiles,
    CommonProgramFiles: path.win32.join(programFiles, 'Common Files'),
    'ProgramFiles(x86)': programFilesX86,
    'CommonProgramFiles(x86)': path.win32.join(programFilesX86, 'Common Files'),
    CommonProgramW6432: path.win32.join(programFiles, 'Common Files'),
    NUMBER_OF_PROCESSORS: String(Math.max(1, cpuCount)),
    USERNAME: userInfo.username,
  };
  for (const [name, value] of Object.entries(defaults)) setDefault(env, name, value);
  return env;
}
