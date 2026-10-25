# uu_install patches

Source: the `uu_install` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/install`).

- Create ancestor directories through opened descriptors with a cleared umask,
  restore the caller's umask on all exits, and apply GNU install's 0755 ancestor
  mode independently of the caller's umask.
- Refuse a symlink race when opening a newly created component and retain the
  pinned descriptor for the later mode/ownership operation, rather than
  re-resolving a path that can be replaced.
- Add the required filesystem/rustix support in the standalone manifest and
  omit upstream workspace benchmark/lint wiring; AXE owns dependency resolution.

Upstreamability: descriptor-pinned directory creation is intended for upstream;
no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/admin_terminal_compat.rs`.
