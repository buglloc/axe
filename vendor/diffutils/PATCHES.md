# diffutils patches

Source: the `diffutils` 0.6.0 crate from crates.io (uutils/diffutils), upstream
revision `815ed7d8de89a236e5f636749f80a5b061017521` (`.cargo_vcs_info.json`).

- `cmp -n`/`--bytes` bounds the comparison and the reads: initial skips are
  subtracted before the size shortcut used by `-s`, so a difference beyond the
  limit is not reported and a zero limit still opens both inputs.
- `cmp` writes its normal and `-l`/`--verbose` reports through a fallible
  buffered writer and flushes it before returning; a stdout write or flush
  failure (for example `/dev/full`) is reported and returns status 2 instead of
  panicking or being lost.
- Keep a library-only standalone manifest: the upstream binary (`src/main.rs`,
  `src/diff.rs`), tests, benchmarks, CI files and their dependencies are not
  vendored. AXE resolves dependencies through the workspace `Cargo.lock`.

AXE uses this crate for `cmp` only; `diff` and `diff3` are implemented in
`crates/axe/src/applets/diffutils.rs`.

Upstreamability: both `cmp` fixes are upstream candidates. No upstream issue or
commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/diff_compat.rs`.
