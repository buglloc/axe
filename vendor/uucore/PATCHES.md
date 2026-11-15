# AXE patches to uucore 0.12.0

Source: `uucore` 0.12.0 from the pinned uutils/coreutils source checkout, commit `dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`.

The standalone manifest resolves upstream workspace dependencies. `locale-source/` retains the complete pinned utility locale inventory required by the multicall build.

- Missing-mode canonicalization detects repeated symlink components before
  expansion and keeps the unresolved component, allowing later `..` traversal.
  Normal and existing-only modes still reject cycles. This also preserves
  the correct relative target for `ln -sr` pointing at a cyclic symlink.
- Unicode escape output follows `LC_CTYPE`: UTF-8 stays UTF-8, C/POSIX
  escapes unrepresentable code points, and other codesets use conversion.
- Group delivery of SIGKILL bypasses handler installation, which the kernel
  rejects for an uncatchable signal. Non-foreground `timeout -s KILL` therefore
  preserves the group termination status.
- Uptime boot timestamps use `i64` Unix seconds across platform implementations,
  avoiding musl's deprecated `libc::time_t` alias without changing FFI layouts.

`just vendor-test` skips upstream test targets that require the non-member package's dev-dependency graph. AXE integration regressions exercise the bundled consumer.
