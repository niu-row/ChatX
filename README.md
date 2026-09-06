# ChatGPTX

ChatGPTX is a local, full-capability MCP server intended to be reached from ChatGPT through a custom MCP app/connector and OpenAI Secure MCP Tunnel.

It does **not** call the OpenAI API itself. ChatGPT remains the MCP client; ChatGPTX only exposes local tools.

```text
ChatGPT
  -> custom MCP app / connector
  -> OpenAI Secure MCP Tunnel
  -> tunnel-client on your machine
  -> http://127.0.0.1:3210/mcp
  -> ChatGPTX
       -> filesystem
       -> shell / processes
       -> Git
```

## Capabilities

### Filesystem

- `fs_list` — list directories, optionally recursively
- `fs_stat` — inspect file/directory/symlink metadata
- `fs_read` — UTF-8 or base64 reads, byte ranges, line ranges
- `fs_write` — create/replace files
- `fs_append` — append to files
- `fs_edit` — exact text replacement with replacement-count checks
- `fs_mkdir` — create directories
- `fs_delete` — delete files/directories
- `fs_move` — move/rename, including cross-device fallback
- `fs_copy` — copy files/directories
- `fs_search` — recursive literal/regex text search

### Shell and processes

- `run_command` — arbitrary command execution using `cmd`, PowerShell, Bash, `sh`, or platform default
- foreground commands with timeout and stdout/stderr capture
- background commands returning a managed `process_id`
- `process_output` — read buffered output/status
- `process_list` — list managed background processes
- `process_stdin` — send stdin to a managed process
- `process_terminate` — terminate a managed process tree where supported

### Git

- `git_status`
- `git_diff`
- `git_log`
- `git_stage` — stage explicitly named paths
- `git_unstage` — unstage paths without discarding working-tree content
- `git_create_branch` — create a local branch without switching the working tree
- `git_commit` — commit already-staged changes with hooks and GPG signing disabled for this tool
- `git_run` — arbitrary Git argument escape hatch; requires the separate **Advanced Git** permission in addition to Git read/write

### Server

- `server_info` — reports platform, configured roots, shell state, and server version
- Streamable HTTP MCP endpoint: `/mcp`
- health/readiness endpoint: `/healthz` and `/readyz`
- MCP 2026-07-28 support plus stateless compatibility for 2025-era clients through the MCP TypeScript SDK v2 handler
- optional stdio mode for local MCP clients and Inspector testing

## Requirements

- Node.js 20 or newer. Node.js 24 is used in CI.
- Windows, macOS, or Linux.
- Git is required only for the Git tools.
- The Tauri desktop console additionally requires Rust 1.77.2 or newer and the normal Tauri platform build prerequisites.
- For ChatGPT access to a machine-local endpoint, use OpenAI Secure MCP Tunnel.

## Install

```bash
npm install
npm run build
```

Development mode:

```bash
npm run dev
```

Production-style local run:

```bash
npm run build
npm start
```

Default endpoint:

```text
http://127.0.0.1:3210/mcp
```

Local management console:

```text
http://127.0.0.1:3210/
```

### Tauri desktop console

The preferred Windows UI is the Tauri desktop console. Double-click `启动-ChatGPTX-桌面版.cmd`, or run `npm run desktop:dev`.

The desktop app keeps the existing Node/MCP backend and adds a native desktop shell. It starts `dist/index.js` when no ChatGPTX backend is already running on port 3210, uses a native Windows folder picker for allowed roots, and proxies only the local management API paths it needs. If ChatGPTX was already running before the desktop app opened, the desktop app reuses it and does not own or stop that external process.

The Tunnel ID is stored in `.chatgptx/settings.json`. The Runtime API Key can remain ephemeral or, when you opt in, be encrypted with Windows DPAPI (`CurrentUser`) and stored separately from `settings.json`; it is never returned in logs. The desktop UI also includes direct buttons for the OpenAI Tunnel ID and Runtime API Key management pages.

The original browser console remains available as a fallback. Double-click `启动-ChatGPTX-Web.cmd`, or use the existing `npm run console` command. The default `启动-ChatGPTX.cmd` now launches the Tauri desktop console.

Health check:

```text
http://127.0.0.1:3210/healthz
```

## Windows: full local access

By default, filesystem tools are restricted to the directory from which ChatGPTX is started. To intentionally give filesystem tools unrestricted access as the current Windows user:

```powershell
$env:CHATGPTX_FULL_ACCESS = "true"
$env:CHATGPTX_ENABLE_SHELL = "true"
npm run dev
```

You normally should **not** run the MCP server as Administrator. The shell tool inherits the permissions of the account running ChatGPTX.

To restrict filesystem tools to selected directories instead:

```powershell
$env:CHATGPTX_ROOTS = "D:\Projects;D:\Documents"
$env:CHATGPTX_FULL_ACCESS = "false"
npm run dev
```

On macOS/Linux, multiple roots use `:` instead of `;`:

```bash
export CHATGPTX_ROOTS="/home/me/projects:/tmp/work"
npm run dev
```

The local console can change allowed roots at runtime. Runtime roots are persisted in settings v2 and take effect on subsequent filesystem/Git tool calls without restarting the server. `CHATGPTX_ROOTS` remains the default/fallback for a fresh settings file and for migration from older settings.

Permission presets are available in the local console:

- **Safe** — filesystem read + Git read; write, shell, advanced Git, and full-access are off.
- **Developer** — filesystem/Git read-write; shell, advanced Git, and full-access are off.
- **Unrestricted** — enables filesystem/Git read-write, arbitrary Git, shell, and full filesystem access.
- Any manual toggle change produces a **Custom** preset.

## Important shell security behavior

`CHATGPTX_ROOTS` is a policy for the dedicated filesystem tools. It is **not a sandbox for arbitrary commands**.

If `run_command` is enabled, a command can access anything the operating-system account itself can access. This is intentional for the full local-agent use case.

Disable command execution completely with:

```powershell
$env:CHATGPTX_ENABLE_SHELL = "false"
```

or:

```bash
export CHATGPTX_ENABLE_SHELL=false
```

## Secure MCP Tunnel

ChatGPT cannot directly call a localhost MCP URL. OpenAI Secure MCP Tunnel runs an outbound-only tunnel client on the same machine/network and forwards MCP requests to the local server.

Start ChatGPTX first and verify `/healthz`, then configure the official `tunnel-client` with these minimum values:

```text
CONTROL_PLANE_API_KEY
CONTROL_PLANE_TUNNEL_ID
MCP_SERVER_URL=http://127.0.0.1:3210/mcp
```

PowerShell example:

```powershell
$env:CONTROL_PLANE_API_KEY = "<runtime-api-key>"
$env:CONTROL_PLANE_TUNNEL_ID = "tunnel_<32-lowercase-hex-chars>"
$env:MCP_SERVER_URL = "http://127.0.0.1:3210/mcp"

tunnel-client doctor --explain
tunnel-client run --log.level=info --log.format=struct-text
```

Then create/configure the custom MCP app in ChatGPT and select the tunnel connection. Keep both ChatGPTX and `tunnel-client` running while ChatGPT is using the connector.

When the local console manages `tunnel-client`, it explicitly adds `Content-Type: application/json` to MCP discovery/probe requests. This avoids newer MCP SDK v2 servers rejecting a probe POST with HTTP 415 when an intermediary/client omits the JSON media type. Tunnel startup is not considered successful until the tunnel client's own ephemeral health listener reports `/readyz` healthy via `--health.url-file`.

Official tunnel client: <https://github.com/openai/tunnel-client>

OpenAI Secure MCP Tunnel documentation: <https://developers.openai.com/api/docs/guides/secure-mcp-tunnels>

## ChatGPT plan limitation vs MCP limitation

ChatGPTX itself does not know or care which ChatGPT plan is calling it. The MCP server exposes the same tool set to any compatible MCP client.

Any limitation on whether ChatGPT may invoke write/modify actions is enforced by the **ChatGPT product/workspace**, not by this MCP server. At the time this README was written, OpenAI documents full MCP write/modify support for Business and Enterprise/Edu, with more limited custom-MCP access on other plans.

That means the same server can expose `fs_write`, `fs_delete`, `run_command`, and `git_run` even if a particular ChatGPT plan chooses not to invoke them.

## Configuration

Copy `.env.example` as a reference. ChatGPTX reads environment variables directly; it does not automatically load `.env` files.

| Variable | Default | Meaning |
| --- | --- | --- |
| `CHATGPTX_HOST` | `127.0.0.1` | HTTP bind host |
| `CHATGPTX_PORT` | `3210` | HTTP port |
| `CHATGPTX_ROOTS` | current working directory | default allowed filesystem roots for fresh/migrated settings |
| `CHATGPTX_SETTINGS_DIR` | `<cwd>/.chatgptx` | settings, DPAPI blob, disabled Git-hook directory, and tunnel health URL file |
| `CHATGPTX_FULL_ACCESS` | `false` | bypass filesystem root checks |
| `CHATGPTX_ENABLE_SHELL` | `true` | enable command/process tools |
| `CHATGPTX_AUTH_TOKEN` | empty | optional static bearer token for direct/private HTTP deployment |
| `CHATGPTX_MAX_FILE_BYTES` | `10485760` | maximum single file read/edit size |
| `CHATGPTX_MAX_COMMAND_OUTPUT_CHARS` | `200000` | foreground stdout/stderr buffer cap |
| `CHATGPTX_MAX_PROCESS_BUFFER_CHARS` | `1000000` | background process buffer cap |
| `CHATGPTX_DEFAULT_COMMAND_TIMEOUT_MS` | `120000` | default foreground command timeout |
| `CHATGPTX_MAX_SEARCH_FILES` | `10000` | file-search scan cap |

When `CHATGPTX_HOST` is loopback, the server applies MCP SDK localhost Host and Origin validation. For the standard tunnel deployment, keep the server bound to `127.0.0.1`.

## Optional bearer token

For a non-tunnel private deployment, you can require:

```text
Authorization: Bearer <CHATGPTX_AUTH_TOKEN>
```

Example:

```powershell
$env:CHATGPTX_AUTH_TOKEN = "replace-with-a-long-random-secret"
```

The normal Secure MCP Tunnel + localhost deployment can remain local-only without adding a fake authentication layer; the tunnel and loopback boundary are the intended transport boundary.

## MCP Inspector / stdio

For local testing without ChatGPT:

```bash
npm run dev:stdio
```

Or run the MCP Inspector against the development command:

```bash
npx @modelcontextprotocol/inspector npx tsx src/index.ts --stdio
```

Do not write normal logs to stdout in stdio mode because stdout is the MCP protocol channel. ChatGPTX writes its startup message to stderr.

## Safety model

This project intentionally exposes high-impact local capabilities. In full-access mode, `run_command` is effectively equivalent to allowing the model to act with the permissions of your logged-in OS account.

Recommended operating model:

- run under a normal user account, not root/Administrator;
- keep the MCP HTTP listener on loopback when using Secure MCP Tunnel;
- use filesystem roots when unrestricted disk access is not required;
- review ChatGPT confirmation prompts for write/destructive actions;
- do not connect an untrusted model/client to this server;
- do not treat filesystem roots as a shell sandbox.

## Development

```bash
npm run check
npm run build
npm test
```

`npm test` covers settings v1→v2 migration, permission presets, runtime root updates, Windows DPAPI persistence (when running on Windows), MCP tool smoke tests, the restricted `git_run` default, bounded Git write tools, dashboard diagnostics, and the non-JSON `/mcp` 415 guard.

CI checks the project on both Ubuntu and Windows.
