# uu_csplit patches

Source: the `uu_csplit` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/csplit`).

- Read and write split records as byte vectors and use byte regexes, preserving
  invalid input bytes instead of rejecting a non-UTF-8 line.
- Create the final empty split required by GNU csplit when input ends exactly at
  a split boundary (except for the repeating pattern case).
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.

Upstreamability: byte-preserving records and final-split behavior are intended
for upstream; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs`.
