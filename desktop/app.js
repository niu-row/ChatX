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
let monitorBusy = false;
let pendingMonitorDeleteDeviceId = '';
let monitorPortDirty = false;
let networkBusy = false;
let networkDirty = false;

const pages = {
  overview: ['概览', 'ChatGPT → Secure MCP Tunnel → Desktop Commander'],
  connection: ['连接', '配置并启动 OpenAI Secure MCP Tunnel。'],
  diagnostics: ['诊断', '检查 Tunnel、Node、Desktop Commander 和本地 runtime。'],
  permissions: ['权限中心', '集中处理 ChatX / Desktop Commander 所需的本机访问权限。'],
  calls: ['调用记录', '查看 ChatGPT 通过 MCP 调用 Desktop Commander 的最近记录。'],
  monitor: ['手机连接', 'Direct / Relay 统一 WSS，端到端加密查看 Tunnel 与 MCP 状态。'],
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
  if (page !== 'monitor') pendingMonitorDeleteDeviceId = '';
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
  if (page === 'monitor') void refreshMonitorPage();
  if (page === 'permissions') void refreshPermissionCenter();
  if (page === 'settings') void refreshSettingsPage();
}

function runtimeRunning(s) {
  if (typeof s?.runtimeActive === 'boolean') return s.runtimeActive;
  return ['ready', 'running', 'healthy', 'connected', 'live'].includes(String(s?.runtimeState || '').toLowerCase());
}

function render(s) {
  state = s;
  const running = runtimeRunning(s);
  const unavailable = s.runtimeState === 'unavailable';
  const reconnecting = Boolean(s.reconnecting);
  const wantsConnection = Boolean(s.desiredConnected);
  const runtimeHealth = String(s.runtimeHealth || '').toLowerCase();
  const degraded = wantsConnection && ['suspect', 'starting'].includes(runtimeHealth);
  const down = wantsConnection && runtimeHealth === 'down';
  const canStop = running || reconnecting || wantsConnection;
  const reconnectAttempt = Number(s.reconnectAttempt || 0);

  $('sidebarDot').className = `dot ${running ? 'ok' : unavailable || down ? 'bad' : 'warn'}`;
  $('sidebarStatus').textContent = running
    ? '已连接 ChatGPT'
    : reconnecting
      ? `正在自动重连${reconnectAttempt ? ` · 第 ${reconnectAttempt} 次` : ''}`
      : degraded
        ? 'Tunnel 连接异常 · 检查中'
        : down
          ? 'Tunnel 已断开'
          : unavailable ? '运行组件缺失' : '未连接';

  $('heroTitle').textContent = running
    ? 'ChatGPT 已连接本机'
    : reconnecting ? 'Tunnel 正在自动重连'
      : degraded ? 'Tunnel 连接正在检查'
        : down ? 'Tunnel 连接已中断'
          : s.configured ? '连接已配置' : '配置一次即可连接';
  $('heroCopy').textContent = running
    ? 'Secure MCP Tunnel 正在把 ChatGPT 的 MCP 调用转发给 Desktop Commander。'
    : reconnecting
      ? 'ChatX 已检测到 Tunnel 掉线，正在后台恢复连接；无需重新输入配置。'
      : degraded
        ? '本地 runtime 仍在运行，但 Control Plane 最近出现连续 poll 异常；ChatX 正在持续检测。'
        : down
          ? 'ChatX 已确认 Control Plane 或本地 runtime 不可用；可等待自动重连或手动处理。'
          : '输入 Tunnel ID 和 Runtime API Key 后，ChatX 会直接启动 Desktop Commander 的 stdio MCP。';
  $('heroConnect').textContent = running ? '查看连接' : reconnecting ? '自动重连中' : degraded || down ? '查看连接状态' : '配置连接';
  $('heroStop').disabled = busy || !canStop;

  $('tunnelFact').textContent = running
    ? 'Ready'
    : reconnecting
      ? 'Reconnecting'
      : degraded
        ? 'Suspect'
        : down
          ? 'Down'
          : String(s.runtimeState || 'Stopped');
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

function liveDuration(ms) {
  const value = Math.max(0, Number(ms || 0));
  return value < 1000 ? `${value} ms` : `${(value / 1000).toFixed(value < 10000 ? 1 : 0)} s`;
}

function renderMcpLive(payload) {
  const status = payload?.status || {};
  const calls = (Array.isArray(payload?.recentCalls) ? payload.recentCalls : []).filter((call) => call.running);
  const badge = $('mcpLiveStatus');
  if (!badge) return;
  badge.textContent = calls.length ? `${calls.length} RUNNING` : String(status.state || 'idle').toUpperCase();
  badge.className = `setting-status ${calls.length ? 'warn' : status.state === 'stalled' ? 'bad' : status.state === 'active' ? 'ok' : 'off'}`;
  const host = $('mcpLiveCalls');
  host.textContent = '';
  if (!calls.length) {
    const empty = document.createElement('p');
    empty.className = 'muted';
    empty.textContent = '当前没有正在执行的 MCP 指令。';
    host.append(empty);
    return;
  }
  for (const call of calls) {
    const row = document.createElement('div');
    row.className = 'mcp-live-row';
    const tool = document.createElement('code');
    tool.textContent = String(call.toolName || 'unknown');
    const duration = document.createElement('span');
    duration.textContent = liveDuration(call.durationMs);
    row.append(tool, duration);
    host.append(row);
  }
}

async function refreshMcpLive() {
  if (!invoke || currentPage !== 'overview') return;
  try { renderMcpLive(await invoke('get_mcp_live_status')); }
  catch { /* live MCP display is observational only */ }
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

function collectNetworkPayload() {
  return {
    proxy: {
      mode: $('proxyMode').value,
      url: $('proxyUrl').value.trim(),
    },
    relay: {
      enabled: $('relayEnabled').checked,
      baseUrl: $('relayBaseUrl').value.trim(),
    },
  };
}

function controlPlaneTone(value) {
  const status = String(value || 'unknown').toLowerCase();
  if (status === 'healthy') return 'ok';
  if (status === 'down') return 'bad';
  if (status === 'suspect' || status === 'starting') return 'warn';
  return 'off';
}

function ageLabel(timestamp) {
  const value = Number(timestamp || 0);
  if (!value) return '—';
  const seconds = Math.max(0, Math.floor((Date.now() - value) / 1000));
  if (seconds < 5) return '刚刚';
  if (seconds < 60) return `${seconds}s 前`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m 前`;
  return `${Math.floor(minutes / 60)}h 前`;
}

function renderNetworkSettings(payload) {
  const proxy = payload?.proxy || {};
  const relay = payload?.relay || {};
  const relayState = relay.status || {};
  const tunnel = payload?.tunnel || {};

  if (!networkDirty) {
    $('proxyMode').value = proxy.mode || 'direct';
    $('proxyUrl').value = proxy.url || '';
    $('relayEnabled').checked = Boolean(relay.enabled);
    $('relayBaseUrl').value = relay.baseUrl || '';
  }

  const proxyMode = $('proxyMode').value || 'direct';
  for (const id of ['proxyMode', 'relayEnabled', 'relayBaseUrl', 'saveNetwork', 'testNetwork', 'saveRelay', 'testRelay', 'registerRelay']) {
    const element = $(id);
    if (element) element.disabled = networkBusy;
  }
  $('proxyUrl').disabled = networkBusy || proxyMode !== 'manual';

  $('systemProxyFact').textContent = proxy.systemError
    ? `读取失败 · ${proxy.systemError}`
    : proxy.systemUrl || '未配置';
  $('effectiveProxyFact').textContent = proxy.effectiveError
    ? `无效 · ${proxy.effectiveError}`
    : proxy.effectiveUrl || 'Direct · 直连';
  const runtimeProxy = String(tunnel.proxySource || '').trim();
  $('runtimeProxyFact').textContent = runtimeProxy
    ? (runtimeProxy === 'none' ? 'Direct · none' : runtimeProxy)
    : '未知 / 等待 runtime';

  const cpState = String(tunnel.controlPlaneState || 'unknown').toLowerCase();
  $('controlPlaneFact').textContent = `${cpState.toUpperCase()}${Number(tunnel.controlPlaneFailures || 0) ? ` · ${Number(tunnel.controlPlaneFailures)} FAIL` : ''}`;
  $('controlPlaneReason').textContent = tunnel.controlPlaneReason || '最近没有 Control Plane 错误。';

  const networkStatus = $('networkStatus');
  networkStatus.textContent = `${proxyMode.toUpperCase()} · ${cpState.toUpperCase()}`;
  networkStatus.className = `setting-status ${controlPlaneTone(cpState)}`;

  const relayStatus = $('relayStatus');
  const enabled = Boolean(relay.enabled);
  const registered = Boolean(relay.registered || relayState.configured || relay.credentialSaved);
  const setupProvisioned = Boolean(relay.setupProvisioned);
  const configured = registered;
  const connected = Boolean(relayState.connected);
  const connecting = Boolean(relayState.connecting);
  relayStatus.textContent = !enabled
    ? '已关闭'
    : connected
      ? '已连接'
      : connecting
        ? '连接中'
        : configured
          ? '离线'
          : '待注册';
  relayStatus.className = `setting-status ${!enabled ? 'off' : connected ? 'ok' : connecting ? 'warn' : registered ? 'bad' : 'warn'}`;

  $('relayRegistrationFact').textContent = registered
    ? '已注册 · 安全存储'
    : setupProvisioned
      ? '未注册 · 可注册'
      : '未注册 · 等待管理员预配';
  const registerRelay = $('registerRelay');
  registerRelay.textContent = registered ? '已注册' : '注册此电脑';
  registerRelay.disabled = networkBusy || registered || !$('relayBaseUrl').value.trim();

  $('relaySessionFact').textContent = connected
    ? `CONNECTED${relayState.sessionId ? ` · ${String(relayState.sessionId).slice(0, 12)}…` : ''}`
    : connecting
      ? `CONNECTING${relayState.reconnectAttempt ? ` · #${relayState.reconnectAttempt}` : ''}`
      : 'DISCONNECTED';
  $('relayDesktopFact').textContent = relayState.desktopId || '—';
  $('relayHeartbeatFact').textContent = ageLabel(relayState.lastHeartbeatAt);
  $('relayErrorFact').textContent = relayState.lastError || '—';
}

async function refreshNetworkSettings() {
  if (!invoke || networkBusy) return;
  try {
    renderNetworkSettings(await invoke('get_network_settings'));
  } catch (error) {
    $('networkStatus').textContent = '读取失败';
    $('networkStatus').className = 'setting-status bad';
    showError(normalizeError(error));
  }
}

async function saveNetworkSettings() {
  if (!invoke || networkBusy) return;
  networkBusy = true;
  showError('');
  $('networkStatus').textContent = '正在应用…';
  $('networkStatus').className = 'setting-status warn';
  try {
    const result = await invoke('set_network_settings', { payload: collectNetworkPayload() });
    networkDirty = false;
    renderNetworkSettings(result);
    $('networkTestResult').textContent = 'Tunnel 网络设置已应用。';
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    networkBusy = false;
    await refreshNetworkSettings();
  }
}

async function saveRelaySettings() {
  if (!invoke || networkBusy) return;
  networkBusy = true;
  showError('');
  $('relayStatus').textContent = '正在应用…';
  $('relayStatus').className = 'setting-status warn';
  try {
    const result = await invoke('set_network_settings', { payload: collectNetworkPayload() });
    networkDirty = false;
    renderNetworkSettings(result);
    $('relayTestResult').textContent = result?.relay?.registered
      ? 'Relay 设置已应用。'
      : 'Relay 设置已保存；此电脑尚未注册。';
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    networkBusy = false;
    await refreshNetworkSettings();
  }
}

async function registerRelayDesktop() {
  if (!invoke || networkBusy) return;
  networkBusy = true;
  showError('');
  $('relayStatus').textContent = '正在注册…';
  $('relayStatus').className = 'setting-status warn';
  $('relayTestResult').textContent = '正在注册此电脑…';
  try {
    const saved = await invoke('set_network_settings', { payload: collectNetworkPayload() });
    networkDirty = false;
    renderNetworkSettings(saved);
    const result = await invoke('register_relay_desktop');
    renderNetworkSettings(result);
    $('relayTestResult').textContent = '此电脑已注册；后续公网连接只使用设备自己的安全凭据。';
  } catch (error) {
    $('relayTestResult').textContent = '注册失败';
    showError(normalizeError(error));
  } finally {
    networkBusy = false;
    await refreshNetworkSettings();
  }
}

async function testNetworkSettings() {
  if (!invoke || networkBusy) return;
  networkBusy = true;
  showError('');
  $('networkTestResult').textContent = '正在测试…';
  try {
    const result = await invoke('test_network_settings', { payload: collectNetworkPayload() });
    const proxy = result?.proxy || {};
    $('networkTestResult').textContent = `Tunnel ${proxy.ok ? 'OK' : 'FAIL'} · ${proxy.target || 'unknown'}`;
  } catch (error) {
    $('networkTestResult').textContent = '测试失败';
    showError(normalizeError(error));
  } finally {
    networkBusy = false;
    try { renderNetworkSettings(await invoke('get_network_settings')); } catch {}
  }
}

async function testRelaySettings() {
  if (!invoke || networkBusy) return;
  networkBusy = true;
  showError('');
  $('relayTestResult').textContent = '正在测试…';
  try {
    const result = await invoke('test_network_settings', { payload: collectNetworkPayload() });
    const relay = result?.relay || {};
    $('relayTestResult').textContent = relay.ok
      ? `Relay OK · ${relay.target || ''}`
      : `Relay FAIL · ${relay.error || relay.target || 'unknown'}`;
  } catch (error) {
    $('relayTestResult').textContent = '测试失败';
    showError(normalizeError(error));
  } finally {
    networkBusy = false;
    try { renderNetworkSettings(await invoke('get_network_settings')); } catch {}
  }
}

async function refreshSettingsPage() {
  await Promise.all([refresh(), refreshPowerSettings(), refreshNetworkSettings()]);
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

function renderMonitorDevices(devices) {
  const host = $('monitorDevices');
  host.innerHTML = '';
  if (!devices.length) {
    const empty = document.createElement('p');
    empty.className = 'muted';
    empty.textContent = '暂无已配对设备。';
    host.append(empty);
    return;
  }
  for (const device of devices) {
    const row = document.createElement('div');
    row.className = 'monitor-device';
    const copy = document.createElement('div');
    const title = document.createElement('strong');
    title.textContent = String(device.name || device.id || 'Android Device');
    const meta = document.createElement('small');
    meta.textContent = `${String(device.id || '').slice(0, 18)} · 最近在线 ${ageLabel(device.lastSeenAt)}`;
    copy.append(title, meta);
    const remove = document.createElement('button');
    const deviceId = String(device.id || '');
    const confirming = pendingMonitorDeleteDeviceId === deviceId;
    remove.className = confirming ? 'ghost danger is-confirming' : 'ghost danger';
    remove.textContent = confirming ? '确认删除' : '删除';
    remove.disabled = monitorBusy;
    remove.addEventListener('click', () => deleteMonitorDevice(deviceId, remove));
    row.append(copy, remove);
    host.append(row);
  }
}

async function deleteMonitorDevice(deviceId, button) {
  if (!invoke || monitorBusy || !deviceId) return;
  if (pendingMonitorDeleteDeviceId !== deviceId) {
    pendingMonitorDeleteDeviceId = deviceId;
    if (button) {
      button.className = 'ghost danger is-confirming';
      button.textContent = '确认删除';
    }
    return;
  }

  pendingMonitorDeleteDeviceId = '';
  monitorBusy = true;
  showError('');
  if (button) {
    button.disabled = true;
    button.textContent = '删除中…';
  }

  let deleted = false;
  try {
    renderMonitorInfo(await invoke('revoke_monitor_device', { deviceId }));
    deleted = true;
  } catch (error) {
    if (button) {
      button.disabled = false;
      button.className = 'ghost danger';
      button.textContent = '删除失败';
    }
    showError(normalizeError(error));
  } finally {
    monitorBusy = false;
    if (deleted) await refreshMonitorPage();
  }
}

function renderMonitorInfo(payload) {
  const enabled = Boolean(payload?.enabled);
  const running = Boolean(payload?.running);
  const toggle = $('monitorEnabled');
  toggle.checked = enabled;
  toggle.disabled = monitorBusy;
  if (!monitorPortDirty && document.activeElement !== $('monitorPort')) {
    $('monitorPort').value = String(payload?.port || 18432);
  }
  $('saveMonitor').disabled = monitorBusy;
  $('createMonitorPairing').disabled = monitorBusy || !running;
  $('monitorBind').textContent = payload?.bindAddress || '—';
  $('monitorFingerprint').textContent = payload?.fingerprintSha256 || '—';
  $('monitorProtocol').textContent = `${payload?.protocol === 'chatx-monitor-wss-v1' ? 'WSS' : '—'} · ${payload?.encryption || 'E2EE'}`;
  renderMonitorDevices(payload?.devices || []);
  const status = $('monitorStatus');
  status.textContent = running ? '已运行' : enabled ? '未运行' : '已关闭';
  status.className = `setting-status ${running ? 'ok' : enabled ? 'bad' : 'off'}`;
}

async function refreshMonitorPage() {
  if (!invoke || monitorBusy) return;
  try {
    const [monitor, network] = await Promise.all([
      invoke('get_monitor_info'),
      invoke('get_network_settings'),
    ]);
    renderMonitorInfo(monitor);
    renderNetworkSettings(network);
  } catch (error) {
    showError(normalizeError(error));
  }
}

async function saveMonitorSettings() {
  if (!invoke || monitorBusy) return;
  const enabled = $('monitorEnabled').checked;
  const port = Number.parseInt($('monitorPort').value, 10);
  if (!Number.isFinite(port) || port < 1024 || port > 65535) {
    showError('Monitor 端口必须为 1024-65535。');
    return;
  }
  monitorBusy = true;
  showError('');
  try {
    renderMonitorInfo(await invoke('set_monitor_enabled', { enabled, port }));
    monitorPortDirty = false;
    $('monitorPairing').hidden = true;
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    monitorBusy = false;
    await refreshMonitorPage();
  }
}

async function createMonitorPairing() {
  if (!invoke || monitorBusy) return;
  monitorBusy = true;
  showError('');
  try {
    const pairing = await invoke('create_monitor_pairing');
    const { qrSvg, ...payload } = pairing || {};
    $('monitorPairingQr').innerHTML = qrSvg || '';
    $('monitorPairingPayload').textContent = JSON.stringify(payload, null, 2);
    $('monitorPairing').hidden = false;
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    monitorBusy = false;
    await refreshMonitorPage();
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
    : currentPage === 'monitor'
      ? refreshMonitorPage()
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
  $('proxyMode').addEventListener('change', () => {
    networkDirty = true;
    $('proxyUrl').disabled = $('proxyMode').value !== 'manual';
  });
  for (const id of ['proxyUrl', 'relayBaseUrl']) {
    $(id).addEventListener('input', () => { networkDirty = true; });
  }
  $('relayEnabled').addEventListener('change', () => { networkDirty = true; });
  $('saveNetwork').addEventListener('click', saveNetworkSettings);
  $('testNetwork').addEventListener('click', testNetworkSettings);
  $('saveRelay').addEventListener('click', saveRelaySettings);
  $('testRelay').addEventListener('click', testRelaySettings);
  $('registerRelay').addEventListener('click', registerRelayDesktop);
  $('monitorPort').addEventListener('input', () => { monitorPortDirty = true; });
  $('monitorEnabled').addEventListener('change', saveMonitorSettings);
  $('saveMonitor').addEventListener('click', saveMonitorSettings);
  $('createMonitorPairing').addEventListener('click', createMonitorPairing);
  $('copyMonitorPairing').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText($('monitorPairingPayload').textContent || ''); }
    catch (error) { showError(normalizeError(error)); }
  });
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
void refreshMcpLive();
setInterval(() => { void refreshMcpLive(); }, 1000);
setInterval(() => {
  if (busy || currentPage === 'diagnostics') return;
  if (currentPage === 'calls') void refreshCallHistory();
  else if (currentPage === 'monitor') void refreshMonitorPage();
  else if (currentPage === 'permissions') void refreshPermissionCenter();
  else if (currentPage === 'settings') void refreshSettingsPage();
  else void refresh();
}, 4000);
