const $ = (id) => document.getElementById(id);
const invoke = window.__TAURI__?.core?.invoke;

let currentPage = 'overview';
let state = null;
let busy = false;

const pages = {
  overview: ['概览', 'ChatGPT → Secure MCP Tunnel → Desktop Commander'],
  connection: ['连接', '配置并启动 OpenAI Secure MCP Tunnel。'],
  diagnostics: ['诊断', '检查 Tunnel、Node、Desktop Commander 和本地 runtime。'],
  logs: ['运行日志', '查看 ChatX 与 Tunnel 生命周期日志。'],
};

function normalizeError(error) {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  try { return JSON.stringify(error); } catch { return String(error); }
}

function showError(message) {
  $('error').hidden = !message;
  $('error').textContent = message || '';
}

function setBusy(value) {
  busy = value;
  for (const id of ['connect', 'stop', 'clearKey', 'heroConnect', 'heroStop', 'runDiagnostics']) {
    const element = $(id);
    if (element) element.disabled = value;
  }
}

function setPage(page) {
  if (!pages[page]) return;
  currentPage = page;
  for (const button of document.querySelectorAll('[data-page]')) {
    button.classList.toggle('active', button.dataset.page === page);
  }
  for (const panel of document.querySelectorAll('[data-page-panel]')) {
    panel.classList.toggle('active', panel.dataset.pagePanel === page);
  }
  $('pageTitle').textContent = pages[page][0];
  $('pageSubtitle').textContent = pages[page][1];
}

function runtimeRunning(s) {
  if (s?.runtimeActive === true) return true;
  return ['ready', 'running', 'healthy', 'connected', 'live'].includes(String(s?.runtimeState || '').toLowerCase());
}

function render(s) {
  state = s;
  const running = runtimeRunning(s);
  const unavailable = s.runtimeState === 'unavailable';

  $('sidebarDot').className = `dot ${running ? 'ok' : unavailable ? 'bad' : 'warn'}`;
  $('sidebarStatus').textContent = running ? '已连接 ChatGPT' : unavailable ? '运行组件缺失' : '未连接';

  $('heroTitle').textContent = running ? 'ChatGPT 已连接本机' : s.configured ? '连接已配置' : '配置一次即可连接';
  $('heroCopy').textContent = running
    ? 'Secure MCP Tunnel 正在把 ChatGPT 的 MCP 调用转发给 Desktop Commander。'
    : '输入 Tunnel ID 和 Runtime API Key 后，ChatX 会直接启动 Desktop Commander 的 stdio MCP。';
  $('heroConnect').textContent = running ? '查看连接' : '配置连接';
  $('heroStop').disabled = busy || !running;

  $('tunnelFact').textContent = running ? 'Ready' : String(s.runtimeState || 'Stopped');
  $('tunnelVersion').textContent = s.tunnelVersion || 'tunnel-client unavailable';
  $('dcFact').textContent = unavailable ? 'Unavailable' : 'Bundled';
  $('dcVersion').textContent = `v${s.desktopCommander?.version || 'unknown'}`;
  $('keyFact').textContent = s.runtimeKeySaved ? '已保存' : '未保存';

  if (s.tunnelId && document.activeElement !== $('tunnelId')) $('tunnelId').value = s.tunnelId;
  $('rememberKey').checked = Boolean(s.rememberKey || s.runtimeKeySaved);
  $('runtimeKey').placeholder = s.runtimeKeySaved ? '已使用 Windows DPAPI 保存，可留空' : '输入 Runtime API Key';
  $('clearKey').disabled = busy || !s.runtimeKeySaved;
  $('connect').disabled = busy || unavailable || running;
  $('stop').disabled = busy || !running;
  $('connect').textContent = running ? '已连接' : '连接并启动';
  $('mcpCommand').textContent = s.mcpCommand || '运行组件准备完成后显示 MCP command。';

  const logs = Array.isArray(s.logs) ? s.logs : [];
  $('logs').textContent = logs.length ? logs.join('\n') : '尚无日志。';
  $('logs').scrollTop = $('logs').scrollHeight;

  const hasRuntimeError = ['error', 'unavailable'].includes(String(s.runtimeState || '').toLowerCase());
  showError(s.lastError && hasRuntimeError ? s.lastError : '');
}

async function refresh() {
  if (!invoke) {
    showError('当前页面不是由 ChatX Tauri 应用启动。');
    return;
  }
  try {
    const payload = await invoke('get_status');
    render(payload);
  } catch (error) {
    showError(normalizeError(error));
  }
}

async function connect() {
  const tunnelId = $('tunnelId').value.trim();
  const runtimeKey = $('runtimeKey').value.trim();
  const rememberKey = $('rememberKey').checked;
  if (!tunnelId) {
    showError('请输入 Tunnel ID。');
    return;
  }
  setBusy(true);
  showError('');
  try {
    const payload = await invoke('connect_tunnel', { tunnelId, runtimeKey, rememberKey });
    $('runtimeKey').value = '';
    render(payload);
    setPage('overview');
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    setBusy(false);
    await refresh();
  }
}

async function stop() {
  setBusy(true);
  showError('');
  try {
    render(await invoke('stop_tunnel'));
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    setBusy(false);
    await refresh();
  }
}

async function clearKey() {
  setBusy(true);
  showError('');
  try {
    render(await invoke('clear_saved_key'));
    $('rememberKey').checked = false;
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    setBusy(false);
  }
}

async function diagnostics() {
  setBusy(true);
  const host = $('diagnostics');
  host.innerHTML = '<p class="muted">正在检查…</p>';
  try {
    const result = await invoke('run_diagnostics');
    host.innerHTML = '';
    for (const check of result.checks || []) {
      const row = document.createElement('div');
      row.className = `diagnostic ${check.ok ? 'ok' : 'bad'}`;
      const badge = document.createElement('span');
      badge.className = 'diag-badge';
      badge.textContent = check.ok ? 'PASS' : 'CHECK';
      const copy = document.createElement('div');
      const title = document.createElement('strong');
      title.textContent = check.name;
      const detail = document.createElement('small');
      detail.textContent = String(check.detail || '');
      copy.append(title, detail);
      row.append(badge, copy);
      host.append(row);
    }
  } catch (error) {
    host.textContent = normalizeError(error);
  } finally {
    setBusy(false);
  }
}

async function openExternal(url) {
  try { await invoke('open_external', { url }); }
  catch (error) { showError(normalizeError(error)); }
}

function bind() {
  for (const button of document.querySelectorAll('[data-page]')) {
    button.addEventListener('click', () => setPage(button.dataset.page));
  }
  for (const button of document.querySelectorAll('[data-url]')) {
    button.addEventListener('click', () => openExternal(button.dataset.url));
  }
  $('refresh').addEventListener('click', refresh);
  $('heroConnect').addEventListener('click', () => setPage('connection'));
  $('heroStop').addEventListener('click', stop);
  $('connect').addEventListener('click', connect);
  $('stop').addEventListener('click', stop);
  $('clearKey').addEventListener('click', clearKey);
  $('runDiagnostics').addEventListener('click', diagnostics);
  $('openLogs').addEventListener('click', async () => {
    try { await invoke('open_logs'); }
    catch (error) { showError(normalizeError(error)); }
  });
  $('copyLogs').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText($('logs').textContent || ''); }
    catch (error) { showError(normalizeError(error)); }
  });
}

bind();
void refresh();
setInterval(() => { if (!busy && currentPage !== 'diagnostics') void refresh(); }, 4000);
