# uu_expand patches

Source: the `uu_expand` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/expand`).

- Treat NUL as a zero-width non-tab byte, matching GNU's column accounting for
  embedded NUL records.
- Take the direct-write fast path only for complete newline-terminated lines;
  preserve the current column for a partial line instead of resetting it.
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.

Upstreamability: NUL/partial-line column behavior is intended for upstream; no
upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs`.
