# rust_tree patches

Source: the `rust_tree` 1.3.0 crate from crates.io, upstream revision
`dfed2820d8d761107b2ddc4bf68b4746c82af302` (`.cargo_vcs_info.json`).

- Preserve raw pathname bytes and resolve `LC_ALL`, `LC_CTYPE`, and `LANG` to
  choose C/UTF-8 quoting, rather than converting names through lossy UTF-8.
- Add GNU-compatible `-N` raw-name and question-mark control-character modes,
  including locale-specific escaping and byte-exact output.
- Keep a standalone manifest without the upstream binary, integration tests,
  benchmarks, benchmark dependencies, and Windows lockfile; AXE invokes the
  library through its own applet wrapper.

Upstreamability: pathname quoting and the `-N` behavior are intended for
upstream; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/tree_compat.rs` and
`crates/axe/tests/path_transport_compat.rs`.
