# sed-rs patches

Source: the `sed-rs` 2.0.0 crate from crates.io, upstream revision
`f4ab118242245c376af68c7da1af266051b71374` (`.cargo_vcs_info.json`).

- Keep scripts, addresses, regexes, replacements, transliteration tables,
  labels, commands, and filenames byte-oriented; decode GNU escapes without
  forcing invalid input through UTF-8.
- Resolve the active locale with libc so `y///` uses bytes in single-byte
  locales and characters in UTF-8 locales, matching GNU sed's locale model.
- Open each distinct `w`/`W` and `s///w` target once at startup, truncate regular
  files, retain handles across input cycles, and preserve `/dev/stdout` and
  `/dev/stderr` special outputs.
- Implement GNU `addr1,+N` and `addr1,~N` ranges and preserve input-file
  lifecycle/last-line behavior.
- Publish in-place replacements with source metadata while clearing
  `S_ISUID` and `S_ISGID`, preserving ordinary permission and sticky bits as GNU
  sed does after a `chown`.
- Keep a standalone manifest without the upstream binary, integration tests,
  benchmarks, benchmark dependencies, and release profile; AXE owns the
  workspace lockfile.

Upstreamability: the byte pipeline, locale handling, address ranges, and file
lifecycle are intended for upstream; the set-ID clearing patch is an AXE
compatibility backport. No upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/sed_compat.rs`.
