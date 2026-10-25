# uu_logname patches

Source: the `uu_logname` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/logname`).

- On Linux musl, resolve the login identity from `/proc/self/loginuid` and
  `getpwuid_r` instead of musl's stubbed/`LOGNAME`-based `getlogin`, matching
  the original login user rather than an environment override.
- Return login names as bytes and write them directly, preserving non-UTF-8
  account names instead of lossy conversion.
- Keep the standalone manifest with explicit dependencies and without upstream
  package/workspace metadata; AXE owns dependency resolution and locales.

Upstreamability: the musl loginuid fallback and byte output are intended for
upstream; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/logname_compat.rs`.
