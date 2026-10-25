# procutils-vmstat patches

Source: the `procutils-vmstat` 0.2.0 crate from crates.io, upstream revision
`5daf44943ecf490b6912abe9d10863c6b6f4d147` (`.cargo_vcs_info.json`, path
`tools/vmstat`).

- Replace derived table rendering with procps-style right-aligned statistics
  rows and propagate stdout errors instead of unwrapping them.
- Use saturating memory accounting with `MemAvailable` fallback and include
  `SReclaimable` in cache, matching procps's `vmstat` memory fields.
- Add the kernel page, swap, allocation, free-page, interrupt, context-switch,
  boot-time, and fork counters expected by the reference output.
- Keep a standalone manifest without the upstream workspace test harness and
  package metadata; AXE owns dependency resolution.

Upstreamability: the counters, accounting, and output changes target procps and
are upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/procutils_compat.rs`.
