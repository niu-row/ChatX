# AGENTS.md

This file captures repository-specific operating rules for coding agents working on ChatX.

## Repository context

ChatX is a Tauri desktop bridge between ChatGPT Secure MCP Tunnel and bundled Desktop Commander.
The repository also contains an Android monitor app.

Primary areas:
- `desktop/`: desktop HTML/CSS/JS UI.
- `src-tauri/`: Rust desktop runtime, tunnel lifecycle, monitor API.
- `android-monitor/`: Android monitor client.
- `scripts/`: build, verification, Desktop Commander launcher and toolchain helpers.

## Critical environment rule: inherited HOME may be fake

When commands are executed through ChatX / bundled Desktop Commander, the process may inherit an isolated HOME:

```text
<ChatX app local data>/state/desktop-commander-home
```

On macOS this can look like:

```text
/Users/<user>/Library/Application Support/com.chatgptx.local/state/desktop-commander-home
```

Do not assume `$HOME`, `~/.cargo`, `~/.gradle` or `~/Library/Android` refer to the real login account.
## Always resolve the real developer toolchain first

Before reporting Cargo, Rust, Java, Android SDK or Gradle as missing, run:

```bash
npm run dev:doctor
```

The repository helper `scripts/dev-toolchain.mjs` resolves the real OS account home with Node
`os.userInfo().homedir` and locates the developer toolchain from there.

Preferred validation commands:

```bash
npm run test:rust:check
npm run test:rust
npm run test:android:gradle
npm run test:local-full
```

`test:local-full` is the canonical full local verification entry point. It runs:
1. repository Node/static tests;
2. Rust unit tests;
3. Android `lintDebug` and Gradle unit tests;
4. `git diff --check`.

Do not manually reconstruct Cargo/JAVA_HOME/ANDROID_HOME paths unless the helper itself is being debugged.
## Gradle behavior

Android Gradle verification should prefer the real user's existing Gradle cache.

If the exact wrapper distribution is already unpacked under the real `~/.gradle/wrapper/dists`, reuse it.

If only this file exists:

```text
~/.gradle/wrapper/dists/gradle-<version>-bin/manual/gradle-<version>-bin.zip
```

the toolchain helper extracts it under:

```text
~/.gradle/chatx-toolchains/
```

and runs that local Gradle directly.

Do not conclude that Android validation is impossible merely because the wrapper tries to download a distribution.

## Preserve existing working-tree changes

This repository is often modified interactively and may contain unrelated uncommitted work.

Before broad edits, inspect `git status --short`.

Prefer surgical edits over whole-file rewrites.
Never reset, checkout, clean, stash, or otherwise discard unrelated user changes unless explicitly requested.
## Tunnel health semantics

Do not collapse all tunnel health signals into one boolean.

The tunnel client distinguishes at least:
- local process/runtime liveness;
- `healthy` / local health endpoint state;
- `ready` / usable readiness state;
- control-plane poll health when exposed.

Important invariant:

```text
healthy == true
```

does not by itself prove end-to-end tunnel readiness.

When `ready` is explicitly present, it must be respected.
Explicit unhealthy/down/stale control-plane health must override a superficially healthy local daemon.

The ChatX monitor uses a health progression such as:

```text
healthy -> suspect -> down
```

A live local runtime that is still becoming ready should receive a startup grace window.
A dead local runtime can be declared down faster.
## Tunnel monitoring and auto-reconnect are separate concerns

Continuous tunnel health sampling must continue even when automatic reconnect is disabled.

Automatic reconnect is a reaction policy, not the source of truth for tunnel health.

`get_status` is a read path. It must not call `runtime_status` or increment tunnel failure counters. Runtime health mutation belongs to startup/lifecycle transitions and the background health monitor only. UI refresh frequency must never change tunnel health.

Persist `connectionIntent` separately from the instantaneous runtime state. An explicit Stop clears it; normal app exit may stop the local runtime but must not silently erase the user's persisted connection intent.

Monitor payloads should expose freshness metadata such as:
- `lastProbeAt`;
- `lastSuccessfulProbeAt`;
- `consecutiveFailures`;
- `health`.

A stale probe must not be rendered as healthy.

Android should independently enforce freshness rather than trusting a cached `active=true` forever.

## Tunnel proxy semantics

Tunnel proxy routing is explicit and must not depend on accidental parent-process environment inheritance.

- `direct`: remove inherited `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and lowercase variants before invoking `tunnel-client`.
- `system`: resolve the OS explicit proxy only when the user applies network settings, persist that resolved address as the applied route, and use the same value for the managed runtime and status probes. Do not silently switch a running Tunnel merely because the OS proxy changed later.
- `manual`: validate and inject the configured proxy explicitly. Do not store or log proxy credentials; authenticated proxy URLs are intentionally rejected for now.
- macOS PAC/WPAD automatic proxy scripts are not equivalent to an explicit proxy URL and are currently unsupported.

`tunnel-client runtimes connect` does not expose a proxy flag, but the managed `run` process inherits `HTTPS_PROXY`; this behavior has been verified against the bundled runtime. Do not patch generated runtime profiles after `runtimes connect`, because connect rewrites them.

## Relay ownership and lifecycle

`Settings.relay` is the source of truth for the public Android Monitor relay. `monitor-relay.json` is legacy migration input only and must not become a second live configuration source.

On macOS ChatX owns `~/Library/LaunchAgents/com.chatx.relay.plist` plus the generated relay wrapper in the ChatX state directory. Relay SSH must use strict host-key checking and the real account `known_hosts`. Preserve the conventional `~/.ssh/id_ed25519` identity when present.

When recovering the reverse port, remote cleanup may terminate a listener only after verifying that the owning process is `sshd`; never kill an arbitrary process that happens to occupy the configured port. Relay reachability sampling must run independently from Tunnel health sampling so a slow public host cannot delay OpenAI Tunnel health checks.

## MCP live activity

Desktop Commander launcher activity is observational only and must never break an MCP call.

Do not store MCP arguments or tool outputs in the live activity snapshot.
Only safe metadata belongs there, such as:
- tool name;
- start time;
- duration;
- success/failure;
- in-flight count;
- in-flight call IDs.

The current activity schema supports multiple concurrent `inFlightCalls`.
The launcher must write a fresh empty activity snapshot at startup so a crashed prior process cannot leave a permanent ghost `RUNNING`/`STALLED` call.

Monitor device authorization is concurrent. Pair, authorize/lastSeen, and revoke operations must mutate one locked in-memory device registry and persist from that registry; do not reintroduce independent read-modify-write cycles against `monitor-devices.json`.
## Desktop live MCP UI

The desktop UI should use the lightweight `get_mcp_live_status` command for high-frequency refresh.

Do not poll the full desktop `get_status` endpoint at sub-second frequency. It is intentionally read-only now, but still assembles broader desktop status and logs; MCP live display should stay on the lightweight endpoint.

The live MCP view may refresh around once per second without coupling MCP display latency to tunnel polling.

## Host information exposed to Android

Monitor host telemetry must remain privacy-conscious.

Safe examples:
- device display name;
- OS/platform;
- architecture;
- battery percentage and charging state.

Do not expose account usernames, home directory paths, serial numbers, secrets, runtime keys,
file contents, command arguments or MCP tool results through the Android monitor API.

## Validation expectations

For changes touching tunnel health, MCP monitoring, host monitor payloads or Android parsing, run:

```bash
npm run test:local-full
git diff --check
```

A successful static test alone is not enough when Rust/Kotlin code changed.
Actual Rust and Android compilation/tests are expected whenever the local toolchain is available.
## Useful installed/runtime locations on macOS

These are discovery examples, not paths to hard-code in product code:

```text
/Applications/ChatX.app/Contents/Resources
/Applications/Android Studio.app/Contents/jbr/Contents/Home
<real home>/.cargo/bin
<real home>/.rustup/toolchains
<real home>/Library/Android/sdk
<real home>/.gradle/wrapper/dists
```

The installed ChatX app bundles runtime assets such as:
- `tunnel-client`;
- Node.js;
- Desktop Commander;
- ChatX launcher resources.

Developer toolchains such as Rust and Android SDK are separate from the ChatX runtime bundle.

## Rule of thumb

If a command says a familiar developer tool is missing while running through ChatX MCP, first suspect the isolated HOME.
Run `npm run dev:doctor` before installing anything, changing PATH globally, or reporting the tool unavailable.
