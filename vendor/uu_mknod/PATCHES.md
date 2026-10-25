# uu_mknod patches

Source: the `uu_mknod` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/mknod`).

- Parse the node pathname as `OsString`/`OsStr` and pass it through the native
  creation call without lossy UTF-8 conversion, matching GNU on Unix names.
- Keep a standalone manifest without upstream workspace lint/benchmark wiring;
  AXE owns dependency resolution.

Upstreamability: byte-preserving path handling is intended for upstream; no
upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/admin_terminal_compat.rs`.
