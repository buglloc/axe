# findutils patches

Source: the `findutils` 0.10.0 crate from crates.io, upstream revision
`520640d896b78c2ce55589e00051206ad7f21c34` (`.cargo_vcs_info.json`). The
original crates.io tarball records `"dirty": true`; this is upstream packaging
provenance, not a local AXE edit.

- Keep find paths, `-files0-from` records, path operands, and matcher inputs as
  `OsStr`/`OsString` bytes, preserving Unix names that are not UTF-8 instead of
  replacing bytes with U+FFFD. This covers `-name`, `-path`, `-lname`, glob
  matching, `-newer`, and the path traversal boundary.
- Run `-regex` and `-iregex` through Oniguruma's ASCII byte encoding, and report
  a matcher error/status instead of silently losing bytes during conversion.
- Make `-printf`/`-fprintf` pathname directives byte-oriented, including width
  and precision, support the GNU flags represented by the parser, and propagate
  flush/write errors as find failures rather than panicking.
- Activate the requested C/locale byte collation and ctype state around find's
  matching work so raw-byte glob/regex behavior follows GNU find in the C
  locale without changing process-global locale state.
- Treat stdout `BrokenPipe` as an ordinary write failure. The upstream success
  special case disagrees with GNU find when the inherited disposition is
  `SIG_IGN`; AXE now reports the write error and non-zero status.
- Keep a standalone find library manifest and omit upstream CI, docs, test data,
  benchmark, and the unrelated `locate`, `updatedb`, and `xargs` targets; AXE
  owns workspace resolution and applet registration.

Upstreamability: all byte-path, formatter, regex, locale, and BrokenPipe
changes are intended for upstream; the source commits identify the upstream
need for formatter byte values and output-error propagation. No upstream issue
or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/path_transport_compat.rs`,
`crates/axe/tests/sigpipe_compat.rs`, and `crates/axe/tests/compression_compat.rs`.
