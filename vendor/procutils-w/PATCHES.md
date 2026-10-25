# procutils-w patches

Source: the `procutils-w` 0.2.0 crate from crates.io, upstream revision
`5daf44943ecf490b6912abe9d10863c6b6f4d147` (`.cargo_vcs_info.json`, path
`tools/w`).

- Treat a missing utmp file as an empty session list, as procps does on systems
  without utmp, instead of failing before printing the header.
- Print the column header in the requested header mode and return success for an
  empty session list.
- Match procps field widths and spacing for `USER`, `TTY`, `FROM`, `LOGIN@`,
  `IDLE`, `JCPU`, `PCPU`, and `WHAT`, truncating long tty names to eight cells.
- Keep a standalone manifest without the upstream workspace test harness and
  package metadata; AXE owns dependency resolution.

Upstreamability: empty-utmp handling and layout changes target procps and are
upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/procutils_compat.rs`.
