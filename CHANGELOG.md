# Changelog

## Unreleased — source refactor (not packaged or deployed)

- Centralized HTTPS MCP connection health and freshness across desktop and Android.
- Split frontend HTTPS settings, OAuth security UI, and server deployment wizard.
- Moved OAuth grant administration to the same bundled Node store used by the Bridge; Rust acts only as a local CLI adapter.
- Allowed fresh installs to generate stable Desktop ID before first Relay registration.
- Removed deployment dependence on a historical Cargo build path, environment-specific UI defaults, and stale MCP version metadata.
- Aligned Node upload timeout with the WSS pipeline, and bounded concurrent JSON-RPC processing.
- Unified Linux installer Desktop ID validation with UI/Node/Rust and tested the real preflight validation in isolation.
- Forwarded HTTP disconnects as MCP SDK cancellation notifications; tested cooperative cancellation with a disposable fake stdio tool. Non-cooperative tools are not guaranteed to stop or undo side effects.
- Unified pre-generation OAuth revocation validity between token checks and UI; bounded the local OAuth administration child process to 10 seconds and 1 MiB output.
- Introduced separate eight-tool and four-control request budgets plus 80 MiB aggregate inbound payload accounting, with HTTP 429 on saturation.
- Deferred Desktop identity creation until deployment panel expansion, fixed offline HTTPS transport labels and removed stale UI examples.
- Updated architecture docs, local tests and runtime resource manifests. **No installer or server deployment in this change.**


## 0.4.14 - 2026-10-12

### Fixed
- Corrected overview and sidebar HTTPS MCP connection status for native WSS Relay. Previously the UI checked the SSH-only `sshConnected` flag, so a healthy WSS v2 session incorrectly showed an orange `未连接` status.
- The status now honors `transportConnected`, public/local readiness and the manual Stop marker; the connection labels reflect WSS versus legacy SSH.
- Added regression coverage for WSS healthy/disconnected, manual Stop and older SSH status snapshots.

## 0.4.13 - 2026-10-12

### Reliability
- Revokes an existing MCP WSS session immediately after Desktop Token re-registration; validates credential generation again at connection establishment and before dispatching public requests.
- Prevents a saturated per-request response or upload queue from blocking unrelated WSS traffic. Slow streams fail individually rather than stalling shared heartbeats.
- Returns HTTP 413 for chunked uploads exceeding 64 MiB and propagates request cancellation when a public HTTP stream ends prematurely.
- Aligns v2 stream and proxy inactivity deadlines around the 600-second desktop request timeout.
- Adds isolated deployment rollback failure injection to the local test suite, covering recovery of binary, environment, vhost and Nginx includes.
- Preserves Desktop Commander permissions and single-Desktop routing.

## 0.4.12 - 2026-10-12

### Changed
- Added native WSS MCP reverse transport and optional authenticated public Relay gateway, preserving existing SSH/Tunnel compatibility.
- Added a two-step, fail-closed server deployment wizard with SSH host-key verification, bundled source, server backup, configuration validation and rollback.
- Added WSS binary streaming v2 with protocol negotiation/fallback: 64 MiB request, 128 MiB response, 64 maximum in-flight requests.
- Added bounded backpressure, cancellation handling, safe Relay health metadata, and local deployment audit logs.
- Kept Desktop Commander permissions and the existing single-Desktop routing model unchanged.


## 0.4.11 - 2026-10-06

### Changed

- Finalized the Monitor route model around LAN IPv4, global IPv6 Direct, and server Relay only; obsolete route-specific references were removed from code, tests, and UI text.
- Bumped Desktop to 0.4.11 and Android Monitor to 0.3.6 (versionCode 9).

## 0.4.10 - 2026-10-06

### Changed

- Android Monitor adds configurable polling/freshness/offline thresholds, compact recent events, true on-demand refresh, and allowlisted E2EE Tunnel reconnect control.
- Monitor control retries are idempotent across Direct/Relay paths, advertise explicit capabilities, use a single operation deadline, and never override an explicit Desktop Stop.
- On-demand Tunnel refresh performs a fresh read-only runtime probe without mutating the background health state machine or failure counters.
- Relay control routing is scoped by desktop/device/request, with websocket capacity, pending-control limits, and per-device control throttling.
- Canonical local verification now includes Relay protocol/server tests; CI runs the full Windows verification plus macOS Rust/Relay checks.
- Android adds byte-level AES-GCM control tests, and stale background snapshots are rejected before endpoint mutation.
- Relay device revocation is now synchronized: Relay waits for an explicit Desktop acknowledgement before deleting the Relay credential, then terminates active device sessions.
- Relay registry persistence is fail-closed on corruption and uses a recoverable backup during Windows replacement.
- Android pairing/revoke work moved into a lifecycle-bound controller; pairing-derived endpoint/snapshot writes are conditional on the currently paired Desktop/device identity.
- Monitor routing is reduced to three supported paths only: private LAN IPv4, global IPv6 Direct, and server Relay; all other Direct route variants are removed from discovery, pairing, storage, routing, and UI.
- Desktop secret handling is isolated in `secrets.rs`; Monitor Master Key corruption is surfaced instead of silently rotating device E2EE material, and Runtime Key persistence is committed only after a successful Tunnel connection.
- Desktop build/package entry points share a real-user HOME/Cargo/Rustup environment, Cargo verification uses `--locked`, and locked runtime downloads use a SHA-verified persistent cache.
- CI now uses the runtime-locked Node 24.16.0 and exercises Desktop runtime verification plus the production no-bundle build on Windows; Gradle wrapper bytes are SHA-256 pinned.

### Security

- Runtime Key remains Desktop-only; mobile control is restricted to `refresh_snapshot` and `reconnect_tunnel`.
- Replayed identical encrypted control requests return the cached encrypted result, while conflicting reuse of a request ID is rejected.
- Existing Direct/Relay Monitor sessions are re-authorized after device revocation so revoked clients stop receiving new Snapshots.

## 0.3.0 - 2026-09-08

### Changed

- Added a macOS permission center that can proactively trigger Desktop/Documents/Downloads and saved-Keychain access in one flow, with a direct System Settings entry for Full Disk Access where macOS requires manual approval.
- Added background Secure MCP Tunnel auto-reconnect with connection intent tracking, session-only in-memory Runtime Key reuse, manual-stop cancellation, and bounded reconnect backoff.
- Added native macOS Apple Silicon (`darwin-arm64`) development and local packaging support, including locked Node.js and OpenAI tunnel-client arm64 release artifacts, platform-specific Tauri resources, and cross-platform Desktop Commander smoke tests.
- macOS local app/DMG builds now use ad-hoc signing identity `-` so the codesign identifier stays aligned with `com.chatgptx.local`; the package step verifies that identity and the Local Network usage description to prevent NECP/Local Network permission regressions.
- macOS Runtime API Keys can be stored in the system Keychain; Windows DPAPI behavior is unchanged.
- Replaced the custom ChatX MCP backend with bundled Desktop Commander 0.2.48.
- ChatX now launches OpenAI `tunnel-client` directly with a local stdio MCP command.
- Removed the localhost `127.0.0.1:3210/mcp` runtime path and the custom Filesystem/Git/Shell MCP tool implementation.
- Simplified the desktop UI to connection status, Runtime Key handling, diagnostics, lifecycle logs, and a read-only Desktop Commander tool-call history view with filtering, selectable 50/100/200/500/1000-row limits, aggregate latency/success statistics, details, and local clearing.
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
- Legacy custom-backend MCP invocation logging; tool execution history now comes from Desktop Commander's isolated local tool-history.jsonl.
- Legacy MCP backend smoke/settings/security/performance test suite.

## 0.2.1 - 2026-09-07

Security and release hardening for the original custom MCP backend.

## 0.2.0 - 2026-09-06

Introduced dynamic tool exposure, project snapshot/process/Git inspection, and desktop runtime improvements for the original custom MCP backend.
