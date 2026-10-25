# procutils-watch patches

Source: the `procutils-watch` 0.2.0 crate from crates.io, upstream revision
`5daf44943ecf490b6912abe9d10863c6b6f4d147` (`.cargo_vcs_info.json`, path
`tools/watch`).

- Run `-x` commands with an argument vector and the default mode through
  `sh -c`, put each command in its own process group, and keep the documented
  distinction between shell and exec forms.
- Drain stdout and stderr concurrently, preserve their combined output, and
  cancel the process group on quit or signal without leaking a child or
  deadlocking on a full pipe.
- Truncate headers by terminal display width (including grapheme clusters and
  tabs), implement no-wrap and terminal resize behavior, and use the procps
  header spacing.
- Handle SIGINT/SIGTERM/SIGHUP and interactive quit through one termination
  state, returning the command status where available.
- Keep a standalone manifest without the upstream workspace test harness and
  package metadata; AXE owns dependency resolution.

Upstreamability: process lifecycle and display changes target procps and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/procutils_compat.rs`.
