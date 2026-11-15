# uu_base32 patches

Source: the `uu_base32` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/base32`).

- Preserve the successfully decoded prefix before reporting an invalid base32,
  base64, or related encoded quantum, including incomplete final quanta; GNU
  coreutils writes valid output before its non-zero invalid-input status.
- Keep decoding and output streaming while flushing only the valid prefix, so an
  error does not discard bytes already accepted by the decoder.
- Skip line feeds while decoding; carriage returns require `--ignore-garbage`,
  matching GNU's byte-level alphabet validation.
- Decode complete unpadded base64url final quanta with the unpadded alphabet,
  without changing padded encoding output or accepting malformed padding.
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.

Upstreamability: valid-prefix output is intended for upstream; no upstream issue
or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs` and
`crates/axe/tests/archive_repair.rs`.
