# uu_df patches

Source: the `uu_df` 0.12.0 crate from uutils/coreutils, revision
`dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`. The package's upstream provenance is
retained in `.cargo_vcs_info.json` and `Cargo.toml.orig`.

- Backport [ea21b107e269928441bd111deacaaf68e18c5750](https://github.com/uutils/coreutils/commit/ea21b107e269928441bd111deacaaf68e18c5750):
  support `-m`, including `--output` and last-option-wins block-size selection.
  The `-m` help uses the literal `like --block-size=1M`: the uucore 0.12.0 locale
  bundle does not contain the backported `df-help-mega` message.
- Replace the selected mount when a better entry for the same device appears,
  rather than appending a second row. The upstream `is_best` ordering and
  filesystem filters are unchanged. Related issue:
  [uutils/coreutils#9883](https://github.com/uutils/coreutils/issues/9883).
- Keep a standalone manifest without benchmarks or their development
  dependencies. The AXE workspace owns dependency resolution and `Cargo.lock`.

The workspace patches uucore to the unmodified upstream `0.12.0` Git tag. Its
build script embeds utility locales from `src/uu` in that checkout, including
df's headers and help. This does not depend on a registry copy of `uu_df`
existing beside uucore in the Cargo cache. The vendored `locales/` files retain
the upstream `-m` backport, but uucore embeds its own checkout's locale files.

AXE consumer regressions are in `crates/axe/tests/df_smoke.rs`.
