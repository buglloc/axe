# uu_env patches

Source: the `uu_env` 0.12.0 crate from crates.io, upstream revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69` (`.cargo_vcs_info.json`, path
`src/uu/env`).

- Return GNU `env`'s command/configuration failure status 125 for invalid
  signals, configuration errors, unsupported argv0 handling, and output/flush
  failures instead of conflating them with usage status 1/2.
- Flush the environment-only output before returning so a successful write and
  the resulting I/O status are observable by callers.
- The standalone unit test `test_error_cases` still expects the old literal
  `-S` diagnostic; the localized package emits the upstream message key
  `env-error-backslash-c-not-allowed`, so that environment/locale-only test
  mismatch is reported rather than used to change runtime behavior.
- Keep a standalone manifest without upstream workspace lint/benchmark wiring;
  AXE owns dependency resolution and locales.

Upstreamability: status and flush behavior target GNU env and are upstreamable;
no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/admin_terminal_compat.rs`.
