# procutils-pmap patches

Source: the `procutils-pmap` 0.2.0 crate from crates.io, upstream revision
`5daf44943ecf490b6912abe9d10863c6b6f4d147` (`.cargo_vcs_info.json`, path
`tools/pmap`).

- Replace derived table rows with procps-compatible address, size, mode, and
  mapping formatting, including fixed address width and the extended/device
  column layouts.
- Render pathname basenames, kernel mapping names, SysV mappings, and anonymous
  or stack mappings according to procps; use the process's start-stack address
  to distinguish anonymous stack mappings.
- Treat a missing process as a status failure without printing a noisy error for
  the normal `/proc` disappearance race.
- Keep a standalone manifest without the upstream workspace test harness and
  package metadata; AXE owns dependency resolution.

Upstreamability: formatting and `/proc` race handling target procps behavior and
are upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/procutils_compat.rs`.
