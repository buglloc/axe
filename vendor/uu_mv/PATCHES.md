# AXE patches to uu_mv 0.12.0

Source: the `uu_mv` 0.12.0 crate from crates.io, commit `dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`.

`just vendor-test` skips upstream test targets that require the non-member package's dev-dependency graph. AXE integration regressions exercise the bundled consumer.

Destination existence checks use symlink metadata so no-clobber preserves dangling
and cyclic symlinks and backups retain those directory entries.
