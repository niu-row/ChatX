(() => {
  window.ChatXUiState = {
    refreshVersions: Object.create(null),
    beginRefresh(channel) {
      const next = Number(this.refreshVersions[channel] || 0) + 1;
      this.refreshVersions[channel] = next;
      return next;
    },
    isLatestRefresh(channel, version) {
      return this.refreshVersions[channel] === version;
    },
  };
})();
