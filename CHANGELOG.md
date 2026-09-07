# Changelog

All notable changes to ChatX are documented in this file.

## 0.2.1 - 2026-09-07

- Authenticate the Tauri-owned local backend with a per-session HMAC challenge and require the session secret on desktop `/api/*` requests.
- Split destructive filesystem operations (`fs_delete`, `fs_move`) from ordinary file writes; the developer preset keeps destructive access disabled by default.
- Treat Advanced Git as Shell-equivalent and require Shell permission for `git_run` at both visibility and handler layers.
- Lock bundled Node and tunnel-client versions plus SHA-256 hashes in `runtime-lock.json` and verify them before every desktop package build.
- Add mandatory Authenticode signing support for distributable releases; `desktop:release` refuses to publish without a configured code-signing certificate and timestamp service.
- Add version consistency checks across npm, Tauri, Cargo, and the MCP server.
- Centralize permission labels/descriptions in the backend so desktop and browser consoles no longer maintain separate permission tables.

- Validate restricted Git working-tree and metadata boundaries; reject path traversal/magic, disable external helpers, and isolate Git environment overrides.
- Stage file moves with destination backup and rollback; reject self/ancestor moves and preserve data on copy/commit failures.
- Default new settings to read-only permissions, preserve explicit legacy grants, and disable operations when configuration is invalid.
- Commit validated settings to disk before switching live permissions and notifying clients.
- Add execution IDs and bounded output caching; continuation reads never re-run commands.
- Add security regression tests to the standard test suite.

- Clean up installed Node and tunnel-client processes before installation and uninstallation, matching full executable paths and waiting for file locks to clear.
- Add a desktop diagnostics action to clean up residual runtime processes and restart the backend.
- Retry failed installed-backend startup once after cleanup and serialize startup/recovery requests.
- Use asynchronous HTTP health checks in desktop async commands.
- Add Windows runtime cleanup regression coverage (`npm run test:installer`).

## 0.2.0 - 2026-09-06

### Added

- Dynamic MCP tool exposure driven by the active permission preset.
- `fs_read_many` for bounded concurrent multi-file reads.
- `fs_project_snapshot` for a bounded project tree, key files, and Git context.
- `run_process` for direct executable invocation without shell parsing.
- `git_inspect` and `git_diff_summary` for compact repository inspection.
- Invocation latency and response-size summaries in the local console API.
- A repeatable `npm run benchmark` performance regression suite.

### Improved

- Added response budgets and continuation offsets to file reads, directory listings, Git diffs, and foreground process output.
- Accelerated literal and compatible regular-expression searches with ripgrep while preserving the JavaScript fallback.
- Added bounded concurrency to directory metadata collection, batch reads, and project key-file reads.
- Cached canonical filesystem roots until path-policy settings change.
- Reduced MCP JSON response size and avoided advertising disabled tools.
- Added visibility-aware dashboard polling and lower-overhead direct process execution.
- Expanded smoke coverage for permission changes, pagination, search engines, project snapshots, Git summaries, and performance metrics.

### Security and reliability

- Preserved per-call permission enforcement in addition to dynamic tool visibility.
- Continued to constrain Git write operations and disable hooks and GPG signing for managed commits.
- Added bounded output buffers, timeouts, process-tree termination, result limits, and listener cleanup.
