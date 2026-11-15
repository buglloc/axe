# uu_fmt patches

Source: the `uu_fmt` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/fmt`).

- Compute the default goal from GNU's 187/200 ratio and use GNU's fixed
  ten-column margin when only a goal is supplied, rather than the upstream
  integer 93% approximation.
- Keep short candidate lines in the line-break search so a prefix consuming
  width does not force a one-word final line, matching GNU fmt wrapping.
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.
- Treat carriage returns and other nonblank control bytes as text instead of
  stripping CRLF or consuming them as word separators.
- Keep record terminators on unformatted lines, including nonmatching prefix
  records, rather than synthesizing a newline at EOF.
- Diagnose failed file operands and continue formatting later files while
  retaining a failing command status.
- Use GNU's three-column secondary indent for an initial unindented tagged
  paragraph, and retain the secondary indent learned from earlier paragraphs.

Upstreamability: width and line-break changes target GNU fmt and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs` and
`crates/axe/tests/layout_repair.rs`.
