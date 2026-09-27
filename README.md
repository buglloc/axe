# AXE

AXE puts a shell, `sshd`, core utilities, and on-demand AXE Store tools in one executable. The primary Linux build is a static musl ELF; it needs no system shell, coreutils, or dynamic loader on the target machine.

Run that executable as an interactive [Brush](https://github.com/reubeno/brush) shell, a bundled applet, or a client that fetches signed AXE Store packages.

## Quick start

Build the OSS edition on Linux x86_64:

```bash
nix develop .#default
just build
./dist/axe-x86_64-unknown-linux-musl --version
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl --list
```

`just build` creates missing development keys and writes `dist/axe-x86_64-unknown-linux-musl`. `AXE_STORE_MODE=off` makes the first run independent of Store network and cache state.

## Downloads

Release [v0.1.0](https://github.com/buglloc/axe/releases/tag/v0.1.0). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.1.0/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.1.0/x86_64-linux/axe) | `b66b71323b794edcce6a77db17aba9bda9c408b6ee0d649429d4bc9426004586` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.1.0/aarch64-linux/axe) | `a5426ff7d2d5db63c3f0a47d5ac0ff4e84a23dc338e343b39ecdff5e0749b781` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.1.0/aarch64-darwin/axe) | `85a979757f24cc5f439da4179e7914e683522ab15b7eb1dc2f9c70556c09dd22` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-relay-x86_64-unknown-linux-musl) | `2cfae05b8bab2c46f97a3d2add9c37b3c5b7f93aa831eac0100c031e91cfe3f7` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-relay-aarch64-unknown-linux-musl) | `52b6b851568d3a0a8e343768211d644b8082f303be398c453ac4527074320b95` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-relay-aarch64-apple-darwin) | `aeec80e823c65df997d94e45c8756abc22fe1c57f06581fd58498df6bf59c3d7` |

## Using AXE

Run AXE as a shell, call a bundled applet, or inspect the command inventory:

```bash
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl -c 'ps'
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl jq -- -n '{ok: true}'
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl commands
```

Inside the shell, aliases/functions and builtins take precedence over bundled applets, then AXE Store, then the host `PATH` under defined failure conditions. See [Commands](docs/commands.md) for invocation, resolution, and exit statuses.

## Included software

- Brush shell and bundled core, process, networking, and diagnostic utilities.
- `http` for bounded HTTP requests, `vzik` for host/container evidence, and certificate-only `sshd`.
- Signed, on-demand AXE Store packages with a verified local cache.

Use `commands` to inspect the active executable and its configured Store. Availability varies by target and Store state; the [bundled command inventory](docs/commands.md#bundled-commands) and [Store package inventory](docs/store.md#package-inventory) describe the available software.

## Installing with Nix

The root flake exposes `packages.<system>.axe` and `default` for targets listed in [`nix/axe-releases.json`](nix/axe-releases.json). It downloads an immutable release binary and verifies its recorded SRI hash.

## Building from source

Enter `nix develop .#default` and run `just build` as shown above. For other targets use `just build-linux-arm64`, `just build-darwin-arm64`, or `just build-all`. See [BOOTSTRAP.md](BOOTSTRAP.md) for building an edition with your own inputs, identities, or Store.

## Documentation

The [documentation index](docs/README.md) links to guides for [HTTP](docs/http.md), [AXE Store](docs/store.md), [Vzik](docs/vzik.md), and [SSH and relay](docs/ssh-relay.md), plus a short [architecture map](docs/architecture.md). Build and publisher procedures are in [BOOTSTRAP.md](BOOTSTRAP.md); release procedures are in [docs/release.md](docs/release.md).
