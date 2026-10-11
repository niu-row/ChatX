const $ = (id) => document.getElementById(id);
const invoke = window.ChatXApi?.invoke;

let currentPage = 'overview';
let state = null;
let busy = false;
let actionError = '';
let rememberKeyDirty = false;
let autoReconnectBusy = false;
let chosenConnectionMode = null;
let connectionModeTouched = false;
let currentSettingsTab = 'connection';
let macPlatform = false;

function setSettingsTab(tab) {
  if (!['connection', 'security', 'network', 'relay', 'system'].includes(tab)) return;
  if (tab === 'system' && !macPlatform) return;
  if (currentSettingsTab === 'security' && tab !== 'security') httpsMcpPage.hidePassword();
  currentSettingsTab = tab;
  for (const button of document.querySelectorAll('[data-settings-tab]')) {
    const selected = button.dataset.settingsTab === tab;
    button.classList.toggle('active', selected);
    button.setAttribute('aria-selected', String(selected));
  }
  for (const panel of document.querySelectorAll('[data-settings-panel]')) {
    panel.hidden = panel.dataset.settingsPanel !== tab;
  }
  if (currentPage === 'settings') void refreshSettingsPage();
}

function showConnectionMode(mode, fromUser = false) {
  if (mode !== 'https' && mode !== 'tunnel') return;
  if (fromUser) connectionModeTouched = true;
  chosenConnectionMode = mode;
  $('httpsConnectionPanel').hidden = mode !== 'https';
  $('tunnelConnectionPanel').hidden = mode !== 'tunnel';
  for (const button of document.querySelectorAll('[data-connection-choice]')) {
    const selected = button.dataset.connectionChoice === mode;
    button.classList.toggle('active', selected);
    button.setAttribute('aria-pressed', String(selected));
  }
}

const uiState = window.ChatXUiState;
function beginRefresh(channel) {
  return uiState.beginRefresh(channel);
}
function isLatestRefresh(channel, version) {
  return uiState.isLatestRefresh(channel, version);
}

const pages = {
  overview: ['概览', 'ChatGPT → HTTPS MCP / Secure MCP Tunnel → Desktop Commander'],
  connection: ['连接', '选择一种 MCP 连接模式；启动新链路时自动停止旧链路。'],
  diagnostics: ['诊断', '检查运行环境，并查看本机日志与错误记录。'],
  calls: ['调用记录', '查看 ChatGPT 通过 MCP 调用 Desktop Commander 的最近记录。'],
  monitor: ['手机连接', '通过 Direct / Relay 端到端加密监控当前 HTTPS MCP 或 Secure Tunnel。'],
  settings: ['设置', '管理连接策略、OAuth 授权、网络与手机 Relay。'],
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
  if (page !== 'monitor') monitorPage.resetPendingDelete();
  if (page !== 'settings') httpsMcpPage.hidePassword();
  currentPage = page;
  for (const button of document.querySelectorAll('[data-page]')) {
    button.classList.toggle('active', button.dataset.page === page);
  }
  for (const panel of document.querySelectorAll('[data-page-panel]')) {
    panel.classList.toggle('active', panel.dataset.pagePanel === page);
  }
  $('pageTitle').textContent = pages[page][0];
  $('pageSubtitle').textContent = pages[page][1];
  if (page === 'connection') void httpsMcpPage.refresh();
  if (page === 'diagnostics') void refresh();
  if (page === 'calls') void refreshCallHistory();
  if (page === 'monitor') void refreshMonitorPage();
  if (page === 'settings') void refreshSettingsPage();
}

function runtimeRunning(s) {
  if (typeof s?.runtimeActive === 'boolean') return s.runtimeActive;
  return ['ready', 'running', 'healthy', 'connected', 'live'].includes(String(s?.runtimeState || '').toLowerCase());
}

function render(s) {
  state = s;
  if (typeof s?.platform === 'string') {
    macPlatform = s.platform.startsWith('macos');
    for (const item of document.querySelectorAll('.platform-macos-only')) {
      item.hidden = !macPlatform;
    }
    if (!macPlatform && currentSettingsTab === 'system') setSettingsTab('connection');
  }
  const activeMode = s.activeMcpMode || (s.httpsMcp?.enabled ? 'https' : s.desiredConnected ? 'tunnel' : 'none');
  if (!connectionModeTouched || !chosenConnectionMode) {
    showConnectionMode(activeMode === 'none' ? 'https' : activeMode);
  }
  const modeBadge = $('activeConnectionMode');
  if (modeBadge) {
    modeBadge.textContent = activeMode === 'https' ? '当前：HTTPS MCP'
      : activeMode === 'tunnel' ? '当前：Secure Tunnel' : '目前无连接';
    modeBadge.className = `setting-status ${activeMode === 'none' ? 'off' : 'ok'}`;
  }
  const running = runtimeRunning(s);
  const httpsEnabled = Boolean(s.httpsMcp?.enabled);
  // sshConnected is deliberately false when HTTPS MCP uses WSS. Use the
  // transport-neutral status, with a fallback for older status snapshots.
  const transportConnected = Boolean(s.httpsMcp?.transportConnected ?? s.httpsMcp?.sshConnected);
  const httpsConnected = httpsEnabled && !s.httpsMcp?.manualStopBlocked &&
    Boolean(s.httpsMcp?.localReady && transportConnected && s.httpsMcp?.remoteReady);
  const httpsTransportName = s.httpsMcp?.transport === 'wss' ? 'WSS Relay' : 'SSH 反向隧道';
  const connected = running || httpsConnected;
  const unavailable = s.runtimeState === 'unavailable';
  const reconnecting = Boolean(s.reconnecting);
  const reconnectPhase = String(s.reconnectPhase || 'idle').toLowerCase();
  const recoveryBusy = reconnecting || ['detecting', 'reconnecting', 'verifying'].includes(reconnectPhase);
  const recoveryWaiting = reconnectPhase === 'backoff';
  const recoveryBlocked = ['blocked', 'failed'].includes(reconnectPhase);
  const nextReconnectSeconds = Math.max(0, Math.ceil((Number(s.nextReconnectAt || 0) - Date.now()) / 1000));
  const wantsConnection = Boolean(s.desiredConnected);
  const runtimeHealth = String(s.runtimeHealth || '').toLowerCase();
  const degraded = wantsConnection && ['suspect', 'starting'].includes(runtimeHealth);
  const down = wantsConnection && runtimeHealth === 'down';
  const canStopTunnel = running || recoveryBusy || recoveryWaiting || wantsConnection;
  const canStop = canStopTunnel || httpsEnabled;
  const reconnectAttempt = Number(s.reconnectAttempt || 0);
  const recoveryLabel = reconnectPhase === 'detecting'
    ? '正在确认 Tunnel 异常'
    : reconnectPhase === 'verifying'
      ? '正在验证 Tunnel Ready'
      : reconnectPhase === 'backoff'
        ? `等待自动重连${nextReconnectSeconds ? ` · ${nextReconnectSeconds}s` : ''}`
        : reconnectPhase === 'blocked'
          ? '自动重连受阻'
          : reconnectPhase === 'failed'
            ? '自动重连失败'
            : `正在自动重连${reconnectAttempt ? ` · 第 ${reconnectAttempt} 次` : ''}`;
  const recoveryCopy = recoveryWaiting
    ? `${s.lastReconnectError || '上一轮自动重连未恢复 Ready。'}${nextReconnectSeconds ? ` ${nextReconnectSeconds} 秒后再次尝试。` : ' 正在等待下一次尝试。'}`
    : recoveryBlocked
      ? (s.lastReconnectError || 'ChatX 当前无法自动恢复 Tunnel。')
      : reconnectPhase === 'verifying'
        ? '重连命令已经完成，ChatX 正在确认 runtime、Ready 与 Control Plane 健康状态。'
        : 'ChatX 已检测到 Tunnel 掉线，正在后台恢复连接；无需重新输入配置。';

  $('sidebarDot').className = `dot ${connected ? 'ok' : unavailable || down ? 'bad' : 'warn'}`;
  $('sidebarStatus').textContent = httpsConnected
    ? 'HTTPS MCP 已连接'
    : running
      ? '已连接 ChatGPT'
    : recoveryBusy || recoveryWaiting || recoveryBlocked
      ? recoveryLabel
      : degraded
        ? 'Tunnel 连接异常 · 检查中'
        : down
          ? 'Tunnel 已断开'
          : unavailable ? '运行组件缺失' : '未连接';

  $('heroTitle').textContent = httpsConnected
    ? 'HTTPS MCP 已就绪 · ChatGPT 可接入'
    : running
      ? 'ChatGPT 已连接本机'
    : recoveryBusy || recoveryWaiting || recoveryBlocked ? recoveryLabel
      : degraded ? 'Tunnel 连接正在检查'
        : down ? 'Tunnel 连接已中断'
          : s.configured ? '连接已配置' : '配置一次即可连接';
  $('heroCopy').textContent = httpsConnected
    ? `HTTPS 与 ${httpsTransportName} 正在运行。请在 ChatGPT 自定义应用中使用 ${s.httpsMcp?.endpoint || 'HTTPS /mcp'}，通过 OAuth 授权后访问 Desktop Commander。`
    : running
      ? 'Secure MCP Tunnel 正在把 ChatGPT 的 MCP 调用转发给 Desktop Commander。'
    : recoveryBusy || recoveryWaiting || recoveryBlocked
      ? recoveryCopy
      : degraded
        ? '本地 runtime 仍在运行，但 Control Plane 最近出现连续 poll 异常；ChatX 正在持续检测。'
        : down
          ? 'ChatX 已确认 Control Plane 或本地 runtime 不可用；可等待自动重连或手动处理。'
          : '输入 Tunnel ID 和 Runtime API Key 后，ChatX 会直接启动 Desktop Commander 的 stdio MCP。';
  $('heroConnect').textContent = httpsConnected ? '查看 HTTPS MCP' : running ? '查看连接' : recoveryBusy ? '自动重连中' : degraded || down || recoveryWaiting || recoveryBlocked ? '查看连接状态' : '配置连接';
  $('heroStop').disabled = busy || !canStop;
  $('heroStop').textContent = !canStopTunnel && httpsEnabled ? '停止 HTTPS MCP' : '停止连接';

  $('tunnelFact').textContent = running
    ? 'Ready'
    : recoveryBusy
      ? reconnectPhase === 'verifying' ? 'Verifying' : 'Reconnecting'
      : recoveryWaiting
        ? 'Backoff'
        : recoveryBlocked
          ? 'Recovery blocked'
          : degraded
            ? 'Suspect'
            : down
              ? 'Down'
              : String(s.runtimeState || 'Stopped');
  $('transportFactLabel').textContent = httpsConnected ? `HTTPS MCP · ${httpsTransportName}` : 'Secure MCP Tunnel';
  if (httpsConnected) $('tunnelFact').textContent = 'Ready';
  $('tunnelVersion').textContent = httpsConnected ? (s.httpsMcp.endpoint || 'HTTPS MCP') : (s.tunnelVersion || 'tunnel-client unavailable');
  $('overviewTransportName').textContent = httpsConnected ? `HTTPS MCP · ${httpsTransportName}` : 'Secure MCP Tunnel';
  $('overviewTransportDescription').textContent = httpsConnected ? (s.httpsMcp.baseUrl || 'newrow.fun') : 'OpenAI';
  $('dcFact').textContent = unavailable ? 'Unavailable' : 'Bundled';
  $('dcVersion').textContent = `v${s.desktopCommander?.version || 'unknown'}`;
  $('keyFactLabel').textContent = httpsConnected ? 'HTTPS OAuth' : 'Runtime Key';
  $('keyFact').textContent = httpsConnected ? '已启用' : s.runtimeKeySaved ? '已保存' : '未保存';
  const sessionOnly = s.keyStorage === 'session only';
  $('keyStorageHint').textContent = httpsConnected ? '密码由本机安全密钥派生' : sessionOnly ? '仅本次输入' : `${s.keyStorage} / 本次输入`;
  $('rememberKeyText').textContent = sessionOnly ? '当前平台不支持保存 Runtime Key' : `使用 ${s.keyStorage === 'macOS Keychain' ? 'macOS 钥匙串' : s.keyStorage} 保存 Runtime Key`;

  if (s.tunnelId && document.activeElement !== $('tunnelId')) $('tunnelId').value = s.tunnelId;
  $('rememberKey').disabled = sessionOnly;
  if (!rememberKeyDirty) $('rememberKey').checked = sessionOnly ? false : Boolean(s.rememberKey || s.runtimeKeySaved || !s.configured);
  $('runtimeKey').placeholder = s.runtimeKeySaved ? '已安全保存，可留空' : '输入 Runtime API Key';
  $('clearKey').disabled = busy || !s.runtimeKeySaved;
  $('connect').disabled = busy || unavailable || running || recoveryBusy;
  $('stop').disabled = busy || !canStopTunnel;
  $('connect').textContent = running ? '已连接' : recoveryBusy ? '自动重连中' : '连接并启动';
  $('mcpCommand').textContent = s.mcpCommand || '运行组件准备完成后显示 MCP command。';

  const autoReconnect = $('autoReconnect');
  const autoReconnectEnabled = s.autoReconnect !== false;
  autoReconnect.checked = autoReconnectEnabled;
  autoReconnect.disabled = busy || autoReconnectBusy;
  const reconnectStatus = $('reconnectStatus');
  reconnectStatus.textContent = recoveryBusy || recoveryWaiting || recoveryBlocked
    ? recoveryLabel
    : autoReconnectEnabled ? '已开启' : '已关闭';
  reconnectStatus.className = `setting-status ${recoveryBusy || recoveryWaiting || recoveryBlocked ? 'warn' : autoReconnectEnabled ? 'ok' : 'off'}`;

  const logs = Array.isArray(s.logs) ? s.logs : [];
  $('logs').textContent = logs.length ? logs.join('\n') : '尚无日志。';
  $('logs').scrollTop = $('logs').scrollHeight;

  const hasRuntimeError = !recoveryBusy && ['error', 'unavailable'].includes(String(s.runtimeState || '').toLowerCase());
  displayError(actionError || s.trayStopError || (s.lastError && hasRuntimeError ? s.lastError : ''));
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
  const transports = payload?.transports || {};
  const transportLine = $('mcpTransportStatuses');
  if (transportLine) {
    const label = (name, value) => `${name}：${value?.sourceUpdatedAt == null ? '未记录' : String(value.state || 'idle').toUpperCase()}`;
    transportLine.textContent = [label('Secure Tunnel', transports.tunnel),
      label('HTTPS MCP', transports.httpsMcp)].join(' · ');
  }
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
    const source = document.createElement('span');
    source.className = 'call-source';
    source.textContent = String(call.source || 'Secure MCP Tunnel');
    const duration = document.createElement('span');
    duration.textContent = liveDuration(call.durationMs);
    row.append(tool, source, duration);
    host.append(row);
  }
}

async function refreshMcpLive() {
  if (!invoke || currentPage !== 'overview') return;
  const version = beginRefresh('mcpLive');
  try {
    const payload = await invoke('get_mcp_live_status');
    if (isLatestRefresh('mcpLive', version)) renderMcpLive(payload);
  } catch {
    /* live MCP display is observational only */
  }
}

async function refresh() {
  if (!invoke) {
    showError('当前页面不是由 ChatX Tauri 应用启动。');
    return;
  }
  const version = beginRefresh('status');
  try {
    const payload = await invoke('get_status');
    if (isLatestRefresh('status', version)) render(payload);
  } catch (error) {
    if (isLatestRefresh('status', version)) {
      displayError(actionError || normalizeError(error));
    }
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
    // Stopping from the overview must close both transports, even when an old
    // Tunnel connection intent is still persisted alongside HTTPS MCP.
    render(await invoke('stop_all_connections'));
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

const httpsMcpPage = window.ChatXPages.createHttpsMcpPage({ $, invoke, showError, normalizeError });

const callHistoryPage = window.ChatXPages.createCallHistoryPage({
  $,
  invoke,
  normalizeError,
});
const refreshCallHistory = () => callHistoryPage.refresh();
const clearCallHistory = () => callHistoryPage.clear();

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

const permissionsPage = window.ChatXPages.createPermissionsPage({
  $,
  invoke,
  normalizeError,
  showError,
  beginRefresh,
  isLatestRefresh,
});
const refreshPermissionCenter = () => permissionsPage.refresh();
const requestAllPermissions = () => permissionsPage.requestAll();
const openFullDiskAccess = () => permissionsPage.openFullDiskAccess();

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

const networkPage = window.ChatXPages.createNetworkPage({
  $,
  invoke,
  normalizeError,
  showError,
  beginRefresh,
  isLatestRefresh,
  ageLabel,
});
const renderNetworkSettings = (payload) => networkPage.render(payload);
const refreshNetworkSettings = () => networkPage.refresh();
const saveNetworkSettings = () => networkPage.saveNetwork();
const saveRelaySettings = () => networkPage.saveRelay();
const registerRelayDesktop = () => networkPage.registerRelay();
const testNetworkSettings = () => networkPage.testNetwork();
const testRelaySettings = () => networkPage.testRelay();

async function refreshSettingsPage() {
  if (currentPage !== 'settings') return;
  switch (currentSettingsTab) {
    case 'connection':
      await Promise.all([refresh(), httpsMcpPage.refresh()]);
      break;
    case 'security':
      await Promise.all([
        httpsMcpPage.refreshSecurity(),
        ...(macPlatform ? [refreshPermissionCenter()] : []),
      ]);
      break;
    case 'network':
    case 'relay':
      await refreshNetworkSettings();
      break;
    case 'system':
      if (macPlatform) await refreshPowerSettings();
      break;
  }
}

const powerPage = window.ChatXPages.createPowerPage({
  $,
  invoke,
  normalizeError,
  showError,
  beginRefresh,
  isLatestRefresh,
});
const refreshPowerSettings = () => powerPage.refresh();
const setClamshellAwake = () => powerPage.setEnabled();

const monitorPage = window.ChatXPages.createMonitorPage({
  $,
  invoke,
  normalizeError,
  showError,
  beginRefresh,
  isLatestRefresh,
  ageLabel,
  renderNetworkSettings,
});
const renderMonitorInfo = (payload) => monitorPage.render(payload);
const refreshMonitorPage = () => monitorPage.refresh();
const saveMonitorSettings = () => monitorPage.save();
const createMonitorPairing = () => monitorPage.createPairing();
const deleteMonitorDevice = (deviceId, button) =>
  monitorPage.deleteDevice(deviceId, button);

async function openExternal(url) {
  try { await invoke('open_external', { url }); }
  catch (error) { showError(normalizeError(error)); }
}

function bind() {
  $('rememberKey').addEventListener('change', () => { rememberKeyDirty = true; });
  for (const button of document.querySelectorAll('[data-settings-tab]')) {
    button.addEventListener('click', () => setSettingsTab(button.dataset.settingsTab));
  }
  $('openHttpsSecurity').addEventListener('click', () => {
    setSettingsTab('security');
    setPage('settings');
  });
  $('openRelaySettings').addEventListener('click', () => {
    setSettingsTab('relay');
    setPage('settings');
  });
  for (const button of document.querySelectorAll('[data-connection-choice]')) {
    button.addEventListener('click', () => showConnectionMode(button.dataset.connectionChoice, true));
  }
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
  $('proxyMode').addEventListener(
    'change',
    networkPage.proxyModeChanged,
  );
  $('proxyUrl').addEventListener('input', networkPage.markProxyDirty);
  $('relayBaseUrl').addEventListener('input', networkPage.markRelayDirty);
  $('relayEnabled').addEventListener('change', networkPage.markRelayDirty);
  $('saveNetwork').addEventListener('click', saveNetworkSettings);
  $('testNetwork').addEventListener('click', testNetworkSettings);
  $('saveRelay').addEventListener('click', saveRelaySettings);
  $('testRelay').addEventListener('click', testRelaySettings);
  $('registerRelay').addEventListener('click', registerRelayDesktop);
  $('monitorPort').addEventListener('input', monitorPage.markPortDirty);
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
// A tray action has no promise rejection path in the webview; surface errors
// here rather than leaving a failed manual stop visible only in log files.
if (window.__TAURI__?.event?.listen) {
  void window.__TAURI__.event.listen('chatx-stop-error', ({ payload }) => {
    showError(String(payload || '停止连接失败'));
    setPage('overview');
  }).catch((error) => showError(normalizeError(error)));
}
void refresh();
void refreshMcpLive();
setInterval(() => { void refreshMcpLive(); }, 1000);
setInterval(() => {
  if (busy) return;
  if (currentPage === 'connection') void httpsMcpPage.refresh();
  if (currentPage === 'calls') void refreshCallHistory();
  else if (currentPage === 'monitor') void refreshMonitorPage();
  else if (currentPage === 'settings') void refreshSettingsPage();
  else void refresh();
}, 4000);
