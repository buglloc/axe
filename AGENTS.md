# AGENTS.md

These instructions apply to the repository. Keep operational rules here; put user-facing behavior in `README.md`, build and release procedures in `BOOTSTRAP.md`.

## Before changing code

- Work in `nix develop .#default` (or run commands with `nix develop .#default --command ...`). Inspect the affected implementation, callers, tests, and crate manifests before editing.
- The public checkout owns the shared Rust code and OSS defaults. Other editions supply their own `AXE_EDITION_ROOT`; do not introduce private configuration, trust material, or package definitions into the public checkout. Do not mix edition-specific build inputs or release outputs.
- For command changes, inspect `crates/axe/src/registry.rs`, `config/aliases.json`, `crates/axe/src/help.txt`, and the README inventory. For Store packages, inspect `store/packages/lib.nix` and the relevant category. For target-specific changes, check the affected `cfg` branches and target ABI, not just host glibc.
- Generate missing *development* keys with `just generate-dev-keys` when a build or test needs them; it does not replace existing keys. Never print, commit, or use production private material as a test fixture.

## Runtime contracts

- The AXE executable is `axe`; do not add a `brush` compatibility binary or a second applet dispatch. Keep one applet registry in `crates/axe/src/registry.rs` and compile-time aliases in `config/aliases.json`.
- Preserve command resolution: shell alias/function → builtin → bundled applet → AXE Store → `PATH` only after a transient Store failure. Blocking Store failures return 126 without `PATH` fallback; unresolved commands return 127.
- Preserve applet `argv[0]` and byte-oriented arguments/paths where supported. PATH bridge publication is best-effort; bundled applets must remain usable without it.
- Keep `brush-shell`'s `minimal` backend for non-terminal execution and `reedline` for interactive TTYs.
- AXE Store must verify metadata and payloads (schema, bounds, signatures, digests, and HTTPS transport) before execution. Preserve atomic installs, stale-lock recovery, offline cache behavior, and Linux's sealed-`memfd` fallback. Do not weaken Index compare-and-swap or target-removal safeguards.
- SSH accepts user certificates only: the login must be allowlisted and match a certificate principal; reject plain public keys and critical options. Bound untrusted network input; give Tokio tasks a shutdown path or connection-owned lifetime.
- Linux release artifacts must be static ELF `EXEC` binaries without `INTERP` or `DT_NEEDED`.

## Build and Store boundaries

- `store/packages/` is the package source of truth. Add packages to an existing category when possible and use its package constructors. Keep package IDs unique, pin upstream sources, and do not ship Linux executables with dynamic or `/nix/store` runtime dependencies.
- Generate `store/bootstrap.json` with `just store-bootstrap`; do not edit it by hand. The build embeds the signed `store/bootstrap-index.cbor.zst` snapshot and public trust from the selected edition root. The `axe` build must never receive the Store private signing key or S3 credentials.
- Keep private keys and credentials under ignored edition-local `keys/` paths. `keys/ssh/user_ca_keys` contains only public CA keys. Remote builders are opt-in through `store-build-remote` or `store-sync-remote`; do not load their identities in local builds.
- Update the command help and README software inventory when the command surface or generated Store metadata changes. Update `BOOTSTRAP.md` or `docs/release.md` only when their contracts change.

## Verification

- Run checks inside the development shell. For Rust changes: `cargo fmt --all -- --check`, `just check`, `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`, and `just smoke`.
- Exercise changed behavior through the real `axe` binary, not only tests. For self-contained applets, use an empty `PATH`; for interactive changes, attach a PTY (`axe -c` does not test Reedline).
- For Store changes run `just check-store-bootstrap`, `just store-smoke`, and `just store-nix-smoke`. For package definitions, build the affected package and run `nix flake check --no-build .`.
- For Unix process, PTY, native dependency, FFI, or linker changes, also run `just build-linux-amd64`: host checks do not verify the musl ABI. For release changes, inspect the staged ELF type, program headers, and dynamic tags and launch it with `PATH=/nonexistent`.
- For SSH authentication changes, run the real server and prove a plain public key is rejected; use a separate test CA for positive certificate tests.
