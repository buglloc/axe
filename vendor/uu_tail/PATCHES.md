# AXE patches to uu_tail 0.12.0

Source: the `uu_tail` 0.12.0 crate from crates.io, uutils/coreutils commit `dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`, directory `src/uu/tail`, as recorded in `.cargo_vcs_info.json`. The archive SHA-256 is `04ff141dd3fab47b991b2063fb8c1c9ff5ca4f9f23f5bcba75c691a247e1aca4`.

- Descriptor following retains the opened file across unlink, rename, pathname replacement, symlink retargeting, hard links and bind mounts. Truncation seeks the retained descriptor to zero.
- Regular-file stdin is duplicated rather than reopened by pathname, preserving the current offset and supporting renamed or unlinked inputs on Linux and Darwin. Pipe stdin still finishes at EOF.
- Name following resolves the original pathname, including symlinks, and compares device and inode with the opened descriptor. Retry handles inaccessible and dangling targets; polling detects mount overlays.
- Native notifications watch files and parent directories and serve only as wakeups. Deadline-based descriptor/path reconciliation covers aliases, removed watches and missing notification events without starvation from unrelated events.
- Polling checks only the followed descriptors and pathnames; it does not scan or hash directory contents. Follow output streams through the retained readers in bounded batches, with nonblocking FIFO reads and periodic PID checks.
- The parser and bounded/unbounded initial-tail algorithms are retained.

Regression coverage: `crates/axe/tests/tail_compat.rs`.

`just vendor-test` skips this package's upstream unit tests: Cargo cannot test
its `rstest` dev-dependencies as a non-member under the workspace lockfile.
The AXE integration tests exercise the bundled command with both native
notifications and polling, including isolated Linux bind mounts.
