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

Upstreamability: byte output, option parsing, and layout target GNU pr and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs`.
