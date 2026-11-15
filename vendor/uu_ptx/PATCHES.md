# uu_ptx patches

Source: the `uu_ptx` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/ptx`).

- Use byte regexes with Unicode disabled and multiline matching, preserving GNU
  ptx behavior for raw input and a trailing lone backslash in a pattern.
- Add explicit sentence/context boundaries, including GNU's default punctuation
  rule, whole-file mode, and user-supplied context regexes; a blank line alone
  does not terminate a sentence context.
- Propagate input/output errors through the utility status and keep the help
  wording compatible with GNU (`FILE`, not `this FILE`).
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.
- Account for whitespace before a context's first keyword when sizing the
  wraparound tail and aligning the left field, without constructing an inverted
  byte range or dropping a suffix that fits the available layout.

Upstreamability: byte regex and context behavior target GNU ptx and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs` and
`crates/axe/tests/layout_repair.rs`.

GNU differential exclusions:

- GNU `scan_input_file` mistakes a zero return from `re_search` (a match at
  offset zero) for a zero-length match, rejecting blank lines with `-r`.
  AXE rejects only context matches that fail to advance.
- GNU's conservative backward jump in `define_all_fields` can omit a prefix
  that fits in the head field, notably with `-W`. AXE retains that context
  instead of reproducing the heuristic's unnecessary truncation.

Primary source: <https://github.com/coreutils/coreutils/blob/master/src/ptx.c>
(`scan_input_file` and `define_all_fields`).
