# uu_shred patches

Source: the `uu_shred` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/shred`).

- Truncate the file to zero length and `sync_all` it before the requested
  removal method, matching GNU shred's final file-removal visibility rather
  than unlinking a still-sized file immediately.
- Keep a standalone manifest without upstream workspace lint/benchmark wiring;
  AXE owns dependency resolution.

Upstreamability: final truncation/synchronization is intended for upstream; no
upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/misc_applets.rs`.
