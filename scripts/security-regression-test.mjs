import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import syncFs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const root = await fs.mkdtemp(path.join(os.tmpdir(), 'chatx-security-'));
process.env.CHATGPTX_SETTINGS_DIR = path.join(root, 'settings');
process.env.CHATGPTX_ROOTS = root;
process.env.CHATGPTX_ENABLE_SHELL = 'false';
process.env.CHATGPTX_FULL_ACCESS = 'false';
process.env.CHATGPTX_MAX_PROCESS_BUFFER_CHARS = '10000';
const settingsUrl = new URL('../dist/settings.js', import.meta.url).href;
const handlers = new Map();
const mock = { registerTool(name, meta, handler) { handlers.set(name, { meta, handler }); } };
async function raw(name, args) {
  const { meta, handler } = handlers.get(name);
  return handler(meta.inputSchema.parse(args));
}
async function call(name, args) {
  const result = await raw(name, args);
  assert.ok(!result.isError, name + ': ' + JSON.stringify(result));
  return JSON.parse(result.content[0].text);
}
async function rejected(name, args) {
  const result = await raw(name, args);
  assert.equal(result.isError, true, name + ' should fail');
  return result;
}
function git(cwd, args) {
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^GIT_/i.test(key)));
  const result = spawnSync('git', ['-c', 'core.hooksPath=/dev/null', ...args], { cwd, env, encoding: 'utf8', windowsHide: true });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}
function childSettings(directory, content, extra = {}) {
  syncFs.mkdirSync(directory, { recursive: true });
  if (content !== undefined) syncFs.writeFileSync(path.join(directory, 'settings.json'), content);
  const env = { ...process.env, CHATGPTX_SETTINGS_DIR: directory, ...extra };
  const result = spawnSync(process.execPath, ['--input-type=module', '-e',
    'const s = await import(' + JSON.stringify(settingsUrl) + '); console.log(JSON.stringify(s.getRuntimeSettings()));'],
    { env, encoding: 'utf8', windowsHide: true });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout.trim());
}
try {
  const settings = await import(settingsUrl);
  assert.equal(settings.getRuntimeSettings().permissionPreset, 'safe');
  assert.equal(settings.getRuntimeSettings().permissions.shell, false);
  assert.equal(settings.getRuntimeSettings().permissions.filesystemWrite, false);
  const freshEnv = { CHATGPTX_ENABLE_SHELL: '', CHATGPTX_FULL_ACCESS: '' };
  assert.equal(childSettings(path.join(root, 'fresh'), undefined, freshEnv).permissionPreset, 'safe');
  const legacy = childSettings(path.join(root, 'legacy'), JSON.stringify({ permissions: { filesystemWrite: true, shell: true } }));
  assert.equal(legacy.permissions.shell, true);
  assert.equal(legacy.permissions.gitAdvanced, false);
  assert.equal(legacy.permissions.gitWrite, false);
  for (const [label, content] of [['broken', '{oops'], ['invalid', '{"version":2,"permissions":{"shell":"false"}}'],
    ['future', '{"version":99,"permissions":{"shell":true}}']]) {
    const directory = path.join(root, label);
    const current = childSettings(directory, content, { CHATGPTX_ENABLE_SHELL: 'true', CHATGPTX_FULL_ACCESS: 'true' });
    assert.ok(Object.values(current.permissions).every(value => value === false));
    assert.equal(syncFs.readFileSync(path.join(directory, 'settings.json'), 'utf8'), content);
  }
  let notifications = 0;
  settings.onPermissionSettingsChanged(() => notifications++);
  const before = settings.getRuntimeSettings();
  const diskBefore = await fs.readFile(settings.settingsFilePath(), 'utf8');
  assert.throws(() => settings.updateRuntimeSettings({ preset: 'unrestricted', roots: [] }));
  assert.throws(() => settings.updateRuntimeSettings({ shell: 'true' }));
  assert.deepEqual(settings.getRuntimeSettings(), before);
  assert.equal(await fs.readFile(settings.settingsFilePath(), 'utf8'), diskBefore);
  assert.equal(notifications, 0);
  const renameSync = syncFs.renameSync;
  try {
    syncFs.renameSync = () => { throw new Error('simulated persist failure'); };
    assert.throws(() => settings.updateRuntimeSettings({ preset: 'unrestricted' }), /persist failure/);
    assert.deepEqual(settings.getRuntimeSettings(), before);
    assert.equal(notifications, 0);
  } finally { syncFs.renameSync = renameSync; }
  settings.updateRuntimeSettings({ preset: 'developer', shell: true, roots: [root] });
  assert.equal(notifications, 1);
  const persisted = JSON.parse(await fs.readFile(settings.settingsFilePath(), 'utf8'));
  assert.deepEqual(persisted, settings.getRuntimeSettings());

  (await import('../dist/tools/filesystem.js')).registerFilesystemTools(mock);
  (await import('../dist/tools/git.js')).registerGitTools(mock);
  (await import('../dist/tools/project.js')).registerProjectTools(mock);
  (await import('../dist/tools/shell.js')).registerShellTools(mock);
  const moveRoot = path.join(root, 'moves');
  await fs.mkdir(moveRoot);
  const source = path.join(moveRoot, 'source.txt');
  const destination = path.join(moveRoot, 'destination.txt');
  await fs.writeFile(source, 'source');
  await fs.writeFile(destination, 'original-destination');
  const destructiveDenied = await rejected('fs_move', { source, destination: path.join(moveRoot, 'denied.txt') });
  assert.match(destructiveDenied.content[0].text, /delete\/move tools is disabled/);
  settings.updateRuntimeSettings({ filesystemDestructive: true });
  await rejected('fs_move', { source, destination: source, overwrite: true });
  assert.equal(await fs.readFile(source, 'utf8'), 'source');
  await rejected('fs_move', { source: moveRoot, destination: path.join(moveRoot, 'child'), overwrite: true });
  await rejected('fs_move', { source, destination: moveRoot, overwrite: true });
  await rejected('fs_move', { source, destination });
  assert.equal(await fs.readFile(destination, 'utf8'), 'original-destination');

  // Failure after backing up the destination must restore both sides.
  const rename = fs.rename;
  try {
    fs.rename = async (from, to) => {
      if (path.basename(from) === 'source' && to === destination) throw Object.assign(new Error('simulated commit failure'), { code: 'EACCES' });
      return rename(from, to);
    };
    await rejected('fs_move', { source, destination, overwrite: true });
  } finally { fs.rename = rename; }
  assert.equal(await fs.readFile(source, 'utf8'), 'source');
  assert.equal(await fs.readFile(destination, 'utf8'), 'original-destination');
  await call('fs_move', { source, destination, overwrite: true });
  assert.equal(await fs.readFile(destination, 'utf8'), 'source');
  assert.equal(await fs.access(source).then(() => true, () => false), false);
  await fs.writeFile(source, 'cross-volume');
  const copy = fs.cp;
  try {
    fs.rename = async (from, to) => {
      if (from === source) throw Object.assign(new Error('cross volume'), { code: 'EXDEV' });
      return rename(from, to);
    };
    fs.cp = async () => { throw new Error('simulated incomplete copy'); };
    await rejected('fs_move', { source, destination, overwrite: true });
    assert.equal(await fs.readFile(source, 'utf8'), 'cross-volume');
    assert.equal(await fs.readFile(destination, 'utf8'), 'source');
    fs.cp = copy;
    await call('fs_move', { source, destination, overwrite: true });
    assert.equal(await fs.readFile(destination, 'utf8'), 'cross-volume');
  } finally { fs.rename = rename; fs.cp = copy; }

  const repo = path.join(root, 'repo');
  await fs.mkdir(path.join(repo, 'allowed'), { recursive: true });
  git(repo, ['init']);
  git(repo, ['config', 'user.name', 'Regression Test']);
  git(repo, ['config', 'user.email', 'test@example.invalid']);
  await fs.writeFile(path.join(repo, 'outside.txt'), 'original\n');
  git(repo, ['add', '.']);
  git(repo, ['commit', '-m', 'initial']);
  await fs.writeFile(path.join(repo, 'outside.txt'), 'changed\n');
  settings.updateRuntimeSettings({ preset: 'safe', roots: [path.join(repo, 'allowed')] });
  await rejected('git_diff', { repo: path.join(repo, 'allowed') });
  const snapshot = await call('fs_project_snapshot', { root: path.join(repo, 'allowed'), key_files: [] });
  assert.equal(snapshot.git.available, false);

  settings.updateAllowedRoots([repo]);
  const helper = path.join(repo, 'external.sh');
  const marker = path.join(repo, 'executed');
  await fs.writeFile(helper, '#!/bin/sh\nprintf executed > "' + marker.replaceAll('\\', '/') + '"\n');
  await fs.chmod(helper, 0o755);
  git(repo, ['config', 'diff.external', helper.replaceAll('\\', '/')]);
  git(repo, ['config', 'core.fsmonitor', helper.replaceAll('\\', '/')]);
  git(repo, ['config', 'core.hooksPath', path.join(repo, 'hooks')]);
  git(repo, ['config', 'diff.test.textconv', helper.replaceAll('\\', '/')]);
  await fs.writeFile(path.join(repo, '.gitattributes'), 'outside.txt diff=test\n');
  const diff = await call('git_diff', { repo });
  assert.equal(diff.exit_code, 0, diff.stderr);
  assert.ok(diff.stdout.includes('changed'));
  const status = await call('git_status', { repo });
  assert.equal(status.exit_code, 0, status.stderr);
  assert.equal(await fs.access(marker).then(() => true, () => false), false);
  for (const paths of [['../outside.txt'], [':(top)outside.txt'], [path.join(repo, 'outside.txt')], ['.git/config']]) {
    await rejected('git_diff', { repo, paths });
  }
  process.env.GIT_WORK_TREE = root;
  process.env.GIT_DIR = path.join(root, 'nonexistent');
  assert.equal((await call('git_status', { repo })).exit_code, 0);
  delete process.env.GIT_WORK_TREE;
  delete process.env.GIT_DIR;

  settings.applyPermissionPreset('developer');
  settings.updateRuntimeSettings({ gitAdvanced: true });
  await rejected('git_run', { repo, args: ['status'] });
  settings.applyPermissionPreset('developer');
  git(repo, ['config', 'filter.test.clean', helper.replaceAll('\\', '/')]);
  await fs.writeFile(path.join(repo, '.gitattributes'), 'outside.txt filter=test\n');
  const filtered = await call('git_stage', { repo, paths: ['outside.txt'] });
  assert.notEqual(filtered.exit_code, 0, 'Filtered staging must fail rather than bypass the filter silently');
  assert.equal(await fs.access(marker).then(() => true, () => false), false);
  await fs.writeFile(path.join(repo, '.gitattributes'), '');
  const staged = await call('git_stage', { repo, paths: ['outside.txt'] });
  assert.equal(staged.exit_code, 0, staged.stderr);
  await fs.mkdir(path.join(repo, 'hooks'));
  for (const name of ['pre-commit', 'post-commit', 'reference-transaction']) {
    await fs.copyFile(helper, path.join(repo, 'hooks', name));
    await fs.chmod(path.join(repo, 'hooks', name), 0o755);
  }
  assert.equal((await call('git_commit', { repo, message: 'safe commit' })).exit_code, 0);
  assert.equal(await fs.access(marker).then(() => true, () => false), false);

  // A .git file pointing to metadata outside the allowed root must be rejected.
  const separate = path.join(root, 'separate');
  const metadata = path.join(root, 'metadata');
  await fs.mkdir(separate);
  git(separate, ['init', '--separate-git-dir', metadata]);
  settings.updateAllowedRoots([separate]);
  await rejected('git_status', { repo: separate });
  settings.updateAllowedRoots([repo]);
  await fs.writeFile(path.join(repo, '.git', 'objects', 'info', 'alternates'), metadata + '\n');
  await rejected('git_status', { repo });
  await fs.rm(path.join(repo, '.git', 'objects', 'info', 'alternates'));
  git(repo, ['config', 'include.path', path.join(root, 'other-config')]);
  await rejected('git_status', { repo });
  git(repo, ['config', '--unset', 'include.path']);

  settings.updateRuntimeSettings({ preset: 'developer', shell: true, roots: [root] });
  const counter = path.join(root, 'counter');
  const args = ['-e', 'require("fs").appendFileSync(' + JSON.stringify(counter) + ',"x"); process.stdout.write("a".repeat(2500));'];
  const execution = await call('run_process', { executable: process.execPath, args, cwd: root, max_output_chars: 1000 });
  assert.equal(execution.stdout_next_offset, 1000);
  const next = await call('execution_output', { execution_id: execution.execution_id, stdout_offset: 1000, max_chars: 1000 });
  const last = await call('execution_output', { execution_id: execution.execution_id, stdout_offset: 2000, max_chars: 1000 });
  assert.equal(execution.stdout + next.stdout + last.stdout, 'a'.repeat(2500));
  assert.equal(last.stdout_next_offset, null);
  assert.equal(await fs.readFile(counter, 'utf8'), 'x');
  await rejected('run_process', { executable: process.execPath, args, cwd: root, output_offset: 1000 });
  await rejected('run_command', { command: 'echo never', cwd: root, output_offset: 1000 });
  assert.equal(await fs.readFile(counter, 'utf8'), 'x');
  const capped = await call('run_process', { executable: process.execPath, args: ['-e', 'process.stdout.write("z".repeat(15000))'], cwd: root });
  assert.equal(capped.stdout_stored_chars, 10000);
  assert.equal(capped.stdout_dropped_chars, 5000);
  assert.equal(capped.stdout_next_offset, null);
  const oldNow = Date.now;
  try {
    Date.now = () => oldNow() + 11 * 60 * 1000;
    await rejected('execution_output', { execution_id: execution.execution_id });
  } finally { Date.now = oldNow; }
  assert.equal(await fs.readFile(counter, 'utf8'), 'x');
  console.log('security regression ok: Git boundaries/helpers, move rollback, safe defaults, settings atomicity, execution pagination');
} finally {
  await fs.rm(root, { recursive: true, force: true });
}
