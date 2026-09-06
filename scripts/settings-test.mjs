import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

const originalCwd = process.cwd();
const tempRoot = await fs.mkdtemp(path.join(os.tmpdir(), 'chatgptx-settings-test-'));
const settingsDir = path.join(tempRoot, '.chatgptx');
const extraRoot = path.join(tempRoot, 'workspace');
await fs.mkdir(settingsDir, { recursive: true });
await fs.mkdir(extraRoot, { recursive: true });

const legacySettings = {
  permissions: {
    filesystemRead: true,
    filesystemWrite: true,
    gitRead: true,
    gitWrite: true,
    shell: true,
    fullAccess: false,
  },
  connection: {
    tunnelId: 'tunnel_12345678',
  },
};
await fs.writeFile(path.join(settingsDir, 'settings.json'), `${JSON.stringify(legacySettings, null, 2)}\n`, 'utf8');

process.env.CHATGPTX_SETTINGS_DIR = settingsDir;
process.env.CHATGPTX_ROOTS = tempRoot;
process.env.CHATGPTX_FULL_ACCESS = 'false';
process.env.CHATGPTX_ENABLE_SHELL = 'true';
process.chdir(tempRoot);

try {
  const settings = await import(new URL('../dist/settings.js', import.meta.url));
  const pathPolicy = await import(new URL('../dist/security/path-policy.js', import.meta.url));
  const credentials = await import(new URL('../dist/security/credential-store.js', import.meta.url));

  const migrated = settings.getRuntimeSettings();
  assert.equal(migrated.version, 2);
  assert.equal(migrated.permissions.gitAdvanced, false);
  assert.equal(migrated.connection.tunnelId, 'tunnel_12345678');
  assert.deepEqual(migrated.filesystem.roots, [path.resolve(tempRoot)]);

  const persisted = JSON.parse(await fs.readFile(path.join(settingsDir, 'settings.json'), 'utf8'));
  assert.equal(persisted.version, 2);
  assert.ok(Array.isArray(persisted.filesystem?.roots));
  assert.ok('permissionPreset' in persisted);

  settings.updateAllowedRoots([extraRoot, extraRoot]);
  assert.deepEqual(settings.getRuntimeSettings().filesystem.roots, [path.resolve(extraRoot)]);
  assert.equal(await pathPolicy.assertPathAllowed(path.join(extraRoot, 'future.txt')), path.join(extraRoot, 'future.txt'));
  await assert.rejects(() => pathPolicy.assertPathAllowed(path.join(tempRoot, 'outside.txt')), /outside configured roots/);

  settings.applyPermissionPreset('safe');
  let current = settings.getRuntimeSettings();
  assert.equal(current.permissionPreset, 'safe');
  assert.equal(current.permissions.filesystemWrite, false);
  assert.equal(current.permissions.shell, false);

  settings.applyPermissionPreset('developer');
  current = settings.getRuntimeSettings();
  assert.equal(current.permissionPreset, 'developer');
  assert.equal(current.permissions.gitWrite, true);
  assert.equal(current.permissions.gitAdvanced, false);

  settings.applyPermissionPreset('unrestricted');
  current = settings.getRuntimeSettings();
  assert.equal(current.permissionPreset, 'unrestricted');
  assert.equal(current.permissions.gitAdvanced, true);
  assert.equal(current.permissions.fullAccess, true);

  settings.updatePermissions({ shell: false });
  assert.equal(settings.getRuntimeSettings().permissionPreset, 'custom');

  const credentialInfo = credentials.credentialStoreInfo();
  if (process.platform === 'win32') {
    assert.equal(credentialInfo.supported, true);
    const secret = 'runtime-key-test-value-1234567890';
    credentials.saveRuntimeKey(secret);
    assert.equal(credentials.loadRuntimeKey(), secret);
    assert.equal(credentials.credentialStoreInfo().saved, true);
    assert.equal(credentials.clearRuntimeKey(), true);
    assert.equal(credentials.loadRuntimeKey(), null);
  } else {
    assert.equal(credentialInfo.supported, false);
    assert.equal(credentials.loadRuntimeKey(), null);
  }

  console.log('settings test ok: migration, presets, dynamic roots, credential store');
} finally {
  process.chdir(originalCwd);
  await fs.rm(tempRoot, { recursive: true, force: true });
}
