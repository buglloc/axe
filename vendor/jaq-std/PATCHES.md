# AXE patches to jaq-std 3.0.4

Source: the `jaq-std` 3.0.4 crate from crates.io, commit `f7fea9219d8d5364c91c6a0278765eaa0e6fa95e`.

`just vendor-test` skips upstream test targets that require the non-member package's dev-dependency graph. AXE integration regressions exercise the bundled consumer.
