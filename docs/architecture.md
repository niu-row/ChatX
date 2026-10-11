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
  that override saved intent.
- `connection_health.rs` is the single health predicate shared by desktop
  `get_status`, `get_https_mcp_status` and Android Monitor.
  It checks local readiness, transport connection, public OAuth readiness,
  explicit Stop and a 45-second public-probe freshness window.
  UI should display `connected` rather than recomputing it from
  `sshConnected`. Android's historical `sshConnected` field retains its
  transport-neutral alias semantics for older clients, while new clients use
  `transportConnected`.
- `https_mcp.rs` owns Bridge/transport lifecycle and public probes.
  `https_mcp_wss.rs` implements the WSS transport and per-stream queues.
- `scripts/https-mcp-authorizations.mjs` exclusively owns persistent
  OAuth grant schema, rotation, listing and revocation. Rust never edits
  grant JSON: `https_mcp_authorizations.rs` invokes the bundled local-only
  `https-mcp-authorizations-admin.mjs` for administrative operations.
- The frontend has independent controllers:
  `pages/https-mcp.js` (connection),
  `pages/oauth-security.js` (OAuth settings),
  `pages/relay-deploy.js` (server provisioning).
- The deployment page explicitly requests a stable local Desktop ID using
  `prepare_relay_deploy_desktop_id`, even before initial Relay registration.
  Read-only network settings do not create identity files.

## Performance contract

The WSS v2 protocol uses 64 KiB frames, 64 MiB requests, 128 MiB responses,
and up to 64 concurrent *transport* requests. Relay and desktop use bounded
per-stream queues. The Node Bridge still materializes MCP JSON-RPC requests
and SDK tool results; it is not zero-copy/end-to-end streaming. To avoid
unbounded aggregate buffering, the Bridge admits eight concurrent MCP JSON
requests and returns HTTP 429 plus Retry-After when saturated.

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
