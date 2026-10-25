# uu_printenv patches

Source: the `uu_printenv` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/printenv`).

- Parse variable names as `OsString` and detect `=` by encoded bytes, preserving
  non-UTF-8 environment names while retaining GNU's invalid-name status.
- Propagate output and flush failures with status 2 instead of allowing a write
  error to be mistaken for a missing variable.
- Keep a standalone manifest without upstream workspace lint/benchmark wiring;
  AXE owns dependency resolution.

Upstreamability: byte names and I/O status behavior target GNU printenv and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/admin_terminal_compat.rs`.
