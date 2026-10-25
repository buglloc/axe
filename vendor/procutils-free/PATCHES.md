# procutils-free patches

Source: the `procutils-free` 0.2.0 crate from crates.io, upstream revision
`f82f6c72bdeb701c761bb4c0aed05dc7415b972c` (`.cargo_vcs_info.json`, path
`tools/free`).

- Replace derived table formatting with procps-style fixed columns: an 8-byte
  label field and 12-byte right-aligned value fields, including the `CachUse`
  double-space separator and the wide-column layout.
- Scale human-readable values independently using procps's three-column
  fitting and one-decimal rules, rather than selecting one unit from total
  memory.
- Preserve the kernel's optional `MemAvailable` value, use it for `used`, and
  retain the legacy formula on kernels that do not expose it; saturate
  subtraction for accounting counters.
- Keep the procps cache definition (`Cached + SReclaimable`) and report the
  same headers and row fields as `free`.
- Use a standalone manifest without the upstream workspace benchmark/test
  wiring; AXE resolves the crate from the workspace.

Upstreamability: output and accounting changes target procps compatibility and
are upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/procutils_compat.rs`.
