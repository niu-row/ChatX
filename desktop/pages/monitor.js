(() => {
  window.ChatXPages = window.ChatXPages || {};

  window.ChatXPages.createMonitorPage = ({
    $,
    invoke,
    normalizeError,
    showError,
    beginRefresh,
    isLatestRefresh,
    ageLabel,
    renderNetworkSettings,
  }) => {
    let busy = false;
    let pendingDeleteDeviceId = '';
    let portDirty = false;

    function resetPendingDelete() {
      pendingDeleteDeviceId = '';
    }

    function markPortDirty() {
      portDirty = true;
    }

    function renderDevices(devices) {
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
        title.textContent = String(
          device.name || device.id || 'Android Device',
        );
        const meta = document.createElement('small');
        meta.textContent =
          `${String(device.id || '').slice(0, 18)} · 最近在线 ${ageLabel(device.lastSeenAt)}`;
        copy.append(title, meta);

        const remove = document.createElement('button');
        const deviceId = String(device.id || '');
        const confirming =
          pendingDeleteDeviceId === deviceId;
        remove.className = confirming
          ? 'ghost danger is-confirming'
          : 'ghost danger';
        remove.textContent =
          confirming ? '确认删除' : '删除';
        remove.disabled = busy;
        remove.addEventListener(
          'click',
          () => deleteDevice(deviceId, remove),
        );
        row.append(copy, remove);
        host.append(row);
      }
    }

    function render(payload) {
      const enabled = Boolean(payload?.enabled);
      const running = Boolean(payload?.running);
      const toggle = $('monitorEnabled');
      toggle.checked = enabled;
      toggle.disabled = busy;

      if (
        !portDirty
        && document.activeElement !== $('monitorPort')
      ) {
        $('monitorPort').value = String(
          payload?.port || 18432,
        );
      }

      $('saveMonitor').disabled = busy;
      $('createMonitorPairing').disabled =
        busy || !running;
      $('monitorBind').textContent =
        payload?.bindAddress || '—';
      $('monitorFingerprint').textContent =
        payload?.fingerprintSha256 || '—';
      $('monitorProtocol').textContent =
        `${payload?.protocol === 'chatx-monitor-wss-v1' ? 'HTTPS' : '—'} · ${payload?.encryption || 'E2EE'}`;

      renderDevices(payload?.devices || []);
      const status = $('monitorStatus');
      status.textContent = running
        ? '已运行'
        : enabled
          ? '未运行'
          : '已关闭';
      status.className =
        `setting-status ${running ? 'ok' : enabled ? 'bad' : 'off'}`;
    }
    async function refresh() {
      if (!invoke || busy) return;
      const version = beginRefresh('monitor');
      try {
        const [monitor, network] = await Promise.all([
          invoke('get_monitor_info'),
          invoke('get_network_settings'),
        ]);
        if (isLatestRefresh('monitor', version)) {
          render(monitor);
          renderNetworkSettings(network);
        }
      } catch (error) {
        if (isLatestRefresh('monitor', version)) {
          showError(normalizeError(error));
        }
      }
    }

    async function deleteDevice(deviceId, button) {
      if (!invoke || busy || !deviceId) return;
      if (pendingDeleteDeviceId !== deviceId) {
        pendingDeleteDeviceId = deviceId;
        if (button) {
          button.className =
            'ghost danger is-confirming';
          button.textContent = '确认删除';
        }
        return;
      }

      pendingDeleteDeviceId = '';
      busy = true;
      showError('');
      if (button) {
        button.disabled = true;
        button.textContent = '删除中…';
      }

      let deleted = false;
      try {
        render(await invoke(
          'revoke_monitor_device',
          { deviceId },
        ));
        deleted = true;
      } catch (error) {
        if (button) {
          button.disabled = false;
          button.className = 'ghost danger';
          button.textContent = '删除失败';
        }
        showError(normalizeError(error));
      } finally {
        busy = false;
        if (deleted) await refresh();
      }
    }

    async function save() {
      if (!invoke || busy) return;
      const enabled = $('monitorEnabled').checked;
      const port = Number.parseInt(
        $('monitorPort').value,
        10,
      );
      if (
        !Number.isFinite(port)
        || port < 1024
        || port > 65535
      ) {
        showError(
          'Monitor 端口必须为 1024-65535。',
        );
        return;
      }

      busy = true;
      showError('');
      try {
        render(await invoke(
          'set_monitor_enabled',
          { enabled, port },
        ));
        portDirty = false;
        $('monitorPairing').hidden = true;
      } catch (error) {
        showError(normalizeError(error));
      } finally {
        busy = false;
        await refresh();
      }
    }

    async function createPairing() {
      if (!invoke || busy) return;
      busy = true;
      showError('');
      try {
        const pairing = await invoke(
          'create_monitor_pairing',
        );
        const { qrSvg, ...payload } = pairing || {};
        $('monitorPairingQr').innerHTML = qrSvg || '';
        $('monitorPairingPayload').textContent =
          JSON.stringify(payload, null, 2);
        $('monitorPairing').hidden = false;
      } catch (error) {
        showError(normalizeError(error));
      } finally {
        busy = false;
        await refresh();
      }
    }
    return Object.freeze({
      render,
      refresh,
      save,
      createPairing,
      deleteDevice,
      markPortDirty,
      resetPendingDelete,
    });
  };
})();
