# findutils patches

Source: the `findutils` 0.11.0 crate from crates.io, upstream revision
`2cb3adea013fbbe4c01750224183c95d8900416f` (`.cargo_vcs_info.json`). The
original crates.io tarball records `"dirty": true`; this is upstream packaging
provenance, not a local AXE edit. Archive SHA-256:
`999509a3ca9a9cdc156820463ce2e10cf406dd8e9c39d4bd5c9ae420c8ad7699`.

## Local changes

- Keep find paths, streamed `-files0-from` records, path operands, executable
  arguments, and matcher inputs as `OsStr`/`OsString` bytes, preserving Unix
  names that are not UTF-8 instead of replacing bytes with U+FFFD. This covers
  `-name`, `-path`, `-lname`, glob matching, `-newer`, `-exec` substitution, and
  the traversal boundary. Diagnostic rendering alone uses lossy display text.
- Run `-regex` and `-iregex` through Oniguruma's ASCII byte encoding.
- Keep `-printf`/`-fprintf` pathname directives byte-oriented before upstream's
  precision, flag, padding, and output-error handling.
- Activate the requested C/locale byte collation and ctype state around find's
  matching work so raw-byte glob/regex behavior follows GNU find in the C
  locale without changing process-global locale state.
- Retry unmatched invalid-byte glob inputs in the C locale when libc reports
  `FNM_NOMATCH` instead of `EILSEQ`, preserving musl UTF-8 filename matches.
- Treat `BrokenPipe` as an ordinary write failure, including stdout's shared
  error handler. The upstream success special case disagrees with GNU find
  when the inherited disposition is `SIG_IGN`; AXE reports the write error and
  non-zero status.
- Keep a standalone find library manifest and omit upstream CI, docs, test data,
  benchmarks, and the unrelated `locate`, `updatedb`, and `xargs` targets; AXE
  owns workspace resolution and applet registration. Retain upstream's shared
  path-list helper for `-execdir`/`-okdir` validation.

## Upstream coverage

The [0.11.0 release](https://github.com/uutils/findutils/releases/tag/0.11.0)
supplies formatter precision and byte-counted padding
([#918](https://github.com/uutils/findutils/pull/918)), non-panicking
`-fprintf` writes ([#840](https://github.com/uutils/findutils/pull/840)), and
shared stdout failure reporting for `-printf`, `-ls`, and `-print`
([#905](https://github.com/uutils/findutils/pull/905),
[#908](https://github.com/uutils/findutils/pull/908)).
These implementations replace AXE's corresponding local formatter/parser and
write-error patches; byte pathname values and failing BrokenPipe status remain
local deltas.

Upstreamability: the byte-path, formatter-value, regex, locale, and BrokenPipe
changes are intended for upstream. No upstream issue or commit is recorded for
these remaining AXE deltas.

AXE consumer regressions are in `crates/axe/tests/path_transport_compat.rs`,
`crates/axe/tests/sigpipe_compat.rs`, and `crates/axe/tests/compression_compat.rs`.
