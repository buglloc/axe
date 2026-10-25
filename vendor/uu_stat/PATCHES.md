# uu_stat patches

Source: the `uu_stat` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/stat`).

- Keep filenames and symlink targets as `OsStr`/`OsString` through format
  expansion, preserving invalid bytes instead of using lossy UTF-8 strings.
- Add GNU's `shell-escape` quoting style and use locale-aware quoting for
  `locale`, `shell`, and `shell-escape-always`, including symlink targets.
- Keep a standalone manifest without upstream workspace lint/benchmark wiring;
  AXE owns dependency resolution.

Upstreamability: byte-preserving names and quoting styles target GNU stat and
are upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/path_transport_compat.rs`.
