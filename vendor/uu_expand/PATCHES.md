# uu_expand patches

Source: the `uu_expand` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/expand`).

- Treat NUL as a zero-width non-tab byte, matching GNU's column accounting for
  embedded NUL records.
- Take the direct-write fast path only for complete newline-terminated lines;
  preserve the current column for a partial line instead of resetting it.
- Preserve explicit `-` stdin operands when expanding obsolete numeric tab
  shortcuts; a bare dash is not a tab-size option.
- Carry the current display column and initial-blank state across read chunks
  and file operands; only a newline begins a new logical line.
- Validate ascending order only for absolute tab stops; a trailing `+N` or `/N`
  specifies an interval and may be smaller than the last absolute stop.
- Use byte columns in single-byte locales and Unicode display widths in UTF-8
  locales; `--no-utf8` keeps byte columns regardless of the locale.
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.

Upstreamability: NUL/partial-line column behavior is intended for upstream; no
upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs` and
`crates/axe/tests/stream_repair.rs`.
