const $ = (id) => document.getElementById(id);
const invoke = window.__TAURI__?.core?.invoke;

let currentPage = 'overview';
let state = null;
let busy = false;
let actionError = '';
let rememberKeyDirty = false;
let callHistoryBusy = false;
let powerSettingsBusy = false;
let permissionBusy = false;
let autoReconnectBusy = false;

const pages = {
  overview: ['概览', 'ChatGPT → Secure MCP Tunnel → Desktop Commander'],
  connection: ['连接', '配置并启动 OpenAI Secure MCP Tunnel。'],
  diagnostics: ['诊断', '检查 Tunnel、Node、Desktop Commander 和本地 runtime。'],
  permissions: ['权限中心', '集中处理 ChatX / Desktop Commander 所需的本机访问权限。'],
  calls: ['调用记录', '查看 ChatGPT 通过 MCP 调用 Desktop Commander 的最近记录。'],
  settings: ['设置', '配置 ChatX 的连接恢复与本机运行方式。'],
  logs: ['运行日志', '查看 ChatX 与 Tunnel 生命周期日志。'],
};

function normalizeError(error) {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  try { return JSON.stringify(error); } catch { return String(error); }
}

function showError(message) {
  actionError = message || '';
  displayError(actionError);
}

function displayError(message) {
  $('error').hidden = !message;
  $('error').textContent = message || '';
}

function setBusy(value) {
  busy = value;
  for (const id of ['connect', 'stop', 'clearKey', 'heroConnect', 'heroStop', 'runDiagnostics', 'authorizeAllPermissions']) {
    const element = $(id);
    if (element) element.disabled = value;
  }
  if (!value && state) render(state);
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
  if (page === 'calls') void refreshCallHistory();
  if (page === 'permissions') void refreshPermissionCenter();
  if (page === 'settings') void refreshSettingsPage();
}

function runtimeRunning(s) {
  if (s?.runtimeActive === true) return true;
  return ['ready', 'running', 'healthy', 'connected', 'live'].includes(String(s?.runtimeState || '').toLowerCase());
}

function render(s) {
  state = s;
  const running = runtimeRunning(s);
  const unavailable = s.runtimeState === 'unavailable';
  const reconnecting = Boolean(s.reconnecting);
  const wantsConnection = Boolean(s.desiredConnected);
  const canStop = running || reconnecting || wantsConnection;
  const reconnectAttempt = Number(s.reconnectAttempt || 0);

  $('sidebarDot').className = `dot ${running ? 'ok' : unavailable ? 'bad' : 'warn'}`;
  $('sidebarStatus').textContent = running
    ? '已连接 ChatGPT'
    : reconnecting
      ? `正在自动重连${reconnectAttempt ? ` · 第 ${reconnectAttempt} 次` : ''}`
      : unavailable ? '运行组件缺失' : '未连接';

  $('heroTitle').textContent = running
    ? 'ChatGPT 已连接本机'
    : reconnecting ? 'Tunnel 正在自动重连'
      : s.configured ? '连接已配置' : '配置一次即可连接';
  $('heroCopy').textContent = running
    ? 'Secure MCP Tunnel 正在把 ChatGPT 的 MCP 调用转发给 Desktop Commander。'
    : reconnecting
      ? 'ChatX 已检测到 Tunnel 掉线，正在后台恢复连接；无需重新输入配置。'
      : '输入 Tunnel ID 和 Runtime API Key 后，ChatX 会直接启动 Desktop Commander 的 stdio MCP。';
  $('heroConnect').textContent = running ? '查看连接' : reconnecting ? '自动重连中' : '配置连接';
  $('heroStop').disabled = busy || !canStop;

  $('tunnelFact').textContent = running ? 'Ready' : reconnecting ? 'Reconnecting' : String(s.runtimeState || 'Stopped');
  $('tunnelVersion').textContent = s.tunnelVersion || 'tunnel-client unavailable';
  $('dcFact').textContent = unavailable ? 'Unavailable' : 'Bundled';
  $('dcVersion').textContent = `v${s.desktopCommander?.version || 'unknown'}`;
  $('keyFact').textContent = s.runtimeKeySaved ? '已保存' : '未保存';
  const sessionOnly = s.keyStorage === 'session only';
  $('keyStorageHint').textContent = sessionOnly ? '仅本次输入' : `${s.keyStorage} / 本次输入`;
  $('rememberKeyText').textContent = sessionOnly ? '当前平台不支持保存 Runtime Key' : `使用 ${s.keyStorage === 'macOS Keychain' ? 'macOS 钥匙串' : s.keyStorage} 保存 Runtime Key`;

  if (s.tunnelId && document.activeElement !== $('tunnelId')) $('tunnelId').value = s.tunnelId;
  $('rememberKey').disabled = sessionOnly;
  if (!rememberKeyDirty) $('rememberKey').checked = sessionOnly ? false : Boolean(s.rememberKey || s.runtimeKeySaved || !s.configured);
  $('runtimeKey').placeholder = s.runtimeKeySaved ? '已安全保存，可留空' : '输入 Runtime API Key';
  $('clearKey').disabled = busy || !s.runtimeKeySaved;
  $('connect').disabled = busy || unavailable || running || reconnecting;
  $('stop').disabled = busy || !canStop;
  $('connect').textContent = running ? '已连接' : reconnecting ? '自动重连中' : '连接并启动';
  $('mcpCommand').textContent = s.mcpCommand || '运行组件准备完成后显示 MCP command。';

  const autoReconnect = $('autoReconnect');
  const autoReconnectEnabled = s.autoReconnect !== false;
  autoReconnect.checked = autoReconnectEnabled;
  autoReconnect.disabled = busy || autoReconnectBusy;
  const reconnectStatus = $('reconnectStatus');
  reconnectStatus.textContent = reconnecting
    ? `重连中${reconnectAttempt ? ` · 第 ${reconnectAttempt} 次` : ''}`
    : autoReconnectEnabled ? '已开启' : '已关闭';
  reconnectStatus.className = `setting-status ${reconnecting ? 'warn' : autoReconnectEnabled ? 'ok' : 'off'}`;

  const logs = Array.isArray(s.logs) ? s.logs : [];
  $('logs').textContent = logs.length ? logs.join('\n') : '尚无日志。';
  $('logs').scrollTop = $('logs').scrollHeight;

  const hasRuntimeError = !reconnecting && ['error', 'unavailable'].includes(String(s.runtimeState || '').toLowerCase());
  displayError(actionError || (s.lastError && hasRuntimeError ? s.lastError : ''));
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
    displayError(actionError || normalizeError(error));
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
  if (!tunnelId.startsWith('tunnel_')) {
    showError('Tunnel ID 应以 tunnel_ 开头。');
    return;
  }
  if (!runtimeKey && !state?.runtimeKeySaved) {
    showError(state?.keyStorage === 'session only'
      ? '请输入 Runtime API Key。macOS 每次连接时都需要输入。'
      : '请输入 Runtime API Key。');
    return;
  }
  setBusy(true);
  $('connect').textContent = '正在连接…';
  showError('');
  try {
    const payload = await invoke('connect_tunnel', { tunnelId, runtimeKey, rememberKey });
    $('runtimeKey').value = '';
    rememberKeyDirty = false;
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
    const payload = await invoke('clear_saved_key');
    rememberKeyDirty = false;
    render(payload);
    $('rememberKey').checked = false;
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    setBusy(false);
  }
}

function callHistoryJson(value) {
  try { return JSON.stringify(value ?? null, null, 2); }
  catch { return String(value); }
}

function callHistoryTime(value) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? String(value || '—') : date.toLocaleString();
}

function appendCallDetail(host, title, value) {
  const section = document.createElement('div');
  section.className = 'call-detail-section';
  const heading = document.createElement('h4');
  heading.textContent = title;
  const block = document.createElement('pre');
  block.textContent = callHistoryJson(value);
  section.append(heading, block);
  host.append(section);
}

function renderCallHistory(payload) {
  const items = Array.isArray(payload?.items) ? payload.items : [];
  const toolNames = Array.isArray(payload?.toolNames) ? payload.toolNames : [];
  const toolSelect = $('callToolFilter');
  const selectedTool = toolSelect.value;
  toolSelect.textContent = '';
  const allTools = document.createElement('option');
  allTools.value = '';
  allTools.textContent = '全部工具';
  toolSelect.append(allTools);
  for (const toolName of toolNames) {
    const option = document.createElement('option');
    option.value = toolName;
    option.textContent = toolName;
    toolSelect.append(option);
  }
  toolSelect.value = toolNames.includes(selectedTool) ? selectedTool : '';

  const filteredTotal = Number(payload?.filteredTotal ?? items.length);
  const total = Number(payload?.total ?? filteredTotal);
  const invalid = Number(payload?.invalidEntries || 0);
  $('callHistorySummary').textContent = `${filteredTotal} / ${total} 条，当前显示 ${items.length} 条${invalid ? `；忽略 ${invalid} 条无效记录` : ''}`;

  const stats = payload?.stats || {};
  const successCount = Number(stats.successCount || 0);
  const averageDuration = stats.averageDurationMs == null ? Number.NaN : Number(stats.averageDurationMs);
  const p95Duration = stats.p95DurationMs == null ? Number.NaN : Number(stats.p95DurationMs);
  $('callStatCount').textContent = String(filteredTotal);
  $('callStatSuccess').textContent = filteredTotal ? `${Math.round(successCount / filteredTotal * 1000) / 10}%` : '—';
  $('callStatAvg').textContent = Number.isFinite(averageDuration) ? `${averageDuration} ms` : '—';
  $('callStatP95').textContent = Number.isFinite(p95Duration) ? `${p95Duration} ms` : '—';

  const host = $('callHistory');
  host.textContent = '';
  if (!items.length) {
    const empty = document.createElement('p');
    empty.className = 'muted';
    empty.textContent = '没有符合当前筛选条件的调用记录。';
    host.append(empty);
    return;
  }
  for (const item of items) {
    const details = document.createElement('details');
    details.className = 'call-record';
    const summary = document.createElement('summary');
    summary.className = 'call-record-summary';
    const time = document.createElement('span');
    time.className = 'call-time';
    time.textContent = callHistoryTime(item.timestamp);
    const tool = document.createElement('code');
    tool.textContent = String(item.toolName || 'unknown');
    const status = document.createElement('span');
    status.className = `call-status ${item.success === false ? 'bad' : 'ok'}`;
    status.textContent = item.success === false ? '失败' : '成功';
    const duration = document.createElement('span');
    duration.className = 'call-duration';
    duration.textContent = Number.isFinite(Number(item.duration)) ? `${Number(item.duration)} ms` : '—';
    summary.append(time, tool, status, duration);
    const detail = document.createElement('div');
    detail.className = 'call-record-detail';
    appendCallDetail(detail, '参数', item.arguments ?? {});
    appendCallDetail(detail, '返回结果', item.output ?? null);
    details.append(summary, detail);
    host.append(details);
  }
}

async function refreshCallHistory() {
  if (!invoke || callHistoryBusy) return;
  callHistoryBusy = true;
  $('refreshCalls').disabled = true;
  $('clearCalls').disabled = true;
  try {
    const limit = Number.parseInt($('callLimit').value, 10);
    const args = { limit: Number.isFinite(limit) ? limit : 200 };
    const toolName = $('callToolFilter').value;
    const status = $('callStatusFilter').value;
    if (toolName) args.toolName = toolName;
    if (status) args.status = status;
    renderCallHistory(await invoke('get_call_history', args));
  } catch (error) {
    $('callHistorySummary').textContent = '读取失败';
    $('callHistory').textContent = normalizeError(error);
  } finally {
    callHistoryBusy = false;
    $('refreshCalls').disabled = false;
    $('clearCalls').disabled = false;
  }
}

async function clearCallHistory() {
  if (!invoke || callHistoryBusy) return;
  if (typeof window.confirm === 'function' && !window.confirm('确认清空本机调用记录？此操作无法撤销。')) return;
  callHistoryBusy = true;
  $('refreshCalls').disabled = true;
  $('clearCalls').disabled = true;
  let cleared = false;
  try {
    await invoke('clear_call_history');
    cleared = true;
  } catch (error) {
    $('callHistorySummary').textContent = '清空失败';
    $('callHistory').textContent = normalizeError(error);
  } finally {
    callHistoryBusy = false;
    $('refreshCalls').disabled = false;
    $('clearCalls').disabled = false;
  }
  if (cleared) await refreshCallHistory();
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

function permissionStatusLabel(status) {
  return {
    granted: '已授权',
    denied: '未授权',
    pending: '待授权',
    notNeeded: '无需授权',
    unavailable: '不可用',
    error: '检查失败',
  }[status] || String(status || '未知');
}

function renderPermissionCenter(payload) {
  const supported = Boolean(payload?.supported);
  const host = $('permissionList');
  host.textContent = '';
  if (!supported) {
    const note = document.createElement('p');
    note.className = 'muted';
    note.textContent = '当前平台不需要 macOS TCC 文件夹预授权。';
    host.append(note);
  } else {
    for (const item of payload?.items || []) {
      const row = document.createElement('div');
      row.className = 'permission-row';
      const copy = document.createElement('div');
      const title = document.createElement('strong');
      title.textContent = String(item.label || item.id || '权限');
      const detail = document.createElement('small');
      detail.textContent = String(item.detail || '');
      copy.append(title, detail);
      const badge = document.createElement('span');
      const status = String(item.status || 'pending');
      badge.className = `setting-status ${status === 'granted' || status === 'notNeeded' ? 'ok' : status === 'denied' || status === 'error' ? 'bad' : status === 'pending' ? 'warn' : 'off'}`;
      badge.textContent = permissionStatusLabel(status);
      row.append(copy, badge);
      host.append(row);
    }
  }
  $('authorizeAllPermissions').disabled = permissionBusy || !supported;
  $('openFullDiskAccess').disabled = permissionBusy || !supported;
  $('fullDiskAccessHint').textContent = String(payload?.fullDiskAccess?.detail || 'macOS 不允许应用静默授予“完全磁盘访问”。');
}

async function refreshPermissionCenter() {
  if (!invoke || permissionBusy) return;
  try {
    renderPermissionCenter(await invoke('get_permission_center'));
  } catch (error) {
    $('permissionList').textContent = normalizeError(error);
  }
}

async function requestAllPermissions() {
  if (!invoke || permissionBusy) return;
  permissionBusy = true;
  $('authorizeAllPermissions').disabled = true;
  $('authorizeAllPermissions').textContent = '正在集中授权…';
  showError('');
  try {
    renderPermissionCenter(await invoke('request_all_permissions'));
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    permissionBusy = false;
    $('authorizeAllPermissions').textContent = '一次触发全部授权';
    await refreshPermissionCenter();
  }
}

async function openFullDiskAccess() {
  try { await invoke('open_full_disk_access_settings'); }
  catch (error) { showError(normalizeError(error)); }
}

async function setAutoReconnect() {
  if (!invoke || autoReconnectBusy) return;
  const toggle = $('autoReconnect');
  const requested = toggle.checked;
  autoReconnectBusy = true;
  toggle.disabled = true;
  $('reconnectStatus').textContent = '正在保存…';
  $('reconnectStatus').className = 'setting-status warn';
  showError('');
  try {
    render(await invoke('set_auto_reconnect', { enabled: requested }));
  } catch (error) {
    toggle.checked = !requested;
    showError(normalizeError(error));
  } finally {
    autoReconnectBusy = false;
    await refresh();
  }
}

async function refreshSettingsPage() {
  await Promise.all([refresh(), refreshPowerSettings()]);
}

function renderPowerSettings(payload) {
  const supported = Boolean(payload?.supported);
  const enabled = Boolean(payload?.clamshellAwake);
  const managed = Boolean(payload?.managedByChatX);
  const toggle = $('clamshellAwake');
  toggle.checked = enabled;
  toggle.disabled = powerSettingsBusy || !supported;
  const status = $('clamshellStatus');
  status.textContent = !supported ? '仅支持 macOS' : enabled ? (managed ? '已开启 · 退出自动恢复' : '已开启 · 系统设置') : '已关闭';
  status.className = `setting-status ${supported ? (enabled ? 'ok' : 'off') : 'off'}`;
}

async function refreshPowerSettings() {
  if (!invoke || powerSettingsBusy) return;
  try {
    renderPowerSettings(await invoke('get_power_settings'));
  } catch (error) {
    $('clamshellAwake').disabled = true;
    $('clamshellStatus').textContent = '读取失败';
    $('clamshellStatus').className = 'setting-status bad';
    showError(normalizeError(error));
  }
}

async function setClamshellAwake() {
  if (!invoke || powerSettingsBusy) return;
  const toggle = $('clamshellAwake');
  const requested = toggle.checked;
  powerSettingsBusy = true;
  toggle.disabled = true;
  $('clamshellStatus').textContent = requested ? '等待管理员授权…' : '正在关闭…';
  $('clamshellStatus').className = 'setting-status warn';
  showError('');
  try {
    renderPowerSettings(await invoke('set_clamshell_awake', { enabled: requested }));
  } catch (error) {
    toggle.checked = !requested;
    showError(normalizeError(error));
  } finally {
    powerSettingsBusy = false;
    await refreshPowerSettings();
  }
}

async function openExternal(url) {
  try { await invoke('open_external', { url }); }
  catch (error) { showError(normalizeError(error)); }
}

function bind() {
  $('rememberKey').addEventListener('change', () => { rememberKeyDirty = true; });
  for (const button of document.querySelectorAll('[data-page]')) {
    button.addEventListener('click', () => setPage(button.dataset.page));
  }
  for (const button of document.querySelectorAll('[data-url]')) {
    button.addEventListener('click', () => openExternal(button.dataset.url));
  }
  $('refresh').addEventListener('click', () => currentPage === 'calls'
    ? refreshCallHistory()
    : currentPage === 'permissions'
      ? refreshPermissionCenter()
      : currentPage === 'settings' ? refreshSettingsPage() : refresh());
  $('heroConnect').addEventListener('click', () => setPage('connection'));
  $('heroStop').addEventListener('click', stop);
  $('connect').addEventListener('click', connect);
  $('stop').addEventListener('click', stop);
  $('clearKey').addEventListener('click', clearKey);
  $('runDiagnostics').addEventListener('click', diagnostics);
  $('authorizeAllPermissions').addEventListener('click', requestAllPermissions);
  $('openFullDiskAccess').addEventListener('click', openFullDiskAccess);
  $('autoReconnect').addEventListener('change', setAutoReconnect);
  $('refreshCalls').addEventListener('click', refreshCallHistory);
  $('clearCalls').addEventListener('click', clearCallHistory);
  $('callToolFilter').addEventListener('change', refreshCallHistory);
  $('callStatusFilter').addEventListener('change', refreshCallHistory);
  $('callLimit').addEventListener('change', refreshCallHistory);
  $('clamshellAwake').addEventListener('change', setClamshellAwake);
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
setInterval(() => {
  if (busy || currentPage === 'diagnostics') return;
  if (currentPage === 'calls') void refreshCallHistory();
  else if (currentPage === 'permissions') void refreshPermissionCenter();
  else if (currentPage === 'settings') void refreshSettingsPage();
  else void refresh();
}, 4000);
