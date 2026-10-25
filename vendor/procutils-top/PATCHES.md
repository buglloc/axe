# procutils-top patches

Source: the `procutils-top` 0.2.0 crate from crates.io, upstream revision
`5daf44943ecf490b6912abe9d10863c6b6f4d147` (`.cargo_vcs_info.json`, path
`tools/top`).

- Match procps memory accounting by preferring `MemAvailable`, saturating used
  memory, and reading `VmRSS`/shared values from `/proc/<pid>/status` in KiB.
- Add disk-sleep task accounting and render the task summary as procps does,
  including the `d-sleep` category.
- Format memory quantities with procps's width-dependent units and truncate
  user names with a `+` marker at the display boundary.
- Keep a standalone manifest without the upstream workspace test harness and
  package metadata; AXE owns dependency resolution.

Upstreamability: accounting and output changes target procps and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/procutils_compat.rs`.
