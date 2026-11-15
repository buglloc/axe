# uu_grep patches

Source: the `uu_grep` 0.2.0 crate from crates.io, upstream revision
`1fea766894236669612e8c5c37d3342a4926c123` (`.cargo_vcs_info.json`). The
original crates.io tarball records `"dirty": true` in that file; this is
upstream packaging provenance, not a local AXE edit.

- Keep filenames, patterns, input lines, context buffers, and diagnostics
  byte-oriented so invalid Unix pathname and input bytes are not replaced by
  Unicode replacement characters, matching GNU grep in the C locale.
- Preserve raw path arguments and xargs records through the grep invocation
  boundary, including non-UTF-8 names.
- Resolve the active locale codeset before matching. C/POSIX uses byte regexes,
  ASCII POSIX classes and word boundaries, and only NUL marks binary data;
  UTF-8 locales retain Unicode matching and invalid-encoding binary detection.
- Keep a standalone manifest without upstream CI, fuzzing, pre-commit, and
  development-only package files; AXE owns dependency resolution.

Upstreamability: byte-oriented paths and records are intended for upstream;
upstream's byte-output/error propagation work is the known compatibility gap
that motivated this fork. No upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/path_transport_compat.rs` and
`crates/axe/tests/language_repair.rs`.
