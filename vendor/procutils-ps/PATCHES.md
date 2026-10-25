# procutils-ps patches

Source: the `procutils-ps` 0.2.0 crate from crates.io, upstream revision
`5daf44943ecf490b6912abe9d10863c6b6f4d147` (`.cargo_vcs_info.json`, path
`tools/ps`).

- Add process and thread row kinds, process-group data, cached `/proc` fields,
  thread-aware pending signals, `wchan`, file-descriptor counts, and group-name
  lookup so requested fields and sort keys use the same procps data.
- Add the procps field registry and minimum widths, including process/thread
  applicability, aliases, fixed PID/user/command widths, and summary placeholders.
- Match procps CPU and memory calculations: single-precision CPU percentage,
  KiB `VmRSS`, tenths-of-a-percent memory, and the documented fallback for
  missing status data.
- Implement native typed sorting, real thread listings, `-L`/`-T` row modes,
  and process/thread selection rules instead of treating every row as a process.
- Align headers, padding, truncation, start-time, signal, and status formatting
  with procps, and return an error status when output or field acquisition fails.
- Keep the standalone manifest and omit upstream snapshot-test development
  dependencies; AXE owns the workspace lockfile. The upstream
  `actual_terminal_width_env_and_options_share_same_policy` test currently
  observes 106 columns where it expects 53; this is a real width regression in
  the AXE patch and is reported rather than masked.

Upstreamability: the procps behavior and formatting fixes are intended for
upstream; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/ps_compat.rs` and
`crates/axe/tests/procutils_compat.rs`.
