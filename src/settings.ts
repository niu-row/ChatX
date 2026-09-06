import fs from 'node:fs';
import path from 'node:path';
import { config } from './config.js';

export const SETTINGS_VERSION = 2 as const;

export type PermissionSettings = {
  filesystemRead: boolean;
  filesystemWrite: boolean;
  gitRead: boolean;
  gitWrite: boolean;
  gitAdvanced: boolean;
  shell: boolean;
  fullAccess: boolean;
};

export type PermissionPreset = 'safe' | 'developer' | 'unrestricted' | 'custom';

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
    gitRead: true,
    gitWrite: false,
    gitAdvanced: false,
    shell: false,
    fullAccess: false,
  },
  developer: {
    filesystemRead: true,
    filesystemWrite: true,
    gitRead: true,
    gitWrite: true,
    gitAdvanced: false,
    shell: false,
    fullAccess: false,
  },
  unrestricted: {
    filesystemRead: true,
    filesystemWrite: true,
    gitRead: true,
    gitWrite: true,
    gitAdvanced: true,
    shell: true,
    fullAccess: true,
  },
};

const legacyCompatiblePermissions: PermissionSettings = {
  filesystemRead: true,
  filesystemWrite: true,
  gitRead: true,
  gitWrite: true,
  gitAdvanced: false,
  shell: config.enableShell,
  fullAccess: config.fullAccess,
};

function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === 'boolean' ? value : fallback;
}

function normalizeRoots(values: unknown): string[] {
  const roots = Array.isArray(values)
    ? values
        .filter((value): value is string => typeof value === 'string' && value.trim().length > 0)
        .map((value) => path.resolve(value.trim()))
    : [];
  const seen = new Set<string>();
  const normalized: string[] = [];
  for (const root of roots) {
    const key = process.platform === 'win32' ? root.toLowerCase() : root;
    if (seen.has(key)) continue;
    seen.add(key);
    normalized.push(root);
  }
  return normalized.length > 0 ? normalized : config.roots.map((root) => path.resolve(root));
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
  permissionPreset: inferPreset(legacyCompatiblePermissions),
  permissions: legacyCompatiblePermissions,
  filesystem: {
    roots: normalizeRoots(config.roots),
  },
  connection: {
    tunnelId: null,
  },
};

function parsePermissions(value: unknown): PermissionSettings {
  const p = value && typeof value === 'object' ? (value as Partial<PermissionSettings>) : {};
  return {
    filesystemRead: bool(p.filesystemRead, defaults.permissions.filesystemRead),
    filesystemWrite: bool(p.filesystemWrite, defaults.permissions.filesystemWrite),
    gitRead: bool(p.gitRead, defaults.permissions.gitRead),
    gitWrite: bool(p.gitWrite, defaults.permissions.gitWrite),
    gitAdvanced: bool(p.gitAdvanced, defaults.permissions.gitAdvanced),
    shell: bool(p.shell, defaults.permissions.shell),
    fullAccess: bool(p.fullAccess, defaults.permissions.fullAccess),
  };
}

function parseTunnelId(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value.trim() : null;
}

function load(): { settings: RuntimeSettings; migrated: boolean } {
  try {
    if (!fs.existsSync(SETTINGS_FILE)) return { settings: structuredClone(defaults), migrated: false };
    const parsed = JSON.parse(fs.readFileSync(SETTINGS_FILE, 'utf8')) as Record<string, unknown>;
    const permissions = parsePermissions(parsed.permissions);
    const version = typeof parsed.version === 'number' ? parsed.version : 1;
    const filesystem = parsed.filesystem && typeof parsed.filesystem === 'object'
      ? (parsed.filesystem as { roots?: unknown })
      : undefined;
    const connection = parsed.connection && typeof parsed.connection === 'object'
      ? (parsed.connection as { tunnelId?: unknown })
      : undefined;
    const requestedPreset = parsed.permissionPreset;
    const permissionPreset: PermissionPreset =
      requestedPreset === 'safe' || requestedPreset === 'developer' || requestedPreset === 'unrestricted'
        ? (permissionsEqual(permissions, permissionPresets[requestedPreset]) ? requestedPreset : 'custom')
        : inferPreset(permissions);

    return {
      settings: {
        version: SETTINGS_VERSION,
        permissionPreset,
        permissions,
        filesystem: {
          roots: normalizeRoots(filesystem?.roots ?? parsed.roots ?? config.roots),
        },
        connection: {
          tunnelId: parseTunnelId(connection?.tunnelId),
        },
      },
      migrated: version !== SETTINGS_VERSION || !filesystem || !('permissionPreset' in parsed),
    };
  } catch (error) {
    console.error('[chatx] failed to load settings:', error);
    return { settings: structuredClone(defaults), migrated: false };
  }
}

let loaded = load();
let current = loaded.settings;

function save(): void {
  fs.mkdirSync(SETTINGS_DIR, { recursive: true });
  const temp = `${SETTINGS_FILE}.tmp`;
  fs.writeFileSync(temp, `${JSON.stringify(current, null, 2)}\n`, { encoding: 'utf8', mode: 0o600 });
  fs.renameSync(temp, SETTINGS_FILE);
}

if (loaded.migrated) {
  try {
    save();
    console.error(`[chatx] migrated settings to version ${SETTINGS_VERSION}: ${SETTINGS_FILE}`);
  } catch (error) {
    console.error('[chatx] failed to persist migrated settings:', error);
  }
}
loaded = { settings: current, migrated: false };

export function getRuntimeSettings(): RuntimeSettings {
  return structuredClone(current);
}

export function updatePermissions(patch: Partial<PermissionSettings>): RuntimeSettings {
  const next = { ...current.permissions };
  for (const key of Object.keys(next) as Array<keyof PermissionSettings>) {
    const value = patch[key];
    if (typeof value === 'boolean') next[key] = value;
  }
  current = { ...current, permissions: next, permissionPreset: inferPreset(next) };
  save();
  return getRuntimeSettings();
}

export function applyPermissionPreset(preset: Exclude<PermissionPreset, 'custom'>): RuntimeSettings {
  const permissions = structuredClone(permissionPresets[preset]);
  current = { ...current, permissionPreset: preset, permissions };
  save();
  return getRuntimeSettings();
}

export function updateAllowedRoots(roots: string[]): RuntimeSettings {
  current = {
    ...current,
    filesystem: {
      roots: normalizeRoots(roots),
    },
  };
  save();
  return getRuntimeSettings();
}

export function requirePermission(permission: keyof PermissionSettings, label: string): void {
  if (!current.permissions[permission]) {
    throw new Error(`${label} is disabled in the ChatX local console.`);
  }
}

export function saveTunnelId(tunnelId: string): RuntimeSettings {
  current = { ...current, connection: { tunnelId: tunnelId.trim() || null } };
  save();
  return getRuntimeSettings();
}

export function settingsFilePath(): string {
  return SETTINGS_FILE;
}

export function settingsDirectoryPath(): string {
  return SETTINGS_DIR;
}
