# AXE patches to jaq-std 3.0.4

Source: the `jaq-std` 3.0.4 crate from crates.io, commit `f7fea9219d8d5364c91c6a0278765eaa0e6fa95e`.

`just vendor-test` skips upstream test targets that require the non-member package's dev-dependency graph. AXE integration regressions exercise the bundled consumer.

- Iterate global regex searches explicitly so an empty match adjacent to a
  nonempty match, including at end of input, is preserved. Advance an empty
  match by one character to guarantee progress.
- Map jq's `m` flag to dot-matches-newline. Keep whole-input anchors for
  jq's Perl NG default, `s`, and `p`, rather than treating `m` as Rust's
  line-anchor flag.
- Consumer coverage: `crates/axe/tests/json_diff_repair.rs`.
