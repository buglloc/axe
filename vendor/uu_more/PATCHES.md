# uu_more patches

Source: the `uu_more` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/more`).

- Add a non-terminal passthrough path: copy stdin/files byte-for-byte, emit
  GNU file banners for multiple inputs, and report file errors without entering
  terminal control mode.
- Avoid terminal clear-line escape output in the non-terminal path, preventing
  control sequences from contaminating redirected output.
- Keep the standalone manifest with explicit `tempfile` test support and
  without the upstream workspace lint/benchmark wiring; AXE owns resolution.
  Cargo refuses this dev-dependency-bearing, non-member package in the shipped
  workspace test graph, so `vendor-test` reports it as an explicit skip.

Upstreamability: non-terminal behavior is intended for upstream; no upstream
issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/smoke.rs`.
