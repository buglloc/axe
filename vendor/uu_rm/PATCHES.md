# AXE patches to uu_rm 0.12.0

Source: the `uu_rm` 0.12.0 crate from crates.io, uutils/coreutils commit `dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`.

- Classify a trailing-slash symlink as an invalid directory operand before traversal. Forced removal ignores that operand; ordinary removal reports ENOTDIR. Neither the symlink nor its target is removed.

Consumer regression: `crates/axe/tests/path_repair.rs`. `just vendor-test` skips upstream targets requiring this non-member package's dev-dependency graph.
