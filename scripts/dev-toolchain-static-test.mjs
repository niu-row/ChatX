import assert from 'node:assert/strict';
import fs from 'node:fs';

const script = fs.readFileSync('scripts/dev-toolchain.mjs', 'utf8');
const agents = fs.readFileSync('AGENTS.md', 'utf8');
const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));

for (const needle of [
  'os.userInfo().homedir',
  "path.join(realHome, '.cargo', 'bin'",
  'Android Studio.app',
  "path.join(realHome, 'Library', 'Android', 'sdk')",
  'cachedGradleZip',
  "['lintDebug', 'test', '--no-daemon']",
  "path.join(process.env.ProgramFiles || 'C:\\\\Program Files'",
  "run('git', ['diff', '--check']",
  "case 'local-full'",
]) {
  assert.ok(script.includes(needle), `dev-toolchain.mjs is missing: ${needle}`);
}

for (const needle of [
  'Critical environment rule: inherited HOME may be fake',
  'npm run dev:doctor',
  'npm run test:local-full',
  'Tunnel health semantics',
  'healthy == true',
  'Tunnel proxy semantics',
  'Relay ownership and lifecycle',
]) {
  assert.ok(agents.includes(needle), `AGENTS.md is missing: ${needle}`);
}

for (const name of [
  'dev:doctor',
  'test:rust:check',
  'test:rust',
  'test:android:gradle',
  'test:local-full',
]) {
  assert.ok(pkg.scripts?.[name], `package.json is missing script: ${name}`);
}

console.log('dev toolchain isolation checks passed');
