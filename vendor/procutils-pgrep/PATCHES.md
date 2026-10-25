# procutils-pgrep patches

Source: the `procutils-pgrep` 0.2.0 crate from crates.io, upstream revision
`5daf44943ecf490b6912abe9d10863c6b6f4d147` (`.cargo_vcs_info.json`, path
`tools/pgrep`).

- Print the match count before deciding whether `-c` or `-q` suppresses the
  normal process list, matching procps's count and quiet status behavior.
- Keep list-name/list-full formatting and shell quoting after count handling,
  so `-c` prints one count and returns success only when matches exist.
- Keep a standalone manifest without the upstream workspace test harness and
  package metadata; AXE owns the dependency resolution.

Upstreamability: the count/quiet ordering is intended for upstream; no
upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/procutils_compat.rs`.
