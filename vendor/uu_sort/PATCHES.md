# uu_sort patches

Source: the `uu_sort` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/sort`).

- Join the reader thread and return its error, rather than silently dropping a
  failed input read or panicking in the external-sort writer.
- Propagate write, flush, temporary-file, compressor, and final-output errors
  through the utility status, including `sort -c` checks and merged output.
- Keep the standalone manifest without upstream benchmarks and benchmark
  development dependencies; AXE owns dependency resolution and locales.

Upstreamability: error propagation targets GNU sort and is upstreamable; no
upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/core_text_compat.rs`.
