# AXE patches to jaq-core 3.1.2

Source: the `jaq-core` 3.1.2 crate from crates.io, commit
`f7fea9219d8d5364c91c6a0278765eaa0e6fa95e`.

- Parse `[:]` as an unbounded slice with an implicit zero start. Keep it
  distinct from `[]`, whose two absent bounds denote iteration internally.
- Consumer regressions: `crates/axe/tests/json_diff_repair.rs`, including
  null, arrays, strings, slice updates, and the distinction from iteration.
