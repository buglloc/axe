# AXE patches to uu_wc 0.12.0

Source: the `uu_wc` 0.12.0 crate from crates.io, commit `dc1efd89948a9ca4c78c3a4b9a6ac891019a8c69`.

`just vendor-test` skips upstream test targets that require the non-member package's dev-dependency graph. AXE integration regressions exercise the bundled consumer.

- Count every byte as a character in single-byte locales, consistently across
  count-only and word/display-width paths.
- Validate UTF-8 for character counts, ignoring invalid and incomplete byte
  sequences and retaining decoder state across input chunks.
- Count words byte by byte in single-byte locales, including GNU's default
  nonbreaking-space separator at byte `0xA0`.
- Recognize all four GNU nonbreaking word separators in UTF-8, including U+2060.
  `POSIXLY_CORRECT` disables those extensions without disabling locale whitespace.
