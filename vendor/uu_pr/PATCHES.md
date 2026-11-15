# uu_pr patches

Source: the `uu_pr` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/pr`).

- Keep input lines as bytes and classify printable/control bytes with the active
  locale, so invalid names/data survive and `-c`/`-v` control rendering matches
  GNU pr instead of failing UTF-8 conversion.
- Implement GNU optional-argument parsing for short and long forms, including
  `-e`, `-n`, `-s`, `-S`, and numeric operands, without swallowing the next
  filename.
- Align pagination and column-mode semantics: header/trailer accounting,
  separators, default widths, numbering width, joined lines, and table filling
  now follow GNU pr.
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.
- Preserve input form-feed page boundaries with omitted headers (except `-T`),
  including empty pages, and keep record numbering continuous across boundaries.
- Do not double-space after an unterminated final record or a form-feed break.
- Keep inter-column padding pending until a following number or content byte
  needs it, so truncated empty columns neither shift later columns nor leak
  trailing padding.

Upstreamability: byte output, option parsing, and layout target GNU pr and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs` and
`crates/axe/tests/layout_repair.rs`.

GNU differential exclusions:

- GNU `src/pr.c` documents one number per merged output line and separators
  for empty merged columns. Its `print_page` calls `align_column` after a
  nonempty unterminated record closes a column, duplicating the number; an
  earlier `cols_ready_to_print` check can instead skip trailing separators.
  AXE keeps numbering and empty-column layout independent of EOF lookahead.
- `-i` output-tab conversion remains an explicitly unsupported option.

Primary source: <https://github.com/coreutils/coreutils/blob/master/src/pr.c>
(`print_page`, `align_column`, and the option contracts).
