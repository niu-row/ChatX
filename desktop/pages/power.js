(() => {
  window.ChatXPages = window.ChatXPages || {};

  window.ChatXPages.createPowerPage = ({
    $,
    invoke,
    normalizeError,
    showError,
    beginRefresh,
    isLatestRefresh,
  }) => {
    let busy = false;

    function render(payload) {
      const supported = Boolean(payload?.supported);
      const enabled = Boolean(payload?.clamshellAwake);
      const managed = Boolean(payload?.managedByChatX);
      const toggle = $('clamshellAwake');
      toggle.checked = enabled;
      toggle.disabled = busy || !supported;

      const status = $('clamshellStatus');
      status.textContent = !supported
        ? '仅支持 macOS'
        : enabled
          ? managed
            ? '已开启 · 退出自动恢复'
            : '已开启 · 系统设置'
          : '已关闭';
      status.className =
        `setting-status ${supported ? (enabled ? 'ok' : 'off') : 'off'}`;
    }

    async function refresh() {
      if (!invoke || busy) return;
      const version = beginRefresh('power');
      try {
        const payload = await invoke('get_power_settings');
        if (isLatestRefresh('power', version)) {
          render(payload);
        }
      } catch (error) {
        if (isLatestRefresh('power', version)) {
          $('clamshellAwake').disabled = true;
          $('clamshellStatus').textContent = '读取失败';
          $('clamshellStatus').className =
            'setting-status bad';
          showError(normalizeError(error));
        }
      }
    }

    async function setEnabled() {
      if (!invoke || busy) return;
      const toggle = $('clamshellAwake');
      const requested = toggle.checked;
      busy = true;
      toggle.disabled = true;
      $('clamshellStatus').textContent = requested
        ? '等待管理员授权…'
        : '正在关闭…';
      $('clamshellStatus').className =
        'setting-status warn';
      showError('');
      try {
        render(await invoke(
          'set_clamshell_awake',
          { enabled: requested },
        ));
      } catch (error) {
        toggle.checked = !requested;
        showError(normalizeError(error));
      } finally {
        busy = false;
        await refresh();
      }
    }

    return Object.freeze({
      render,
      refresh,
      setEnabled,
    });
  };
})();
