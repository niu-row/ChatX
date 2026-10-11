(() => {
  window.ChatXPages = window.ChatXPages || {};
  window.ChatXPages.createHttpsMcpPage = ({ $, invoke, showError, normalizeError }) => {
    let busy = false;
    let dirty = false;
    let started = false;
    let httpsReconnectBusy = false;
    const deployment = window.ChatXPages.createRelayDeployController({ $, invoke, normalizeError });
    const security = window.ChatXPages.createOauthSecurityPage({
      $, invoke, showError, normalizeError,
      getBusy: () => busy, setBusy: (value) => { busy = value; },
      refreshConnection: () => refresh(),
    });
    const fields = ['httpsMcpUrl', 'httpsMcpTransport', 'httpsMcpHost', 'httpsMcpUser',
      'httpsMcpSshPort', 'httpsMcpLocalPort', 'httpsMcpRemotePort'];
    const defaults = {
      baseUrl: '', transport: 'wss', sshHost: '',
      sshUser: '', sshPort: 22, localPort: 3000, remotePort: 18080,
    };
    function updateTransportVisibility() {
      const legacy = $('httpsMcpTransport').value === 'ssh';
      $('httpsMcpSshFields').hidden = !legacy;
      $('httpsMcpSshAdvanced').hidden = !legacy;
    }
    function payload() {
      return {
        enabled: true,
        baseUrl: $('httpsMcpUrl').value.trim().replace(/\/$/, ''),
        transport: $('httpsMcpTransport').value,
        sshHost: $('httpsMcpHost').value.trim(),
        sshUser: $('httpsMcpUser').value.trim(),
        sshPort: Number($('httpsMcpSshPort').value),
        localPort: Number($('httpsMcpLocalPort').value),
        remotePort: Number($('httpsMcpRemotePort').value),
        // Startup must use the last saved policy, never an unsaved option.
        authorizationTtlHours: security.committedHours(),
      };
    }
    function render(s) {
      security.renderPolicy(s);
      const autoReconnectEnabled = s?.autoReconnect !== false;
      if (!httpsReconnectBusy) $('httpsAutoReconnect').checked = autoReconnectEnabled;
      $('httpsAutoReconnect').disabled = busy || httpsReconnectBusy;
      $('httpsReconnectStatus').textContent = autoReconnectEnabled ? '已开启' : '已关闭';
      $('httpsReconnectStatus').className = `setting-status ${autoReconnectEnabled ? 'ok' : 'off'}`;
      if (!dirty) {
        const data = { ...defaults, ...s };
        $('httpsMcpUrl').value = data.baseUrl || defaults.baseUrl;
        $('httpsMcpTransport').value = data.transport || defaults.transport;
        $('httpsMcpHost').value = data.sshHost || defaults.sshHost;
        $('httpsMcpUser').value = data.sshUser || defaults.sshUser;
        $('httpsMcpSshPort').value = data.sshPort;
        $('httpsMcpLocalPort').value = data.localPort;
        $('httpsMcpRemotePort').value = data.remotePort;
      }
      updateTransportVisibility();
      deployment.autofill();
      const routeUp = Boolean(s?.transportConnected ?? s?.sshConnected);
      const isWss = $('httpsMcpTransport').value === 'wss';
      const routeName = isWss ? 'WSS Relay' : 'SSH';
      const connected = Boolean(!s?.manualStopBlocked && (s?.connected ??
        (routeUp && s?.localReady && s?.remoteReady)));
      started = Boolean(s?.enabled);
      const badge = $('httpsMcpStatus');
      badge.textContent = s?.manualStopBlocked ? '已手动停止' : connected ? 'HTTPS 已验证' : routeUp ? '检测公网 HTTPS 中' : started ? '启动中 / 重连中' : '未启动';
      badge.className = `setting-status ${connected ? 'ok' : started ? 'warn' : 'off'}`;
      $('startHttpsMcp').disabled = busy;
      $('stopHttpsMcp').disabled = busy || !started;
      for (const id of fields) $(id).disabled = busy;
      $('httpsMcpEndpoint').textContent = `${$('httpsMcpUrl').value.trim().replace(/\/$/, '')}/mcp`;
      $('httpsMcpHint').textContent = s?.manualStopBlocked
        ? '根据上次手动停止标记保持断开；持久配置仍可能显示启用。请检查设置保存权限，需要恢复时再手动启动。'
        : s?.lastError || s?.remoteLastError
          ? `最近错误：${s.lastError || s.remoteLastError}。请检查 HTTPS MCP、${routeName} 和公网代理日志。`
        : connected
          ? `公网 HTTPS、MCP OAuth 和 ${routeName} 已通过实际探测。ChatGPT 端继续使用上方 /mcp 地址。`
          : started
            ? s?.autoReconnect === false
              ? '连接尚未就绪，断线自动重连已关闭。检查网络后请手动启动重试。'
              : isWss
                ? '正在建立 WSS Relay。先在网络设置完成同域名 Relay Desktop 注册；服务器须配置 CHATX_MCP_DESKTOP_ID 并部署新版网关。'
                : '正在启动或自动重连。SSH 需要已有 known_hosts 和免交互密钥。'
            : isWss
              ? 'WSS 无 SSH 依赖；使用已注册的 Relay Desktop 凭据，公网 Nginx 须代理 /mcp、OAuth 和 /v1/ws/mcp。'
              : 'SSH 兼容模式需要公网 Nginx 反向代理端口。';
    }
    async function refresh() {
      if (!invoke || busy) return;
      try {
        const result = await invoke('get_https_mcp_status');
        if (!busy) {
          render(result);
        }
      } catch (error) {
        $('httpsMcpStatus').textContent = '读取失败';
        showError(normalizeError(error));
      }
    }
    async function setAutoReconnect() {
      if (!invoke || busy || httpsReconnectBusy) return;
      const toggle = $('httpsAutoReconnect');
      const enabled = toggle.checked;
      httpsReconnectBusy = true;
      toggle.disabled = true;
      showError('');
      try {
        render(await invoke('set_https_mcp_auto_reconnect', { enabled }));
      } catch (error) {
        toggle.checked = !enabled;
        showError(normalizeError(error));
      } finally {
        httpsReconnectBusy = false;
        await refresh();
      }
    }
    async function start() {
      if (!invoke || busy) return;
      busy = true;
      showError('');
      $('httpsMcpStatus').textContent = '正在启动…';
      try {
        render(await invoke('start_https_mcp', { config: payload() }));
        dirty = false;
      } catch (error) { showError(normalizeError(error)); }
      finally { busy = false; await refresh(); }
    }
    async function stop() {
      if (!invoke || busy) return;
      busy = true;
      showError('');
      try {
        render(await invoke('stop_https_mcp'));
        security.hidePassword();
      } catch (error) { showError(normalizeError(error)); }
      finally { busy = false; await refresh(); }
    }
    function bind() {
      for (const id of fields) {
        $(id).addEventListener('input', () => {
          dirty = true;
          $('httpsMcpEndpoint').textContent = `${$('httpsMcpUrl').value.trim().replace(/\/$/, '')}/mcp`;
        });
      }
      deployment.bind();
      $('httpsMcpTransport').addEventListener('change', () => {
        dirty = true;
        updateTransportVisibility();
      });
      $('startHttpsMcp').addEventListener('click', start);
      $('stopHttpsMcp').addEventListener('click', stop);
      $('httpsAutoReconnect').addEventListener('change', setAutoReconnect);
      security.bind();
    }
    bind();
    return { refresh, refreshSecurity: security.refreshSecurity, hidePassword: security.hidePassword };
  };
})();
