# awk-rs patches

Source: the `awk-rs` 0.2.0 crate from crates.io, upstream revision
`4addaefab9ff35c1e8d00ce348aacee259cf8b26` (`.cargo_vcs_info.json`).

- Replace the UTF-8 string pipeline with byte-oriented values, records, fields,
  escapes, output redirections, and command arguments. This preserves Unix names
  and input bytes instead of replacing invalid UTF-8, as gawk does in the C
  locale.
- Add POSIX/gawk-compatible regex handling with Oniguruma, including byte
  encoding, leftmost-longest matching, anchors, escapes, and capture groups.
- Keep AWK input lifecycle state in one run: `BEGIN`/`END`, `NR`/`FNR`,
  `FILENAME`, `ARGV` assignments, `getline`, `next`, `nextfile`, and `exit`
  now stop or continue at the same boundaries as gawk.
- Bound and validate `NF` and field indexes, and select only canonical numeric
  `ARGV` entries below `ARGC`; this prevents huge values from causing hangs or
  capacity-overflow panics. The 10,000,000 field bound is an AXE resource
  policy and is documented in the consumer tests.
- Honor `OFMT`/`CONVFMT`, numeric-string comparison, printf width/precision and
  `%c` behavior, and byte-preserving case/length operations.
- Keep a standalone library manifest without the upstream binary, integration
  tests, benchmarks, benchmark dependencies, and release profile; AXE invokes
  the library through its own applet wrapper.

Upstreamability: the byte, lifecycle, regex, and formatting fixes are intended
for upstream; the finite field bound needs upstream policy agreement. No
upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/awk_compat.rs`.
