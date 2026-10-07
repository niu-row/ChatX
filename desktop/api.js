(() => {
  const rawInvoke = window.__TAURI__?.core?.invoke;
  const invoke = typeof rawInvoke === 'function'
    ? (command, args) => rawInvoke(command, args)
    : null;

  window.ChatXApi = Object.freeze({
    invoke,
    available: Boolean(invoke),
  });
})();
