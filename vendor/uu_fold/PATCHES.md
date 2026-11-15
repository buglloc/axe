# uu_fold patches

Source: the `uu_fold` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/fold`).

- Process valid UTF-8 chunks and invalid bytes together, counting invalid bytes
  as one display column instead of falling back to a separate lossy/error path.
- Wrap repeatedly while a multi-column character or tab still exceeds the
  width, preserving GNU fold's boundary behavior for long graphemes and raw
  bytes.
- Keep a standalone manifest without the upstream benchmark and benchmark
  dependencies; AXE owns dependency resolution and locales.
- Count nonprinting control characters as one column (except zero-width NUL),
  consistently in ASCII, decoded UTF-8, and retained wraparound buffers.
- Use single-byte folding in C/POSIX locales; UTF-8 byte-count mode counts
  encoded bytes without splitting a decoded character.

Upstreamability: byte and wrapping behavior target GNU fold and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs` and
`crates/axe/tests/layout_repair.rs`.
