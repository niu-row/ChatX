import assert from 'node:assert/strict';
import fs from 'node:fs';

const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));
const lock = JSON.parse(fs.readFileSync('package-lock.json', 'utf8'));
const tauri = JSON.parse(fs.readFileSync('src-tauri/tauri.conf.json', 'utf8'));
const cargo = fs.readFileSync('src-tauri/Cargo.toml', 'utf8');
const server = fs.readFileSync('src/server.ts', 'utf8');

const cargoVersion = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const serverVersion = server.match(/SERVER_VERSION\s*=\s*'([^']+)'/)?.[1];

assert.ok(/^\d+\.\d+\.\d+$/.test(pkg.version), `Invalid package version: ${pkg.version}`);
assert.equal(lock.version, pkg.version, 'package-lock root version must match package.json');
assert.equal(lock.packages?.['']?.version, pkg.version, 'package-lock package version must match package.json');
assert.equal(tauri.version, pkg.version, 'Tauri version must match package.json');
assert.equal(cargoVersion, pkg.version, 'Cargo version must match package.json');
assert.equal(serverVersion, pkg.version, 'MCP server version must match package.json');

console.log(`version consistency ok: ${pkg.version}`);
