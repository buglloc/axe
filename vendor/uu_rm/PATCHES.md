# AXE patches to uu_rm 0.12.0

Source: the `uu_rm` 0.12.0 crate from crates.io, uutils/coreutils commit `dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`.

- Forced recursive removal treats ENOTDIR/ENOENT during the no-follow directory open as a missing operand. A trailing-slash symlink and its target remain untouched.

Consumer regression: `crates/axe/tests/path_repair.rs`. `just vendor-test` skips upstream targets requiring this non-member package's dev-dependency graph.
