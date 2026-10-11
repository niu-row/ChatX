(() => {
  window.ChatXPages = window.ChatXPages || {};
  window.ChatXPages.createHttpsMcpPage = ({ $, invoke, showError, normalizeError }) => {
    let busy = false;
    let dirty = false;
    let policyDirty = false;
    let started = false;
    let committedHours = 168;
    let httpsReconnectBusy = false;
    let grantRows = [];
    let deployBusy = false;
    let deployValidated = '';
    let deployDirty = false;
    const deployIds = ['relayDeployHost', 'relayDeployUser', 'relayDeployPort',
      'relayDeployDomain', 'relayDeployDesktopId', 'relayDeployKeyPath'];
    let deployLines = [];
    function deployLog(message, reset = false) {
      if (reset) deployLines = [];
      deployLines.push(String(message));
      deployLines = deployLines.slice(-24);
      $('relayDeployLog').textContent = deployLines.join('\n');
    }
    function deployConfig() {
      return {
        host: $('relayDeployHost').value.trim(),
        user: $('relayDeployUser').value.trim(),
        port: Number($('relayDeployPort').value),
        domain: $('relayDeployDomain').value.trim().toLowerCase(),
        desktopId: $('relayDeployDesktopId').value.trim(),
        keyPath: $('relayDeployKeyPath').value.trim(),
      };
    }
    function invalidateDeploy() {
      deployDirty = true;
      deployValidated = '';
      $('relayDeployApply').disabled = true;
    }
    function autofillDeploy() {
      if (deployDirty) return;
      if (!$('relayDeployHost').value) $('relayDeployHost').value = $('httpsMcpHost').value || '';
      if (!$('relayDeployDomain').value) {
        $('relayDeployDomain').value =
          $('httpsMcpUrl').value.trim().replace(/^https:\/\//, '').split('/')[0] || '';
      }
    }
    async function deployAction(apply) {
      if (!invoke || deployBusy) return;
      const config = deployConfig();
      const fingerprint = JSON.stringify(config);
      if (apply && deployValidated !== fingerprint) {
        deployLog('服务器信息变更后必须重新预检。'); return;
      }
      if (apply && !window.confirm(
        '即将通过 SSH 更新服务器 Relay 和 Nginx，期间服务会短暂重启。已有配置会先备份，失败自动回滚。继续吗？'
      )) return;
      deployBusy = true;
      $('relayDeployCheck').disabled = true;
      $('relayDeployApply').disabled = true;
      deployLog(apply ? '开始部署（远端 Cargo 编译可能需要数分钟）' : '开始只读预检…', true);
      try {
        const response = await invoke('manage_relay_server', { config, apply });
        if (!response?.ok) throw new Error(response?.message || '服务器未返回成功状态');
        if (!apply) deployValidated = fingerprint;
        deployLog(response.message || '成功');
      } catch (error) {
        deployValidated = '';
        deployLog('失败：' + normalizeError(error));
      } finally {
        deployBusy = false;
        $('relayDeployCheck').disabled = false;
        $('relayDeployApply').disabled = deployValidated !== JSON.stringify(deployConfig());
      }
    }
    const fields = ['httpsMcpUrl', 'httpsMcpTransport', 'httpsMcpHost', 'httpsMcpUser',
      'httpsMcpSshPort', 'httpsMcpLocalPort', 'httpsMcpRemotePort'];
    const defaults = {
      baseUrl: 'https://newrow.fun', transport: 'ssh', sshHost: '47.103.79.86',
      sshUser: 'chatx-mcp', sshPort: 22, localPort: 3000, remotePort: 18080,
    };
    const lifetimeOptions = [1, 24, 168, 720];
    function chosenHours() {
      const choice = $('httpsMcpAuthorizationDuration').value;
      if (choice === 'unlimited') return null;
      const hours = choice === 'custom' ? Number($('httpsMcpCustomHours').value) : Number(choice);
      if (!Number.isInteger(hours) || hours < 1 || hours > 87600) {
        throw new Error('授权有效期必须为 1～87600 小时，或选择「不限时间」');
      }
      return hours;
    }
    function updateLifetimeVisibility() {
      const choice = $('httpsMcpAuthorizationDuration').value;
      $('httpsMcpCustomHoursWrap').hidden = choice !== 'custom';
      $('httpsMcpUnlimitedWarning').hidden = choice !== 'unlimited';
    }
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
        authorizationTtlHours: committedHours,
      };
    }
    function render(s) {
      const ttl = s?.authorizationTtlHours === undefined ? 168 : s.authorizationTtlHours;
      committedHours = ttl;
      if (!policyDirty) {
        $('httpsMcpAuthorizationDuration').value = ttl === null ? 'unlimited'
          : lifetimeOptions.includes(ttl) ? String(ttl) : 'custom';
        if (ttl !== null) $('httpsMcpCustomHours').value = ttl;
      }
      updateLifetimeVisibility();
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
      autofillDeploy();
      const routeUp = Boolean(s?.transportConnected ?? s?.sshConnected);
      const isWss = $('httpsMcpTransport').value === 'wss';
      const routeName = isWss ? 'WSS Relay' : 'SSH';
      const connected = Boolean(!s?.manualStopBlocked && routeUp && s?.localReady && s?.remoteReady);
      started = Boolean(s?.enabled);
      const badge = $('httpsMcpStatus');
      badge.textContent = s?.manualStopBlocked ? '已手动停止' : connected ? 'HTTPS 已验证' : routeUp ? '检测公网 HTTPS 中' : started ? '启动中 / 重连中' : '未启动';
      badge.className = `setting-status ${connected ? 'ok' : started ? 'warn' : 'off'}`;
      $('startHttpsMcp').disabled = busy;
      $('stopHttpsMcp').disabled = busy || !started;
      $('saveHttpsMcpAuthorizationDuration').disabled = busy;
      $('rotateHttpsPassword').disabled = busy;
      $('revokeHttpsMcpAuthorization').disabled = busy;
      $('revokeAllHttpsMcpAuthorizations').disabled = busy;
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
    function grantLabel(grant) {
      const expired = grant.expiresAt !== null && Number(grant.expiresAt) <= Date.now() / 1000;
      const state = grant.revoked ? '已撤销' : expired ? '已过期' : '有效';
      const expiration = grant.expiresAt === null ? '不限时间'
        : new Date(Number(grant.expiresAt) * 1000).toLocaleString('zh-CN');
      const source = grant.id === 'legacy' ? '旧版迁移授权'
        : `ChatGPT · ${String(grant.clientTag || '').slice(0, 12)}`;
      return `${source} · ${state} · 到期：${expiration}`;
    }
    function selectedGrantActive() {
      const selected = grantRows.find((grant) => grant.id === $('httpsMcpAuthorizationList').value);
      return Boolean(selected && !selected.revoked &&
        (selected.expiresAt === null || Number(selected.expiresAt) > Date.now() / 1000));
    }
    function refreshSelectedGrantAction() {
      $('revokeHttpsMcpAuthorization').disabled = busy || !selectedGrantActive();
    }
    function renderGrants(response) {
      const grants = Array.isArray(response?.grants) ? response.grants : [];
      grantRows = grants;
      const list = $('httpsMcpAuthorizationList');
      const previous = list.value;
      list.innerHTML = '';
      if (typeof document.createElement === 'function') {
        for (const grant of grants) {
          const option = document.createElement('option');
          option.value = grant.id;
          option.textContent = grantLabel(grant);
          if (grant.id === previous) option.selected = true;
          list.appendChild(option);
        }
      }
      const active = grants.filter((g) => !g.revoked &&
        (g.expiresAt === null || Number(g.expiresAt) > Date.now() / 1000)).length;
      $('httpsMcpAuthorizationSummary').textContent =
        `共 ${grants.length} 条授权记录，其中 ${active} 条有效。改变默认期限不影响现有授权。`;
      refreshSelectedGrantAction();
      $('revokeAllHttpsMcpAuthorizations').disabled = busy || !active;
    }
    async function refreshGrants() {
      try { renderGrants(await invoke('get_https_mcp_authorizations')); }
      catch (error) {
        $('httpsMcpAuthorizationSummary').textContent = `读取授权记录失败：${normalizeError(error)}`;
      }
    }
    async function saveLifetime() {
      if (busy) return;
      let hours;
      try { hours = chosenHours(); }
      catch (error) { showError(normalizeError(error)); return; }
      if (hours === null && !window.confirm(
        '不限时间的授权不会自动失效；已授权的 ChatGPT 可以长期操作本机文件和执行命令，直到你主动撤销。确定保存吗？'
      )) return;
      busy = true;
      showError('');
      try {
        await invoke('set_https_mcp_authorization_lifetime', { ttlHours: hours });
        committedHours = hours;
        policyDirty = false;
      } catch (error) { showError(normalizeError(error)); }
      finally { busy = false; await refresh(); }
    }
    async function revokeGrant(all) {
      if (busy) return;
      const id = $('httpsMcpAuthorizationList').value;
      if (!all && !selectedGrantActive()) return;
      if (!window.confirm(all
        ? '确定撤销全部 OAuth 授权？ChatGPT 将需要重新授权。'
        : '确定撤销这项 OAuth 授权？现有访问令牌和刷新令牌将失效。'
      )) return;
      busy = true;
      showError('');
      try {
        const command = all ? 'revoke_all_https_mcp_authorizations' : 'revoke_https_mcp_authorization';
        renderGrants(await invoke(command, all ? {} : { grantId: id }));
      } catch (error) { showError(normalizeError(error)); }
      finally { busy = false; await refreshSecurity(); }
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
    async function refreshSecurity() {
      if (!invoke || busy) return;
      await refresh();
      if (!busy) await refreshGrants();
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
    function hidePassword() {
      const input = $('httpsMcpPassword');
      input.value = '';
      input.type = 'password';
      $('revealHttpsPassword').textContent = '查看 OAuth 授权密码';
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
        $('httpsMcpPassword').value = '';
        $('httpsMcpPassword').type = 'password';
      } catch (error) { showError(normalizeError(error)); }
      finally { busy = false; await refresh(); }
    }
    async function rotatePassword() {
      if (busy || !window.confirm(
        '重新生成 OAuth 密码会立即撤销全部 ChatGPT 授权，并重启 HTTPS MCP。需要使用新密码重新授权。确定继续吗？'
      )) return;
      busy = true;
      showError('');
      try {
        const nextPassword = await invoke('rotate_https_mcp_login_password');
        $('httpsMcpPassword').value = nextPassword;
        $('httpsMcpPassword').type = 'text';
        $('revealHttpsPassword').textContent = '隐藏授权密码';
      } catch (error) { showError(normalizeError(error)); }
      finally { busy = false; await refreshSecurity(); }
    }
    async function revealPassword() {
      if (!invoke) return;
      const input = $('httpsMcpPassword');
      if (input.type === 'text') {
        input.type = 'password';
        $('revealHttpsPassword').textContent = '查看 OAuth 授权密码';
        return;
      }
      try {
        input.value = await invoke('get_https_mcp_login_password');
        input.type = 'text';
        $('revealHttpsPassword').textContent = '隐藏授权密码';
        input.select();
      } catch (error) { showError(normalizeError(error)); }
    }
    function bind() {
      for (const id of fields) {
        $(id).addEventListener('input', () => {
          dirty = true;
          $('httpsMcpEndpoint').textContent = `${$('httpsMcpUrl').value.trim().replace(/\/$/, '')}/mcp`;
        });
      }
      for (const id of deployIds) $(id).addEventListener('input', invalidateDeploy);
      if (invoke) {
        invoke('get_network_settings').then((snapshot) => {
          const desktopId = snapshot?.relay?.status?.desktopId;
          if (!deployDirty && /^d_[a-f0-9]{32}$/.test(desktopId || '')) {
            $('relayDeployDesktopId').value = desktopId;
          }
        }).catch(() => {});
      }
      $('relayDeployCheck').addEventListener('click', () => deployAction(false));
      $('relayDeployApply').addEventListener('click', () => deployAction(true));
      if (window.__TAURI__?.event?.listen) {
        window.__TAURI__.event.listen('relay-deploy-progress', (event) => {
          if (deployBusy && event?.payload?.message) deployLog(event.payload.message);
        }).catch(() => {});
      }
      $('httpsMcpTransport').addEventListener('change', () => {
        dirty = true;
        updateTransportVisibility();
      });
      $('startHttpsMcp').addEventListener('click', start);
      $('stopHttpsMcp').addEventListener('click', stop);
      $('httpsAutoReconnect').addEventListener('change', setAutoReconnect);
      $('revealHttpsPassword').addEventListener('click', revealPassword);
      $('rotateHttpsPassword').addEventListener('click', rotatePassword);
      $('httpsMcpAuthorizationDuration').addEventListener('change', () => {
        policyDirty = true;
        updateLifetimeVisibility();
      });
      $('httpsMcpCustomHours').addEventListener('input', () => { policyDirty = true; });
      $('saveHttpsMcpAuthorizationDuration').addEventListener('click', saveLifetime);
      $('httpsMcpAuthorizationList').addEventListener('change', refreshSelectedGrantAction);
      $('revokeHttpsMcpAuthorization').addEventListener('click', () => revokeGrant(false));
      $('revokeAllHttpsMcpAuthorizations').addEventListener('click', () => revokeGrant(true));
    }
    bind();
    return { refresh, refreshSecurity, hidePassword };
  };
})();
