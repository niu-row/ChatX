# Changelog

## 0.3.0 - 2026-09-08

### Changed

- Replaced the custom ChatX MCP backend with bundled Desktop Commander 0.2.48.
- ChatX now launches OpenAI `tunnel-client` directly with a local stdio MCP command.
- Removed the localhost `127.0.0.1:3210/mcp` runtime path and the custom Filesystem/Git/Shell MCP tool implementation.
- Simplified the desktop UI to connection status, Runtime Key handling, diagnostics, and lifecycle logs.
- Runtime API Keys remain compatible with the existing Windows DPAPI CurrentUser store.
- Migrates the legacy `connection.tunnelId` setting from ChatX 0.2.1.
- Treats tunnel-client `runtime_state=ready` and structured `ready/healthy` fields as an active connection.
- Bundles and verifies Desktop Commander's required Windows ripgrep binary.
- Runs bundled Desktop Commander through an isolated ChatX HOME/USERPROFILE and disables its telemetry with the upstream environment kill-switch.
- Added a real local stdio MCP bridge smoke test covering tool discovery, write/read, and content search.
- Desktop packaging records the generated Desktop Commander dependency-lock hash and ripgrep SHA-256 in `runtime-manifest.json`.

### Removed

- ChatX permission presets and duplicated Filesystem/Git/Shell permissions.
- ChatX MCP invocation log UI; tool execution now happens directly inside Desktop Commander.
- Legacy MCP backend smoke/settings/security/performance test suite.

## 0.2.1 - 2026-09-07

Security and release hardening for the original custom MCP backend.

## 0.2.0 - 2026-09-06

Introduced dynamic tool exposure, project snapshot/process/Git inspection, and desktop runtime improvements for the original custom MCP backend.
