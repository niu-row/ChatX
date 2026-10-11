// Isolated failure injection for the *same rollback function* bundled in ChatX.
// Uses only temporary fixture files; never touches Nginx/systemd or production.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = fs.readFileSync(path.join(root, 'deploy/oneclick/install.sh'), 'utf8');
const begin = source.indexOf('\nrollback() {');
const end = source.indexOf('\ntrap rollback ERR HUP INT TERM', begin);
assert.ok(begin >= 0 && end > begin, 'rollback function must be available');
const rollback = source.slice(begin, end);
// Exercise the REAL install.sh argument validation before any root, Nginx or
// systemd checks. This is where old IDs used to fail despite passing UI/Rust.
const rootCheck = source.indexOf('\nif [ "$(id -u)"');
assert.ok(rootCheck > 0, 'installer root safety check must exist');
const validateOnly = source.slice(0, rootCheck);
const bash = process.platform === 'win32'
  ? path.join(process.env.ProgramFiles || 'C:\\Program Files', 'Git', 'bin', 'bash.exe')
  : '/bin/bash';
assert.ok(fs.existsSync(bash), 'bash is needed for deployment rollback test');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'chatx-rollback-'));
const slash = (p) => p.replace(/\\/g, '/');
const quote = (value) => "'" + slash(value).replace(/'/g, "'\\''") + "'";
try {
  const checkId = (id) => spawnSync(bash,
    ['-c', validateOnly, '--', 'check', 'relay.example.com', id],
    { encoding: 'utf8', timeout: 10000 });
  for (const size of [16, 32, 78]) {
    for (const digit of ['a', 'A']) {
      const id = 'd_' + digit.repeat(size);
      assert.equal(checkId(id).status, 0, `installer must accept existing ${size}-character IDs`);
    }
  }
  for (const id of ['d_' + 'a'.repeat(15), 'd_' + 'b'.repeat(79),
    'd_' + 'z'.repeat(32), '../../invalid']) {
    assert.equal(checkId(id).status, 2, 'installer must reject invalid ID: ' + id);
  }
  const backup = path.join(scratch, 'backup');
  const active = path.join(scratch, 'active');
  fs.mkdirSync(backup);
  fs.mkdirSync(active);
  const names = { server: 'bin', 'relay.env': 'envfile',
    'mcp.conf': 'conf', 'base.conf': 'base_conf', site: 'site' };
  for (const [original, current] of Object.entries(names)) {
    fs.writeFileSync(path.join(backup, original), 'previous-' + original);
    fs.writeFileSync(path.join(active, current), 'broken-' + original);
  }
  const lines = [
    'set -Eeuo pipefail',
    'backup=' + quote(backup),
    ...Object.values(names).map((name) =>
      name + '=' + quote(path.join(active, name))),
    'had_bin=1; had_env=1; had_conf=1; had_base=1',
    'site_modified=1; service_created=0; had_service=0',
    'nginx_run() { :; }',
    rollback,
    'rollback',
  ];
  const execution = spawnSync(bash, ['-c', lines.join('\n')],
    { encoding: 'utf8', timeout: 10000 });
  assert.equal(execution.status, 1,
    'failure injection must terminate with a failing status: ' + execution.stderr);
  assert.match(execution.stderr, /Rolled back/);
  for (const [original, current] of Object.entries(names)) {
    assert.equal(fs.readFileSync(path.join(active, current), 'utf8'),
      'previous-' + original, current + ' must be restored');
  }
  assert.equal(fs.existsSync(path.join(active, 'bin.rollback')), false);
  console.log('Relay rollback failure injection passed: binary, env and Nginx restored');
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
