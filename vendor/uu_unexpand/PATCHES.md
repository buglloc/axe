# uu_unexpand patches

Source: the `uu_unexpand` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/unexpand`).

- Treat NUL as a zero-width non-tab byte when converting spaces to tabs, so
  embedded NULs do not advance the column as printable text would.
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.

Upstreamability: NUL column behavior is intended for upstream; no upstream issue
or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs`.
