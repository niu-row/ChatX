import assert from 'node:assert/strict';
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';

const config = JSON.parse(fs.readFileSync('src-tauri/tauri.conf.json', 'utf8'));
const cargo = fs.readFileSync('src-tauri/Cargo.toml', 'utf8');
const rust = fs.readFileSync('src-tauri/src/main.rs', 'utf8');
const html = fs.readFileSync('desktop/index.html', 'utf8');
const appJs = fs.readFileSync('desktop/app.js', 'utf8');
const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));
const defaultLauncher = fs.readFileSync('启动-ChatGPTX.cmd', 'utf8');
const webLauncher = fs.readFileSync('启动-ChatGPTX-Web.cmd', 'utf8');

assert.equal(config.build.frontendDist, '../desktop');
assert.equal(config.app.withGlobalTauri, true);
assert.equal(config.app.windows?.[0]?.label, 'main');
assert.match(cargo, /tauri-plugin-dialog\s*=\s*"2"/);
assert.match(rust, /async fn pick_folders/);
assert.match(rust, /blocking_pick_folders/);
assert.match(rust, /async fn backend_request/);
assert.match(rust, /async fn ensure_backend/);
assert.match(html, /id="chooseFolders"/);
assert.match(html, /获取 Tunnel ID/);
assert.match(html, /获取 Runtime Key/);
assert.match(html, /data-page="guide"/);
assert.match(html, /5 分钟完成 ChatGPTX 设置/);
assert.match(html, /<details class="advanced-box">/);
assert.doesNotMatch(html, /<textarea[^>]+id="roots"/);
assert.match(appJs, /invoke\('pick_folders'/);
assert.match(appJs, /updateSettings\(\{ roots: merged \}\)/);
assert.equal(pkg.scripts['desktop:dev'], 'npm run build && tauri dev');
assert.equal(pkg.scripts['desktop:build'], 'npm run build && tauri build');
assert.match(defaultLauncher, /start-chatgptx-desktop\.cmd/i);
assert.match(webLauncher, /start-chatgptx\.cmd/i);

execFileSync(process.execPath, ['--check', 'desktop/app.js'], { stdio: 'pipe' });
console.log('desktop static test ok: Tauri shell, native folder picker, backend bridge');
