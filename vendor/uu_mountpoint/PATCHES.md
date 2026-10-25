# uu_mountpoint patches

Source: `uu_mountpoint` 0.0.1 from `https://github.com/uutils/util-linux`,
revision `0cb1f90b9983c504a7f895ba65caa4b0b1f51c20` (source path
`src/uu/mountpoint`).

- Add GNU/util-linux `-q`, `-d`/`--fs-devno`, and `-x`/`--devno` modes with
  their distinct output and status rules.
- Parse paths as `PathBuf` and write normal mountpoint messages as raw path
  bytes, preserving undecodable names.
- Keep Linux `/proc` identity checks and the parent-device fallback for systems
  without procfs; report quiet-mode errors through the documented status.
- Use a standalone library manifest with explicit dependencies and no multicall
  binary target; AXE registers the library directly.

Upstreamability: option, status, and byte-path behavior target util-linux and
are upstreamable; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/linux_storage.rs`.
