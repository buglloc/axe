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
