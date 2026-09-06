# Changelog

All notable changes to ChatX are documented in this file.

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
