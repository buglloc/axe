# uu_stty patches

Source: the `uu_stty` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/stty`).

- Use Linux `termios2` for arbitrary numeric speeds, including separate input
  and output rates, while retaining standard names and `exta`/`extb` aliases.
- Preserve numeric split speeds in saved states and restore them with the
  correct `TCSETS*2` ioctl; keep non-Linux targets on their native speed API.
- Initialize the full termios ABI before conversion so musl's short `TCGETS`
  result does not leave control characters or speed fields undefined, and align
  the PowerPC build cfg with the implementation cfg.
- The upstream `test_string_to_baud_invalid` still expects `995` to be rejected;
  AXE's arbitrary-speed patch intentionally accepts it, so that vendored unit
  test is a real patch regression to report rather than mask.
- Keep a standalone manifest without upstream workspace lint/benchmark wiring;
  AXE owns dependency resolution.

Upstreamability: arbitrary Linux speed support targets GNU stty and is
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/admin_terminal_compat.rs`.
