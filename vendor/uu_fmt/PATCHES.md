# uu_fmt patches

Source: the `uu_fmt` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/fmt`).

- Compute the default goal from GNU's 187/200 ratio and use GNU's fixed
  ten-column margin when only a goal is supplied, rather than the upstream
  integer 93% approximation.
- Score paragraph suffixes using GNU's line-length, raggedness, punctuation,
  widow, and orphan costs instead of a different cubic line-break heuristic.
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.
- Treat carriage returns and other nonblank control bytes as text instead of
  stripping CRLF or consuming them as word separators.
- Preserve control whitespace as word-leading text after it ends the preceding
  word, so uniform spacing can separate it without deleting the byte.
- Keep record terminators on unformatted lines, including nonmatching prefix
  records, rather than synthesizing a newline at EOF.
- Diagnose failed file operands and continue formatting later files while
  retaining a failing command status.
- Use GNU's three-column secondary indent for an initial unindented tagged
  paragraph, and retain the secondary indent learned from earlier paragraphs.
- Measure tab-separated word spacing in the original input columns and emit
  equivalent tabs/spaces at the chosen output positions, including canonical
  indentation on unformatted records.

Upstreamability: width and line-break changes target GNU fmt and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs` and
`crates/axe/tests/layout_repair.rs`.

GNU differential exclusion: `src/fmt.c` implements `isopen(c)` with `strchr`
on a C string. NUL matches the string terminator and incorrectly receives an
opening-delimiter bonus instead of an orphan-word cost. AXE classifies only
the explicit opening punctuation bytes, so paragraphs containing a
NUL-leading word can have different, intentional line breaks.

Primary source: <https://github.com/coreutils/coreutils/blob/master/src/fmt.c>
(`isopen`, `check_punctuation`, and `base_cost`).
