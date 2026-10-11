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
const bash = process.platform === 'win32'
  ? path.join(process.env.ProgramFiles || 'C:\\Program Files', 'Git', 'bin', 'bash.exe')
  : '/bin/bash';
assert.ok(fs.existsSync(bash), 'bash is needed for deployment rollback test');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'chatx-rollback-'));
const slash = (p) => p.replace(/\\/g, '/');
const quote = (value) => "'" + slash(value).replace(/'/g, "'\\''") + "'";
try {
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
