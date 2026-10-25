# jaq-json patches

Source: the `jaq-json` 2.0.3 crate from crates.io, upstream revision
`4229c5fad8470773f961fbb106e0634da8bf744d` (`.cargo_vcs_info.json`).

- Add jq-compatible reader entry points that accept jq numeric spellings and
  NUL framing while retaining jaq's value builder.
- Add JSON-sequence framing and resynchronization at record separators, so a
  malformed record does not consume the following sequence record.
- Keep the original jaq reader for jaq callers; the jq mode is opt-in and
  rejects comments rather than inheriting jaq's comment-tolerant input mode.
- Preserve invalid bytes and jq-compatible number values through the reader
  boundary; the AXE jq frontend performs the final output validation and
  canonicalization.
- This is a standalone library manifest; the workspace owns resolution and
  `Cargo.lock`, and the upstream command-line package is not vendored. Its
  upstream `defs`/`funs` tests require `jaq-std/format` and the optional
  serde feature, so standalone `cargo test` does not compile those tests; this
  is a packaging-only test limitation.
- Add the MIT license text at the vendor root because the crates.io archive
  carries the MIT metadata but no top-level license file; the provenance check
  requires a redistributable license file.

Upstreamability: the jq reader mode is suitable as an opt-in upstream feature;
no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/jq_compat.rs`.
