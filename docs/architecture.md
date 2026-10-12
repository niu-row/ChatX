# ChatX architecture — source tree

This document describes the source-only refactor after 0.4.14. Source changes
are **not installed or deployed** until explicitly requested.

## Data flow

```text
ChatGPT HTTPS/OAuth -> Trusted Nginx :443 -> Relay Server
                                            |
                                            | WSS v2 (v1 fallback)
                                            v
ChatX Desktop Rust -> local Node OAuth/MCP Bridge -> Desktop Commander stdio
       |                          |
       |                    OAuth grant storage
       +-- Android Monitor (separate encrypted Relay/control session)
```

The native WSS transport requires no SSH at runtime. OpenAI Secure MCP
Tunnel and legacy SSH reverse forwarding remain separate compatibility
modes, mutually exclusive with the active HTTPS transport. Desktop Commander
continues to own the exact tool and permission model; multi-Desktop routing
is intentionally not included.

## Module ownership

- `src-tauri/src/settings.rs` holds persisted transport settings and
  effective mode selection. `connection_intent.rs` holds durable Stop markers
  that override saved intent. `connection_coordinator.rs` serializes
  cross-mode operations and centralizes the ordering of Stop markers,
  process shutdown and settings updates. Tunnel and HTTPS workers retain
  their separate transport-specific implementations; manual, automatic,
  Monitor-initiated and network-reconfiguration entry points use the same
  coordination lock. Before switching modes, the coordinator writes **both**
  durable Stop markers, then stops the previous transport and persists the
  disabled state. A failed stop or settings write therefore cannot resurrect
  either stale mode on app restart; only a successful start clears its own
  marker. The ordering and failure paths have local Rust regression tests.
- `connection_health.rs` is the single health predicate shared by desktop
  `get_status`, `get_https_mcp_status` and Android Monitor.
  `status_contract.rs` defines the typed, backward-compatible camelCase
  HTTPS status DTO, constructed from one consistent runtime snapshot.
  It checks local readiness, transport connection, public OAuth readiness,
  explicit Stop and a 45-second public-probe freshness window.
  UI should display `connected` rather than recomputing it from
  `sshConnected`. Android's historical `sshConnected` field retains its
  transport-neutral alias semantics for older clients, while new clients use
  `transportConnected`.
- `https_mcp.rs` owns Bridge/transport lifecycle and public probes.
  `https_mcp_wss.rs` implements the WSS transport and per-stream queues.
  `relay-protocol/src/mcp_http.rs` supplies the single path/header
  allowlist and legacy limits for Relay and Desktop. `mcp_control.rs`
  defines shared WSS v1/v2 JSON message kind names; binary framing stays
  in `mcp_stream.rs`.
- `src-tauri/src/app_state.rs` groups Relay connection, status and
  deployment-admission fields under `relay`, while `connection` owns the
  shared operation lock. `proxy_utils.rs` owns system proxy parsing and
  environment injection, separately from the desktop main entry point.
- `scripts/https-mcp-bridge.mjs` is HTTP/OAuth routing and assembly.
  `https-mcp-handler.mjs` owns MCP JSON-RPC admission, cancellation and
  request execution; `https-mcp-upstream.mjs` owns the singleton Desktop
  Commander stdio session, bootstrap and close behavior. The existing
  esbuild Bridge bundling automatically includes both internal modules.
- `scripts/https-mcp-authorizations.mjs` exclusively owns persistent
  OAuth grant schema, rotation, listing and revocation. Rust never edits
  grant JSON: `https_mcp_authorizations.rs` invokes the bundled local-only
  `https-mcp-authorizations-admin.mjs` for administrative operations.
  The admin process is bounded to 10 seconds and 1 MiB output; listing,
  revocation, and password-rotation orchestration use Tauri's blocking pool,
  not the UI thread. Revocation commands acknowledge the durable mutation
  without depending on a full list read (which may fail on unrelated corrupted
  data); the UI refreshes the list separately. Password rotation revokes
  before stopping the Bridge and attempts connection recovery even if updating
  the protected platform key fails. Legacy grants use the same revocation
  predicate for token checks and UI.
- The frontend has independent controllers:
  `pages/https-mcp.js` (connection),
  `pages/oauth-security.js` (OAuth settings),
  `pages/relay-deploy.js` (server provisioning).
- Opening the deployment panel (not opening Settings) requests a stable local
  Desktop ID via `prepare_relay_deploy_desktop_id`, even before initial Relay
  registration. Read-only network settings do not create identity files.
  The UI, Node deploy runner, Rust validator and Linux installer accept
  `d_` followed by 16–78 hexadecimal characters, including historic uppercase.

## Performance contract

The WSS v2 protocol uses 64 KiB frames, 64 MiB requests, 128 MiB responses,
and up to 64 concurrent *transport* requests. Relay and desktop use bounded
per-stream queues. The Node Bridge still materializes MCP JSON-RPC requests
and SDK tool results; it is not zero-copy/end-to-end streaming.
The Bridge separates eight ordinary request readers from four lightweight
control-request slots (12 total). Small-body candidates are checked after
JSON parsing and cannot remain in a control slot if they are actually tools;
control bodies are capped at 4 KiB and 10 seconds. Control slots are not an
absolute availability guarantee against four simultaneous slow control
candidates, but ordinary slow uploads cannot exhaust them. The Bridge
accounts for up to **80 MiB total buffered incoming wire bytes** and
**128 MiB of serialized HTTP response buffers**, returning 413/429 or a
bounded JSON-RPC error on saturation. This is **not** a strict V8/SDK heap
ceiling: Desktop Commander tool results are materialized before serialization.
The Bridge forwards both HTTP disconnects and explicit stateless MCP
`notifications/cancelled` messages to the SDK. Concurrent independent
requests **may reuse the same JSON-RPC ID within one OAuth grant**. Each
request owns its own cancellation controller; when a client supplies the
standard `Mcp-Session-Id` header, an explicit cancellation is scoped to
that session. Without a session ID, an ambiguous cancellation returns HTTP
409 rather than guessing and stopping the wrong tool. A different OAuth grant
can never cancel another grant's work.
A 250 ms in-flight grant/expiry recheck detects durable per-grant and global
OAuth revocation from the separate administration process, sends a
cooperative MCP cancellation, and revalidates permission before returning
successful tool data. This is **not an atomic rollback mechanism**: a tool
that ignores cancellation may continue making local file or shell changes.

Incoming Node requests have a 600-second timeout; the Relay request/stream
timeouts are 605 seconds; Nginx MCP timeout is 630 seconds.

## Deployment and compatibility

The deployment wizard uses SSH only for preflight, source upload and service
updates. It verifies host keys, backs up existing server configuration,
checks Nginx, and can roll back the application and proxy config. A custom
Cargo registry mirror may be supplied via `CHATX_RELAY_CARGO_CONFIG`
(on the server), rather than by copying files from a historical release
directory.

Retain WSS v1/Base64, SSH, the OpenAI Tunnel, and the legacy OAuth
migration until a separately planned compatibility sunset. Preserve
Android Monitor's old schema fields and Desktop Commander permissions.

## Local-only verification

`npm test`, `npm run test:rust:check`, and `npm run test:local-full`
verify sources. No NSIS/DMG/package build, installation, server update,
Git push, or release is part of this refactor. Producing a single
development-only OAuth admin resource for Rust/Tauri compile checking is
not an installer build.
