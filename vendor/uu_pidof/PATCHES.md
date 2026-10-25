# uu_pidof patches

Source: `uu_pidof` 0.0.1 from `https://github.com/uutils/procps`, revision
`d318892db47de95ed181aab4270dd9b4a1130c43` (source path `src/uu/pidof`).

- Read `/proc/<pid>/cmdline` as NUL-separated arguments and compare executable
  paths, basenames, and script arguments without flattening spaces; this keeps
  process names containing spaces distinct.
- Check `/proc/<pid>/exe` and retain the worker-name fallback, including the
  existing `--scripts` prefix rule, while preserving raw Unix argument bytes.
- Use a standalone manifest with explicit procps/uucore revisions and no
  workspace path dependency; AXE owns the lockfile and applet registration.

Upstreamability: argument-boundary and executable-path matching target procps
and are intended for upstream; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/procutils_compat.rs`.
