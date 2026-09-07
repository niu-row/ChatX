import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { config } from './config.js';

export const SETTINGS_VERSION = 3 as const;

export type PermissionSettings = {
  filesystemRead: boolean;
  filesystemWrite: boolean;
  filesystemDestructive: boolean;
  gitRead: boolean;
  gitWrite: boolean;
  gitAdvanced: boolean;
  shell: boolean;
  fullAccess: boolean;
};

export type PermissionPreset = 'safe' | 'developer' | 'unrestricted' | 'custom';

export const permissionMetadata: ReadonlyArray<Readonly<{
  key: keyof PermissionSettings;
  title: string;
  description: string;
  highRisk: boolean;
}>> = [
  { key: 'filesystemRead', title: '读取文件', description: '目录列表、读取、搜索和元数据', highRisk: false },
  { key: 'filesystemWrite', title: '修改文件', description: '写入、编辑、复制和创建目录', highRisk: false },
  { key: 'filesystemDestructive', title: '删除/移动', description: '删除或移动文件与目录；开发模式默认关闭', highRisk: true },
  { key: 'gitRead', title: 'Git 读取', description: 'status、diff、log', highRisk: false },
  { key: 'gitWrite', title: 'Git 写入', description: '受约束的 stage、unstage、branch、commit', highRisk: false },
  { key: 'gitAdvanced', title: '高级 Git', description: '任意 git 参数；等同 Shell，需同时开启 Shell', highRisk: true },
  { key: 'shell', title: 'Shell 命令', description: '高权限；不受允许目录边界约束', highRisk: true },
  { key: 'fullAccess', title: '完整文件系统访问', description: '绕过允许目录边界', highRisk: true },
];

export type RuntimeSettings = {
  version: typeof SETTINGS_VERSION;
  permissionPreset: PermissionPreset;
  permissions: PermissionSettings;
  filesystem: {
    roots: string[];
  };
  connection: {
    tunnelId: string | null;
  };
};

const SETTINGS_DIR = config.settingsDir;
const SETTINGS_FILE = path.join(SETTINGS_DIR, 'settings.json');

export const permissionPresets: Record<Exclude<PermissionPreset, 'custom'>, PermissionSettings> = {
  safe: {
    filesystemRead: true,
    filesystemWrite: false,
    filesystemDestructive: false,
    gitRead: true,
    gitWrite: false,
    gitAdvanced: false,
    shell: false,
    fullAccess: false,
  },
  developer: {
    filesystemRead: true,
    filesystemWrite: true,
    filesystemDestructive: false,
    gitRead: true,
    gitWrite: true,
    gitAdvanced: false,
    shell: false,
    fullAccess: false,
  },
  unrestricted: {
    filesystemRead: true,
    filesystemWrite: true,
    filesystemDestructive: true,
    gitRead: true,
    gitWrite: true,
    gitAdvanced: true,
    shell: true,
    fullAccess: true,
  },
};

const initialPermissions: PermissionSettings = {
  ...permissionPresets.safe,
  shell: config.enableShell,
  fullAccess: config.fullAccess,
};

function normalizeRoots(values: unknown): string[] {
  if (!Array.isArray(values) || values.length === 0 ||
      values.some(value => typeof value !== 'string' || !value.trim() || value.includes('\0'))) {
    throw new Error('At least one valid allowed directory is required.');
  }
  const seen = new Set<string>();
  return values.map(value => path.resolve(value.trim())).filter(root => {
    const key = process.platform === 'win32' ? root.toLowerCase() : root;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function permissionsEqual(a: PermissionSettings, b: PermissionSettings): boolean {
  return (Object.keys(a) as Array<keyof PermissionSettings>).every((key) => a[key] === b[key]);
}

function inferPreset(permissions: PermissionSettings): PermissionPreset {
  for (const [name, preset] of Object.entries(permissionPresets) as Array<[
    Exclude<PermissionPreset, 'custom'>,
    PermissionSettings,
  ]>) {
    if (permissionsEqual(permissions, preset)) return name;
  }
  return 'custom';
}

const defaults: RuntimeSettings = {
  version: SETTINGS_VERSION,
  permissionPreset: inferPreset(initialPermissions),
  permissions: initialPermissions,
  filesystem: {
    roots: normalizeRoots(config.roots),
  },
  connection: {
    tunnelId: null,
  },
};

function parsePermissions(value: unknown, sourceVersion: number): PermissionSettings {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid permissions.');
  const input = value as Record<string, unknown>;
  const result = { ...permissionPresets.safe };
  for (const key of Object.keys(result) as Array<keyof PermissionSettings>) {
    if (input[key] === undefined) {
      if (sourceVersion === 1) continue;
      if (sourceVersion === 2 && key === 'filesystemDestructive') {
        // Preserve prior "fully unrestricted" behavior while keeping ordinary
        // developer/file-write configurations on the new safer default.
        result.filesystemDestructive = input.filesystemWrite === true && input.fullAccess === true;
        continue;
      }
      throw new Error('Invalid permission: ' + key);
    }
    if (typeof input[key] !== 'boolean') throw new Error('Invalid permission: ' + key);
    result[key] = input[key];
  }
  return result;
}

function load(): { settings: RuntimeSettings; migrated: boolean } {
  try {
    if (!fs.existsSync(SETTINGS_FILE)) return { settings: structuredClone(defaults), migrated: true };
    const parsed: unknown = JSON.parse(fs.readFileSync(SETTINGS_FILE, 'utf8'));
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('Invalid settings.');
    const record = parsed as Record<string, unknown>;
    const version = record.version ?? 1;
    if (version !== 1 && version !== 2 && version !== SETTINGS_VERSION) throw new Error('Unsupported settings version.');
    const legacy = version === 1;
    const permissions = parsePermissions(record.permissions, Number(version));
    const filesystem = record.filesystem as { roots?: unknown } | undefined;
    const connection = record.connection as { tunnelId?: unknown } | undefined;
    if (!legacy && (!filesystem || typeof filesystem !== 'object' ||
        !connection || typeof connection !== 'object')) throw new Error('Invalid settings structure.');
    const tunnelId = connection?.tunnelId ?? null;
    if (tunnelId !== null && typeof tunnelId !== 'string') throw new Error('Invalid Tunnel ID.');
    return {
      settings: {
        version: SETTINGS_VERSION,
        permissionPreset: inferPreset(permissions),
        permissions,
        filesystem: { roots: normalizeRoots(filesystem?.roots ?? (legacy ? record.roots ?? config.roots : undefined)) },
        connection: { tunnelId: typeof tunnelId === 'string' ? tunnelId.trim() || null : null },
      },
      migrated: version !== SETTINGS_VERSION,
    };
  } catch (error) {
    console.error('[chatx] failed to load settings; all operation permissions disabled:', error);
    const settings = structuredClone(defaults);
    for (const key of Object.keys(settings.permissions) as Array<keyof PermissionSettings>) {
      settings.permissions[key] = false;
    }
    settings.permissionPreset = 'custom';
    // Preserve the damaged file for recovery instead of overwriting it with defaults.
    return { settings, migrated: false };
  }
}

let loaded = load();
let current = loaded.settings;
let pathPolicyRevision = 0;
const permissionListeners = new Set<() => void>();

function notifyPermissionListeners(): void {
  for (const listener of permissionListeners) {
    try {
      listener();
    } catch (error) {
      console.error('[chatx] permission listener failed:', error);
    }
  }
}

export function onPermissionSettingsChanged(listener: () => void): () => void {
  permissionListeners.add(listener);
  return () => permissionListeners.delete(listener);
}

function save(settings: RuntimeSettings): void {
  fs.mkdirSync(SETTINGS_DIR, { recursive: true });
  const temp = SETTINGS_FILE + '.' + randomUUID() + '.tmp';
  try {
    fs.writeFileSync(temp, JSON.stringify(settings, null, 2) + '\n', { encoding: 'utf8', mode: 0o600, flag: 'wx' });
    fs.renameSync(temp, SETTINGS_FILE);
  } finally {
    try { fs.rmSync(temp, { force: true }); } catch { /* Preserve the original save result. */ }
  }
}

if (loaded.migrated) {
  try {
    save(current);
    console.error(`[chatx] migrated settings to version ${SETTINGS_VERSION}: ${SETTINGS_FILE}`);
  } catch (error) {
    console.error('[chatx] failed to persist migrated settings:', error);
  }
}
loaded = { settings: current, migrated: false };

export function getRuntimeSettings(): RuntimeSettings {
  return structuredClone(current);
}

export function getPathPolicySettings(): { fullAccess: boolean; roots: string[]; revision: number } {
  return {
    fullAccess: current.permissions.fullAccess,
    roots: [...current.filesystem.roots],
    revision: pathPolicyRevision,
  };
}

function commitSettings(next: RuntimeSettings): RuntimeSettings {
  save(next);
  const permissionsChanged = !permissionsEqual(current.permissions, next.permissions);
  current = next;
  pathPolicyRevision += 1;
  if (permissionsChanged) notifyPermissionListeners();
  return getRuntimeSettings();
}

export function updateRuntimeSettings(value: unknown): RuntimeSettings {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid settings update.');
  const patch = value as Record<string, unknown>;
  const permissionKeys = Object.keys(current.permissions) as Array<keyof PermissionSettings>;
  const allowed = new Set<string>(['preset', 'roots', ...permissionKeys]);
  for (const key of Object.keys(patch)) {
    if (!allowed.has(key)) throw new Error('Unknown setting: ' + key);
  }
  const next = structuredClone(current);
  if ('preset' in patch) {
    const preset = patch.preset;
    if (preset !== 'safe' && preset !== 'developer' && preset !== 'unrestricted') {
      throw new Error('Invalid permission preset.');
    }
    next.permissions = structuredClone(permissionPresets[preset]);
  }
  for (const key of permissionKeys) {
    if (!(key in patch)) continue;
    if (typeof patch[key] !== 'boolean') throw new Error('Invalid permission: ' + key);
    next.permissions[key] = patch[key];
  }
  if ('roots' in patch) next.filesystem.roots = normalizeRoots(patch.roots);
  next.permissionPreset = inferPreset(next.permissions);
  return commitSettings(next);
}

export function updatePermissions(patch: Partial<PermissionSettings>): RuntimeSettings {
  return updateRuntimeSettings(patch);
}

export function applyPermissionPreset(preset: Exclude<PermissionPreset, 'custom'>): RuntimeSettings {
  return updateRuntimeSettings({ preset });
}

export function updateAllowedRoots(roots: string[]): RuntimeSettings {
  return updateRuntimeSettings({ roots });
}

export function requirePermission(permission: keyof PermissionSettings, label: string): void {
  if (!current.permissions[permission]) {
    throw new Error(`${label} is disabled in the ChatX local console.`);
  }
}

export function saveTunnelId(tunnelId: string): RuntimeSettings {
  return commitSettings({ ...current, connection: { tunnelId: tunnelId.trim() || null } });
}

export function settingsFilePath(): string {
  return SETTINGS_FILE;
}

export function settingsDirectoryPath(): string {
  return SETTINGS_DIR;
}
