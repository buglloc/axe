# uu_mkfifo patches

Source: the `uu_mkfifo` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/mkfifo`).

- Parse FIFO operands as `OsString`/`OsStr` and create them with byte-preserving
  paths, so invalid Unix pathname bytes are not rejected by a UTF-8 conversion.
- Keep the umask-cleared atomic creation path and restore the umask around the
  `mkfifo` operation, preserving GNU's mode and race behavior.
- Keep a standalone manifest without upstream workspace lint/benchmark wiring;
  AXE owns dependency resolution.

Upstreamability: byte-preserving path handling is intended for upstream; no
upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/admin_terminal_compat.rs`.
