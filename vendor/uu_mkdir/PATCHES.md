# uu_mkdir patches

Source: the `uu_mkdir` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/mkdir`).

- Restore all requested special mode bits, including the sticky bit, after
  directory creation; upstream restored only set-ID bits.
- Pin the created directory with `O_PATH|O_NOFOLLOW`/directory descriptors and
  apply mode changes through `fchmodat2`, procfs magic links, or a pinned-parent
  fallback, avoiding a path substitution race.
- Preserve the existing-directory recursive case only for `EEXIST`, rather than
  swallowing unrelated creation errors.
- Keep a standalone manifest with the extra filesystem support and without
  upstream workspace lint/benchmark wiring; AXE owns dependency resolution.

Upstreamability: descriptor-pinned creation and full special-mode restoration
are intended for upstream; the source commit identifies uutils/coreutils issue
`#12709` as the upstream sticky-bit limitation.

AXE consumer regressions are in `crates/axe/tests/admin_terminal_compat.rs`.
