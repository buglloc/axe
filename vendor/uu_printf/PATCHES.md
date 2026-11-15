# uu_printf patches

Source: the `uu_printf` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/printf`).

- Add a native C long-double bridge for floating conversions, preserving target
  ABI precision and locale-aware decimal parsing without narrowing operands to
  `f64`.
- Implement GNU-compatible hexadecimal long-double formatting, rounding modes,
  signs, alternate form, width, precision, exponent, and locale radix handling.
- Resolve dynamic `*` width/precision and positional parameters with printf's
  missing-argument and repeated-format rules; propagate conversion/range/I/O
  errors instead of panicking.
- Keep the build script deterministic across glibc, musl, and Darwin: compile
  the checked-in C bridge, use debug optimization required by fortified libc,
  and link `libm` only where the target needs it. Run compiler probes in Cargo's
  `OUT_DIR`: cargo-zigbuild's Darwin `--version` probe writes an empty `a.o`
  in its working directory.
- Emit Unicode escapes in the active `LC_CTYPE` codeset through uucore's
  shared format writer, including `%b`; unrepresentable characters use
  uppercase `\\uHHHH`/`\\UHHHHHHHH` fallback notation.
- Propagate malformed numeric escape errors in `%b` instead of substituting a
  literal `\x`; emit completed prefixes without buffering the whole operand.
- Keep a standalone manifest without upstream workspace lint/benchmark wiring;
  AXE owns dependency resolution. The checked-in `tests/` cover the AXE native
  bridge.

Upstreamability: the numeric behavior is intended for upstream; the C bridge is
an AXE portability implementation because the upstream Rust formatter narrows
long doubles. No upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs` and
`crates/axe/tests/language_repair.rs`.
