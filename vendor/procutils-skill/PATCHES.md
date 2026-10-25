# procutils-skill patches

Source: the `procutils-skill` 0.2.0 crate from crates.io, upstream revision
`5daf44943ecf490b6912abe9d10863c6b6f4d147` (`.cargo_vcs_info.json`, path
`tools/skill`).

- Accept signal zero in both `-0` shorthand and `--signal 0`, matching the
  procps `skill`/`kill` convention for signal-existence checks instead of
  rejecting it as an unknown signal.
- Keep a standalone manifest without the upstream workspace test harness and
  package metadata; AXE owns dependency resolution.

Upstreamability: signal-zero handling is intended for upstream; no upstream
issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/procutils_compat.rs`.
