# AXE patches to uu_wc 0.12.0

Source: the `uu_wc` 0.12.0 crate from crates.io, commit `dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`.

`just vendor-test` skips upstream test targets that require the non-member package's dev-dependency graph. AXE integration regressions exercise the bundled consumer.

- Count every byte as a character in single-byte locales, consistently across
  count-only and word/display-width paths.
- Validate UTF-8 for character counts, ignoring invalid and incomplete byte
  sequences and retaining decoder state across input chunks.
