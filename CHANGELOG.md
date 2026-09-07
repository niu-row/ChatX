# Changelog

## Unreleased

### Changed

- Replaced the custom ChatX MCP backend with bundled Desktop Commander 0.2.48.
- ChatX now launches OpenAI `tunnel-client` directly with a local stdio MCP command.
- Removed the localhost `127.0.0.1:3210/mcp` runtime path and the custom Filesystem/Git/Shell MCP tool implementation.
- Simplified the desktop UI to connection status, Runtime Key handling, diagnostics, and lifecycle logs.
- Runtime API Keys can still be stored with Windows DPAPI CurrentUser protection.
- Desktop packaging now bundles Desktop Commander, its dependencies, and its MIT license.
- Runtime locking schema moved to version 2 and now pins Desktop Commander.

### Removed

- ChatX permission presets and duplicated Filesystem/Git/Shell permissions.
- ChatX MCP invocation log UI; tool execution now happens directly inside Desktop Commander.
- Legacy MCP backend smoke/settings/security/performance test suite.

## 0.2.1 - 2026-09-07

Security and release hardening for the original custom MCP backend.

## 0.2.0 - 2026-09-06

Introduced dynamic tool exposure, project snapshot/process/Git inspection, and desktop runtime improvements for the original custom MCP backend.
