# AXE patches to uucore 0.12.0

Source: `uucore` 0.12.0 from the pinned uutils/coreutils source checkout, commit `dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`.

The standalone manifest resolves upstream workspace dependencies. `locale-source/` retains the complete pinned utility locale inventory required by the multicall build.

`just vendor-test` skips upstream test targets that require the non-member package's dev-dependency graph. AXE integration regressions exercise the bundled consumer.
