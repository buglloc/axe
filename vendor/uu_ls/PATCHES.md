# uu_ls patches

Source: the `uu_ls` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/ls`).

- Preserve undecodable filename bytes while hiding control characters in the
  implicit display mode, and distinguish implicit control hiding from explicit
  `-q`/quoting styles.
- Keep the standalone manifest without the upstream benchmark and benchmark
  development dependencies; AXE owns dependency resolution and locales.

Upstreamability: control-character display is intended for upstream; no
upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/path_transport_compat.rs`.
