import assert from 'node:assert/strict';
import fs from 'node:fs';

const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));
const lock = JSON.parse(fs.readFileSync('package-lock.json', 'utf8'));
const tauri = JSON.parse(fs.readFileSync('src-tauri/tauri.conf.json', 'utf8'));
const cargo = fs.readFileSync('src-tauri/Cargo.toml', 'utf8');

const cargoVersion = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];

assert.ok(/^\d+\.\d+\.\d+$/.test(pkg.version), `Invalid package version: ${pkg.version}`);
assert.equal(tauri.version, pkg.version, 'Tauri version must match package.json');
assert.equal(cargoVersion, pkg.version, 'Cargo version must match package.json');
assert.equal(lock.packages?.['']?.name, pkg.name, 'package-lock root package name must match package.json');

// npm ci requires dependency specs to match the lock. The root package version
// itself is metadata and may lag until npm next refreshes the lock file.
for (const section of ['dependencies', 'devDependencies']) {
  const expected = pkg[section] ?? {};
  const locked = lock.packages?.['']?.[section] ?? {};
  for (const [name, spec] of Object.entries(expected)) {
    assert.equal(locked[name], spec, `package-lock ${section}.${name} must match package.json`);
  }
}

console.log(`version consistency ok: ${pkg.version}`);
