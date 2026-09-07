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
- Tunnel status failures and malformed JSON responses are now surfaced as runtime errors instead of being reported as stopped/unknown.
- Reconnect now propagates unexpected failures when stopping an existing `chatx-local` runtime.
- Status polling reads the bundled tunnel-client version from `runtime-manifest.json`; direct `--version` execution remains part of explicit diagnostics.
- Desktop Commander build input now uses the official 0.2.48 GitHub Release MCPB locked by release tag, asset name, URL, byte size, SHA-256, and source commit.
- Removed build-time Desktop Commander npm dependency resolution/rebuild; ChatX now consumes the production `node_modules` and cross-platform ripgrep already contained in the verified MCPB release artifact.
- Runtime lock/manifest schema moved to version 3 with `locked-github-release` provenance metadata, bundle manifest hash, and ripgrep SHA-256.
- Runs bundled Desktop Commander through an isolated ChatX HOME/USERPROFILE and disables its telemetry with the upstream environment kill-switch.
- Removes `CHATX_TUNNEL_RUNTIME_KEY` from the Desktop Commander launcher environment before loading the MCP server so Desktop Commander child processes do not inherit the Tunnel credential.
- Added a real local stdio MCP bridge smoke test covering tool discovery, Runtime Key isolation, process execution, write/read, and content search.
- `desktop:release` now requires a valid Authenticode certificate configuration and refuses to publish an unsigned installer; local `desktop:installer` builds may remain unsigned for development.

### Removed

- ChatX permission presets and duplicated Filesystem/Git/Shell permissions.
- ChatX MCP invocation log UI; tool execution now happens directly inside Desktop Commander.
- Legacy MCP backend smoke/settings/security/performance test suite.

## 0.2.1 - 2026-09-07

Security and release hardening for the original custom MCP backend.

## 0.2.0 - 2026-09-06

Introduced dynamic tool exposure, project snapshot/process/Git inspection, and desktop runtime improvements for the original custom MCP backend.
