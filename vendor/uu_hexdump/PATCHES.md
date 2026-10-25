# uu_hexdump patches

Source: `uu_hexdump` 0.0.1 from `https://github.com/uutils/util-linux`,
revision `0cb1f90b9983c504a7f895ba65caa4b0b1f51c20` (source path
`src/uu/hexdump`).

- Parse input operands as `PathBuf` and retain them through chained reading and
  diagnostics, preserving non-UTF-8 paths instead of collecting `String`s.
- Keep the supported format options explicit in help; format strings (`-e`) and
  format files (`-f`) remain unsupported rather than being silently accepted.
- Use a standalone library manifest with explicit `uucore`/`clap` versions and
  no multicall binary target; AXE registers the library directly.

Upstreamability: byte-preserving paths are intended for util-linux upstream;
format-string support is explicitly outside this vendored surface. No upstream
issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/linux_storage.rs`.
