const $ = (id) => document.getElementById(id);
const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke;

let state = null;
let busy = false;
let currentPage = 'overview';
let credentialsDirty = false;

const pageMeta = {
  overview: ['概览', '查看连接、权限和本地服务状态。'],
  connection: ['连接', '配置并启动 OpenAI Secure MCP Tunnel。'],
  access: ['权限', '设置访问模式和 ChatGPT 可访问的本地目录。'],
  diagnostics: ['诊断', '检查 MCP、Tunnel、Git 与本地环境。'],
  invocations: ['调用日志', '查看最近的 MCP 工具调用、结果与耗时。'],
  guide: ['使用教程', '按步骤完成第一次 ChatX 设置。'],
};

const permissionMeta = [
  ['filesystemRead', '读取文件', '目录列表、读取、搜索和元数据'],
  ['filesystemWrite', '修改文件', '写入、编辑、复制、移动和删除'],
  ['gitRead', 'Git 读取', 'status、diff、log'],
  ['gitWrite', 'Git 写入', '受约束的 stage、unstage、branch、commit'],
  ['gitAdvanced', '高级 Git', '任意 git 参数；高风险，默认关闭'],
  ['shell', 'Shell 命令', '高权限；不受允许目录边界约束'],
  ['fullAccess', '完整文件系统访问', '绕过允许目录边界'],
];

function showError(message) {
  $('error').hidden = !message;
  $('error').textContent = message || '';
}

function normalizeError(error) {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  try { return JSON.stringify(error); } catch { return String(error); }
}

function setPage(page) {
  if (!pageMeta[page]) return;
  currentPage = page;
  for (const button of document.querySelectorAll('[data-page]')) {
    button.classList.toggle('active', button.dataset.page === page);
  }
  for (const panel of document.querySelectorAll('[data-page-panel]')) {
    panel.classList.toggle('active', panel.dataset.pagePanel === page);
  }
  $('pageTitle').textContent = pageMeta[page][0];
  $('pageSubtitle').textContent = pageMeta[page][1];
  if (page === 'invocations') void refreshInvocations();
  window.scrollTo({ top: 0, behavior: 'smooth' });
}

function bindNavigation() {
  for (const button of document.querySelectorAll('[data-page]')) {
    button.addEventListener('click', () => setPage(button.dataset.page));
  }
  for (const button of document.querySelectorAll('[data-go-page]')) {
    button.addEventListener('click', () => setPage(button.dataset.goPage));
  }
  $('openGuide').addEventListener('click', () => setPage('guide'));
  $('heroConnect').addEventListener('click', () => setPage('connection'));
  $('heroDiagnose').addEventListener('click', async () => {
    setPage('diagnostics');
    await runDiagnostics();
  });
}

async function backend(method, path, body) {
  if (!invoke) throw new Error('当前页面不是由 Tauri 启动，无法调用桌面后端。');
  return await invoke('backend_request', { method, path, body: body ?? null });
}

function bindCredentialMirrors() {
  const textPairs = [
    ['tunnelId', 'guideTunnelIdInput', true],
    ['apiKey', 'guideApiKeyInput', false],
  ];

  for (const [leftId, rightId, marksDirty] of textPairs) {
    const left = $(leftId);
    const right = $(rightId);
    const sync = (source, target) => {
      target.value = source.value;
      if (marksDirty) credentialsDirty = true;
    };
    left.addEventListener('input', () => sync(left, right));
    right.addEventListener('input', () => sync(right, left));
  }

  const remember = $('rememberKey');
  const guideRemember = $('guideRememberKey');
  remember.addEventListener('change', () => { guideRemember.checked = remember.checked; });
  guideRemember.addEventListener('change', () => { remember.checked = guideRemember.checked; });
}

async function refresh() {
  try {
    const payload = await backend('GET', '/api/tunnel/status');
    render(payload);
  } catch (error) {
    $('sidebarDot').className = 'status-dot bad';
    $('sidebarStatus').textContent = '后端不可用';
    showError(normalizeError(error));
  }
}

async function updateSettings(patch) {
  busy = true;
  try {
    const payload = await backend('POST', '/api/settings', patch);
    render(payload.status);
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    busy = false;
    await refresh();
  }
}

function renderPermissionRows(permissions) {
  const host = $('permissionList');
  host.innerHTML = '';
  permissionMeta.forEach(([key, title, description]) => {
    const row = document.createElement('label');
    row.className = 'perm';

    const copy = document.createElement('span');
    const strong = document.createElement('strong');
    strong.textContent = title;
    const small = document.createElement('small');
    small.textContent = description;
    copy.append(strong, small);

    const input = document.createElement('input');
    input.type = 'checkbox';
    input.checked = Boolean(permissions[key]);
    input.disabled = busy;
    input.addEventListener('change', async () => {
      if ((key === 'fullAccess' || key === 'gitAdvanced' || key === 'shell') && input.checked) {
        const ok = window.confirm('这是高权限选项。确定启用？');
        if (!ok) {
          input.checked = false;
          return;
        }
      }
      await updateSettings({ [key]: input.checked });
    });

    row.append(copy, input);
    host.append(row);
  });
}

function renderRoots(roots) {
  const host = $('rootList');
  host.innerHTML = '';

  if (!roots?.length) {
    const empty = document.createElement('div');
    empty.className = 'root-empty';
    empty.textContent = '尚未配置允许目录。点击“选择文件夹”开始添加。';
    host.append(empty);
    return;
  }

  roots.forEach((root) => {
    const row = document.createElement('div');
    row.className = 'root-item';

    const path = document.createElement('div');
    path.className = 'root-path';
    path.title = root;
    path.textContent = root;

    const remove = document.createElement('button');
    remove.className = 'danger subtle-danger';
    remove.type = 'button';
    remove.textContent = '移除';
    remove.disabled = busy || roots.length <= 1;
    remove.addEventListener('click', async () => {
      const next = roots.filter((item) => item !== root);
      if (!next.length) return;
      await updateSettings({ roots: next });
    });

    row.append(path, remove);
    host.append(row);
  });
}

function setStepState(id, text, mode = '') {
  const node = $(id);
  node.textContent = text;
  node.className = `step-state${mode ? ` ${mode}` : ''}`;
}

function renderOverview(s, tunnel, running) {
  const configuredTunnel = Boolean(s.connection.tunnelId);
  const hasRoots = Boolean(s.policy.fullAccess || s.policy.roots?.length);
  const permissions = s.policy.permissions || {};

  $('heroTitle').textContent = running ? 'ChatX 已连接' : 'ChatX 本地服务已就绪';
  $('heroCopy').textContent = running
    ? 'Secure Tunnel 正在运行，ChatGPT 可以通过 MCP 调用已授权的本机能力。'
    : configuredTunnel
      ? 'Tunnel 已配置。启动 Secure Tunnel 后即可让 ChatGPT 连接本机 MCP。'
      : '先配置 Tunnel ID、Runtime Key 和允许目录，然后启动 Secure Tunnel。';

  $('heroConnect').textContent = running ? '查看连接状态' : '连接 Tunnel';
  $('heroConnect').onclick = () => setPage('connection');

  $('fullAccessFact').textContent = permissions.fullAccess ? '已开启' : '关闭';
  $('shellFact').textContent = permissions.shell ? '已开启' : '关闭';

  setStepState('stepConnection', configuredTunnel ? '已配置' : '未配置', configuredTunnel ? 'done' : 'active');
  setStepState('stepRoots', hasRoots ? (s.policy.fullAccess ? '全部路径' : `${s.policy.roots.length} 个`) : '未配置', hasRoots ? 'done' : 'active');
  setStepState('stepTunnel', running ? '已连接' : '未连接', running ? 'done' : 'active');
}

function render(payload) {
  const s = payload.status || payload;
  state = s;
  const tunnel = s.tunnel;
  const running = tunnel.state === 'running';
  const connecting = tunnel.state === 'connecting';
  const permissions = s.policy.permissions || {};

  $('service').textContent = `${s.service.name} ${s.service.version}`;
  $('tunnel').textContent = `${tunnel.installed ? tunnel.version.split(' ')[0] : '未安装'} · ${tunnel.state}`;
  $('presetFact').textContent = s.policy.permissionPreset;
  $('keyFact').textContent = s.connection.runtimeKeySaved
    ? 'DPAPI 已保存'
    : (s.connection.runtimeKeySupported ? '未保存' : '不支持持久化');
  $('rootsFact').textContent = s.policy.fullAccess ? '全部路径' : `${s.policy.roots.length} 个目录`;
  $('rootsFact').title = s.policy.roots.join('; ');
  $('settingsVersion').textContent = String(s.settings.version);
  $('preset').value = s.policy.permissionPreset;

  renderPermissionRows(permissions);
  renderRoots(s.policy.roots || []);
  renderOverview(s, tunnel, running);

  if (!credentialsDirty && s.connection.tunnelId) {
    $('tunnelId').value = s.connection.tunnelId;
    $('guideTunnelIdInput').value = s.connection.tunnelId;
  }
  $('settingsFile').textContent = `设置文件：${s.connection.settingsFile}`;

  $('rememberKey').disabled = !s.connection.runtimeKeySupported || busy;
  $('guideRememberKey').disabled = !s.connection.runtimeKeySupported || busy;
  $('clearKey').disabled = !s.connection.runtimeKeySaved || busy;
  $('chooseFolders').disabled = busy || Boolean(s.policy.fullAccess);
  $('keyHelp').textContent = s.connection.runtimeKeySupported
    ? (s.connection.runtimeKeySaved
      ? '已使用 Windows DPAPI（CurrentUser）加密保存；API Key 可留空直接重连。'
      : '可选择用 Windows DPAPI（CurrentUser）加密保存，不写入 settings.json。')
    : '当前平台不支持安全持久化，Key 仅用于本次操作。';

  const keyPlaceholder = s.connection.runtimeKeySaved ? '已安全保存，可留空' : '输入 Runtime API Key';
  $('apiKey').placeholder = keyPlaceholder;
  $('guideApiKeyInput').placeholder = keyPlaceholder;

  const connectDisabled = busy || running || connecting || !tunnel.installed;
  const stopDisabled = busy || !running;
  const connectText = connecting ? '正在连接…' : (running ? '已连接' : '连接并启动');
  $('connect').disabled = connectDisabled;
  $('guideConnect').disabled = connectDisabled;
  $('stop').disabled = stopDisabled;
  $('guideStop').disabled = stopDisabled;
  $('connect').textContent = connectText;
  $('guideConnect').textContent = connectText;

  for (const id of ['connectionBadge', 'guideConnectionBadge']) {
    const badge = $(id);
    badge.textContent = running ? '已连接' : (connecting ? '连接中' : '未连接');
    badge.className = `connection-badge${running ? ' running' : connecting ? ' connecting' : ''}`;
  }

  $('sidebarDot').className = `status-dot ${running ? 'ok' : 'warn'}`;
  $('sidebarStatus').textContent = running ? 'Tunnel 已连接' : '本地服务正常';

  $('log').textContent = s.logs?.length ? s.logs.join('\n') : '尚无操作。';
  $('log').scrollTop = $('log').scrollHeight;
  showError(tunnel.lastError || '');
}

async function chooseFolders() {
  if (!invoke) return;
  busy = true;
  try {
    const picked = await invoke('pick_folders');
    if (!Array.isArray(picked) || picked.length === 0) return;

    const current = state?.policy?.roots || [];
    const seen = new Set(current.map((value) => value.toLowerCase()));
    const merged = [...current];
    for (const folder of picked) {
      const key = String(folder).toLowerCase();
      if (!seen.has(key)) {
        seen.add(key);
        merged.push(String(folder));
      }
    }
    await updateSettings({ roots: merged });
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    busy = false;
    await refresh();
  }
}

async function openExternal(url) {
  try {
    await invoke('open_external', { url });
  } catch (error) {
    showError(normalizeError(error));
  }
}

function formatInvocationTime(value) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value || '-');
  return date.toLocaleTimeString('zh-CN', { hour12: false });
}

async function refreshInvocations() {
  const rows = $('invocationRows');
  if (!rows) return;
  const limit = Math.max(1, Number.parseInt($('invocationLimit').value, 10) || 50);
  rows.innerHTML = '<tr><td colspan="4" class="empty-cell">正在读取调用日志…</td></tr>';
  try {
    const payload = await backend('GET', '/api/invocations');
    const entries = Array.isArray(payload.entries) ? payload.entries.slice(0, limit) : [];
    $('invocationCount').textContent = `${entries.length} 条调用`;
    rows.innerHTML = '';
    if (!entries.length) {
      rows.innerHTML = '<tr><td colspan="4" class="empty-cell">尚无调用记录。开始在 ChatGPT 中使用 @chatx 后，这里会显示工具调用。</td></tr>';
      return;
    }
    for (const entry of entries) {
      const row = document.createElement('tr');
      const time = document.createElement('td');
      time.textContent = formatInvocationTime(entry.startedAt);
      time.title = String(entry.startedAt || '');
      const tool = document.createElement('td');
      const toolCode = document.createElement('code');
      toolCode.textContent = String(entry.tool || '-');
      tool.append(toolCode);
      const status = document.createElement('td');
      const badge = document.createElement('span');
      badge.className = `call-status ${entry.status === 'ok' ? 'ok' : 'bad'}`;
      badge.textContent = entry.status === 'ok' ? '成功' : '失败';
      status.append(badge);
      const duration = document.createElement('td');
      duration.textContent = `${Number(entry.durationMs) || 0} ms`;
      row.append(time, tool, status, duration);
      rows.append(row);
    }
  } catch (error) {
    $('invocationCount').textContent = '读取失败';
    rows.innerHTML = '';
    const row = document.createElement('tr');
    const cell = document.createElement('td');
    cell.colSpan = 4;
    cell.className = 'empty-cell bad';
    cell.textContent = normalizeError(error);
    row.append(cell);
    rows.append(row);
  }
}

async function runDiagnostics() {
  const host = $('diagnostics');
  host.innerHTML = '<div class="empty-state">正在诊断…</div>';
  try {
    const data = await backend('GET', '/api/diagnostics');
    host.innerHTML = '';
    for (const check of data.checks || []) {
      const row = document.createElement('div');
      row.className = 'check';
      const name = document.createElement('span');
      name.textContent = check.name;
      const value = document.createElement('span');
      value.className = check.status === 'ok' ? 'ok' : (check.status === 'warn' ? 'warn' : 'bad');
      value.textContent = check.message;
      row.append(name, value);
      host.append(row);
    }
  } catch (error) {
    host.innerHTML = '';
    const node = document.createElement('div');
    node.className = 'empty-state';
    node.textContent = normalizeError(error);
    host.append(node);
  }
}

async function connectTunnel() {
  busy = true;
  showError('');
  try {
    const payload = await backend('POST', '/api/tunnel/connect', {
      tunnelId: $('tunnelId').value.trim(),
      apiKey: $('apiKey').value,
      rememberKey: $('rememberKey').checked,
    });
    credentialsDirty = false;
    $('apiKey').value = '';
    $('guideApiKeyInput').value = '';
    render(payload.status);
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    busy = false;
    await refresh();
  }
}

async function stopTunnel() {
  busy = true;
  try {
    const payload = await backend('POST', '/api/tunnel/stop', {});
    render(payload.status);
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    busy = false;
    await refresh();
  }
}

$('connect').addEventListener('click', connectTunnel);
$('guideConnect').addEventListener('click', connectTunnel);
$('stop').addEventListener('click', stopTunnel);
$('guideStop').addEventListener('click', stopTunnel);

$('clearKey').addEventListener('click', async () => {
  if (!window.confirm('清除本机安全保存的 Runtime Key？')) return;
  busy = true;
  try {
    const payload = await backend('POST', '/api/tunnel/key/clear', {});
    render(payload.status);
  } catch (error) {
    showError(normalizeError(error));
  } finally {
    busy = false;
    await refresh();
  }
});

$('applyPreset').addEventListener('click', async () => {
  const preset = $('preset').value;
  if (preset === 'custom') return;
  if (preset === 'unrestricted' && !window.confirm('“完全开放”会启用 Shell、完整文件系统访问和高级 Git。确定应用？')) return;
  await updateSettings({ preset });
});

$('chooseFolders').addEventListener('click', chooseFolders);
$('diagnose').addEventListener('click', runDiagnostics);
$('refreshInvocations').addEventListener('click', refreshInvocations);
$('invocationLimit').addEventListener('change', refreshInvocations);

$('getTunnelId').addEventListener('click', () => openExternal('https://platform.openai.com/settings/organization/tunnels'));
$('getRuntimeKey').addEventListener('click', () => openExternal('https://platform.openai.com/settings/organization/api-keys'));
$('guideOpenTunnelPage').addEventListener('click', () => openExternal('https://platform.openai.com/settings/organization/tunnels'));
$('guideOpenRuntimeKeyPage').addEventListener('click', () => openExternal('https://platform.openai.com/settings/organization/api-keys'));
$('guideChatGPTPlugins').addEventListener('click', () => openExternal('https://chatgpt.com/plugins'));
$('guideDeveloperModeDocs').addEventListener('click', () => openExternal('https://help.openai.com/en/articles/12584461-developer-mode-and-full-mcp-connectors-in-chatgpt-beta'));

let refreshTimer = null;
let refreshLoopRunning = false;

async function refreshLoop() {
  if (refreshLoopRunning) return;
  refreshLoopRunning = true;
  try {
    if (!document.hidden) {
      await refresh();
      if (currentPage === 'invocations') await refreshInvocations();
    }
  } finally {
    refreshLoopRunning = false;
    refreshTimer = setTimeout(refreshLoop, document.hidden ? 15000 : 2500);
  }
}

document.addEventListener('visibilitychange', () => {
  if (!document.hidden) {
    if (refreshTimer) clearTimeout(refreshTimer);
    void refreshLoop();
  }
});

async function bootstrap() {
  bindNavigation();
  bindCredentialMirrors();
  setPage(currentPage);

  if (!invoke) {
    $('sidebarDot').className = 'status-dot bad';
    $('sidebarStatus').textContent = '非 Tauri 环境';
    showError('没有检测到 Tauri 运行环境。请使用 npm run desktop:dev 启动。');
    return;
  }

  try {
    $('log').textContent = '正在检查 ChatX 后端…';
    await invoke('ensure_backend');
    await refresh();
    refreshTimer = setTimeout(refreshLoop, 2500);
  } catch (error) {
    $('sidebarDot').className = 'status-dot bad';
    $('sidebarStatus').textContent = '启动失败';
    showError(normalizeError(error));
    $('log').textContent = '桌面控制台无法启动本地后端。';
  }
}

bootstrap();
