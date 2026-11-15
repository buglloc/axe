# AXE patches to uu_rm 0.12.0

Source: the `uu_rm` 0.12.0 crate from crates.io, uutils/coreutils commit `dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`.

- Preserve GNU trailing-slash operand semantics: recursive removal empties the
  directory reached through an operand symlink, but retains the symlink and its
  target directory. Links discovered inside that directory are not followed.
  Force mode ignores the final ENOTDIR when unlinking the operand; nonrecursive
  removal still rejects the directory.

Consumer regression: `crates/axe/tests/path_repair.rs`. `just vendor-test` skips upstream targets requiring this non-member package's dev-dependency graph.
