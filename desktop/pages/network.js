(() => {
  window.ChatXPages = window.ChatXPages || {};

  window.ChatXPages.createNetworkPage = ({
    $,
    invoke,
    normalizeError,
    showError,
    beginRefresh,
    isLatestRefresh,
    ageLabel,
  }) => {
    let busy = false;
    let dirty = false;

    function markDirty() {
      dirty = true;
    }

    function collectPayload() {
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
      if (status === 'suspect' || status === 'starting') {
        return 'warn';
      }
      return 'off';
    }

    function render(payload) {
      const proxy = payload?.proxy || {};
      const relay = payload?.relay || {};
      const relayState = relay.status || {};
      const tunnel = payload?.tunnel || {};

      if (!dirty) {
        $('proxyMode').value = proxy.mode || 'direct';
        $('proxyUrl').value = proxy.url || '';
        $('relayEnabled').checked = Boolean(relay.enabled);
        $('relayBaseUrl').value = relay.baseUrl || '';
      }

      const proxyMode = $('proxyMode').value || 'direct';
      for (const id of [
        'proxyMode',
        'relayEnabled',
        'relayBaseUrl',
        'saveNetwork',
        'testNetwork',
        'saveRelay',
        'testRelay',
        'registerRelay',
      ]) {
        const element = $(id);
        if (element) element.disabled = busy;
      }
      $('proxyUrl').disabled =
        busy || proxyMode !== 'manual';

      $('systemProxyFact').textContent = proxy.systemError
        ? `读取失败 · ${proxy.systemError}`
        : proxy.systemUrl || '未配置';
      $('effectiveProxyFact').textContent =
        proxy.effectiveError
          ? `无效 · ${proxy.effectiveError}`
          : proxy.effectiveUrl || 'Direct · 直连';
      const runtimeProxy = String(
        tunnel.proxySource || '',
      ).trim();
      $('runtimeProxyFact').textContent = runtimeProxy
        ? runtimeProxy === 'none'
          ? 'Direct · none'
          : runtimeProxy
        : '未知 / 等待 runtime';

      const cpState = String(
        tunnel.controlPlaneState || 'unknown',
      ).toLowerCase();
      const failures = Number(
        tunnel.controlPlaneFailures || 0,
      );
      $('controlPlaneFact').textContent =
        `${cpState.toUpperCase()}${failures ? ` · ${failures} FAIL` : ''}`;
      $('controlPlaneReason').textContent =
        tunnel.controlPlaneReason
        || '最近没有 Control Plane 错误。';

      const networkStatus = $('networkStatus');
      networkStatus.textContent =
        `${proxyMode.toUpperCase()} · ${cpState.toUpperCase()}`;
      networkStatus.className =
        `setting-status ${controlPlaneTone(cpState)}`;

      const relayStatus = $('relayStatus');
      const enabled = Boolean(relay.enabled);
      const registered = Boolean(
        relay.registered
        || relayState.configured
        || relay.credentialSaved,
      );
      const setupProvisioned = Boolean(
        relay.setupProvisioned,
      );
      const connected = Boolean(relayState.connected);
      const connecting = Boolean(relayState.connecting);

      relayStatus.textContent = !enabled
        ? '已关闭'
        : connected
          ? '已连接'
          : connecting
            ? '连接中'
            : registered
              ? '离线'
              : '待注册';
      relayStatus.className =
        `setting-status ${!enabled ? 'off' : connected ? 'ok' : connecting ? 'warn' : registered ? 'bad' : 'warn'}`;
      $('relayRegistrationFact').textContent = registered
        ? '已注册 · 安全存储'
        : setupProvisioned
          ? '未注册 · 可注册'
          : '未注册 · 等待管理员预配';

      const registerRelay = $('registerRelay');
      registerRelay.textContent =
        registered ? '已注册' : '注册此电脑';
      registerRelay.disabled =
        busy
        || registered
        || !$('relayBaseUrl').value.trim();

      $('relaySessionFact').textContent = connected
        ? `CONNECTED${relayState.sessionId ? ` · ${String(relayState.sessionId).slice(0, 12)}…` : ''}`
        : connecting
          ? `CONNECTING${relayState.reconnectAttempt ? ` · #${relayState.reconnectAttempt}` : ''}`
          : 'DISCONNECTED';
      $('relayDesktopFact').textContent =
        relayState.desktopId || '—';
      $('relayHeartbeatFact').textContent =
        ageLabel(relayState.lastHeartbeatAt);
      $('relayErrorFact').textContent =
        relayState.lastError || '—';
    }

    async function refresh() {
      if (!invoke || busy) return;
      const version = beginRefresh('network');
      try {
        const payload = await invoke(
          'get_network_settings',
        );
        if (isLatestRefresh('network', version)) {
          render(payload);
        }
      } catch (error) {
        if (isLatestRefresh('network', version)) {
          $('networkStatus').textContent = '读取失败';
          $('networkStatus').className =
            'setting-status bad';
          showError(normalizeError(error));
        }
      }
    }

    async function saveNetwork() {
      if (!invoke || busy) return;
      busy = true;
      showError('');
      $('networkStatus').textContent = '正在应用…';
      $('networkStatus').className = 'setting-status warn';
      try {
        const result = await invoke(
          'set_network_settings',
          { payload: collectPayload() },
        );
        dirty = false;
        render(result);
        $('networkTestResult').textContent =
          'Tunnel 网络设置已应用。';
      } catch (error) {
        showError(normalizeError(error));
      } finally {
        busy = false;
        await refresh();
      }
    }

    async function saveRelay() {
      if (!invoke || busy) return;
      busy = true;
      showError('');
      $('relayStatus').textContent = '正在应用…';
      $('relayStatus').className = 'setting-status warn';
      try {
        const result = await invoke(
          'set_network_settings',
          { payload: collectPayload() },
        );
        dirty = false;
        render(result);
        $('relayTestResult').textContent =
          result?.relay?.registered
            ? 'Relay 设置已应用。'
            : 'Relay 设置已保存；此电脑尚未注册。';
      } catch (error) {
        showError(normalizeError(error));
      } finally {
        busy = false;
        await refresh();
      }
    }

    async function registerRelay() {
      if (!invoke || busy) return;
      busy = true;
      showError('');
      $('relayStatus').textContent = '正在注册…';
      $('relayStatus').className = 'setting-status warn';
      $('relayTestResult').textContent =
        '正在注册此电脑…';
      try {
        const saved = await invoke(
          'set_network_settings',
          { payload: collectPayload() },
        );
        dirty = false;
        render(saved);
        const result = await invoke(
          'register_relay_desktop',
        );
        render(result);
        $('relayTestResult').textContent =
          '此电脑已注册；后续公网连接只使用设备自己的安全凭据。';
      } catch (error) {
        $('relayTestResult').textContent = '注册失败';
        showError(normalizeError(error));
      } finally {
        busy = false;
        await refresh();
      }
    }

    async function testNetwork() {
      if (!invoke || busy) return;
      busy = true;
      showError('');
      $('networkTestResult').textContent = '正在测试…';
      try {
        const result = await invoke(
          'test_network_settings',
          { payload: collectPayload() },
        );
        const proxy = result?.proxy || {};
        $('networkTestResult').textContent =
          `Tunnel ${proxy.ok ? 'OK' : 'FAIL'} · ${proxy.target || 'unknown'}`;
      } catch (error) {
        $('networkTestResult').textContent = '测试失败';
        showError(normalizeError(error));
      } finally {
        busy = false;
        await refresh();
      }
    }

    async function testRelay() {
      if (!invoke || busy) return;
      busy = true;
      showError('');
      $('relayTestResult').textContent = '正在测试…';
      try {
        const result = await invoke(
          'test_network_settings',
          { payload: collectPayload() },
        );
        const relay = result?.relay || {};
        $('relayTestResult').textContent = relay.ok
          ? `Relay OK · ${relay.target || ''}`
          : `Relay FAIL · ${relay.error || relay.target || 'unknown'}`;
      } catch (error) {
        $('relayTestResult').textContent = '测试失败';
        showError(normalizeError(error));
      } finally {
        busy = false;
        await refresh();
      }
    }

    function proxyModeChanged() {
      dirty = true;
      $('proxyUrl').disabled =
        $('proxyMode').value !== 'manual';
    }
    return Object.freeze({
      render,
      refresh,
      saveNetwork,
      saveRelay,
      registerRelay,
      testNetwork,
      testRelay,
      markDirty,
      proxyModeChanged,
    });
  };
})();
