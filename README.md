# AXE

AXE puts a shell, `sshd`, core utilities, and on-demand AXE Store tools in one executable. The primary Linux build is a static musl ELF; it needs no system shell, coreutils, or dynamic loader on the target machine.

Run that executable as an interactive [Brush](https://github.com/reubeno/brush) shell, a bundled applet, or a client that fetches signed AXE Store packages.

## Quick start

Download the Linux x86_64 release binary and run it:

```bash
curl -fL https://storage.yandexcloud.net/axe-store/axe/releases/v0.1.2/x86_64-linux/axe -o axe
chmod +x axe
./axe --version
./axe --list
```

Verify the download against the SHA-256 in [Downloads](#downloads) before using it. For other platforms, choose the corresponding binary below.

## Downloads

Release [v0.4.0](https://github.com/buglloc/axe/releases/tag/v0.4.0). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.4.0/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.4.0/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/x86_64-linux/axe) | `0954e4623f90aa419d4a61e66f0a077ba31e2c7c580996b37775c00d43a55f27` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.4.0/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/aarch64-linux/axe) | `093399a3ab9ad6d22c6d926bbea69b27fe119c7883fdc8acd47ec71f908a2f21` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.4.0/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/aarch64-darwin/axe) | `7e356bbbf9547297f51240ea51dff39c6297764e0781c5c1a47d2101ff7e1360` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.4.0/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/x86_64-linux/axe-relay) | `6eb654bda89adc4ac52a9d2e73566d2e08b3edc1e4fa6429c8034d48a076d6b3` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.4.0/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/aarch64-linux/axe-relay) | `568bdd54713c8b45d4a539a64f4a52232ae3bb8cc0760b541245f540f9ad56e5` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.4.0/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/aarch64-darwin/axe-relay) | `c0155c0f307cb57aad7fe4d5046598d4d344a0b6e54fdc142ed8bfbebaa94727` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.4.0/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/x86_64-linux/vzik) | `854f888eb3a9a0dfbd50819e8695127156305a830316da1914a32ef92316b26c` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.4.0/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/aarch64-linux/vzik) | `e86fb02de6ee1ac0e18029cb2e69a2f3ae16852334196731247465bcd122c3ea` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.4.0/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/aarch64-darwin/vzik) | `ccd075e439f26c6e4e0d5003d7af340c8f64c3b04e3c1159fd695cf14344a370` |

## Using AXE

Run the downloaded binary as a shell, call a bundled applet, or inspect the command inventory:

```bash
./axe -c 'ps'
./axe jq -- -n '{ok: true}'
./axe commands
```

`jq` is bundled; on-demand tools such as `rg` require access to the configured Store or a cached copy.

Inside the shell, aliases/functions and builtins take precedence over bundled applets, then AXE Store, then the host `PATH` under defined failure conditions. See [Commands](docs/commands.md) for invocation, resolution, and exit statuses.

## Included software

- Brush shell and bundled core, process, networking, and diagnostic utilities.
- `http` for bounded HTTP requests, `vzik` for host/container evidence, and certificate-only `sshd`.
- Signed, on-demand AXE Store packages with a verified local cache.

Use `commands` to inspect the active executable and its configured Store. Availability varies by target and Store state; the [bundled command inventory](docs/commands.md#bundled-commands) and [Store package inventory](docs/store.md#package-inventory) describe the available software.

## Installing with Nix

The root flake exposes `packages.<system>.axe` and `default` for targets listed in [`nix/axe-releases.json`](nix/axe-releases.json). It downloads an immutable release binary and verifies its recorded SRI hash.

## Building from source

For the OSS edition on Linux x86_64:

```bash
nix develop .#default
just build
install -Dm755 dist/axe-x86_64-unknown-linux-musl "$HOME/.local/bin/axe"
"$HOME/.local/bin/axe" --version
```

`just build` creates missing development keys and writes the Linux x86_64 artifact under `dist/`. Install it as `axe` to use the commands in this guide. For other targets use `just build-linux-arm64`, `just build-darwin-arm64`, or `just build-all`. See [Custom editions](docs/editions.md) for a build with your own identity and Store trust; [BOOTSTRAP.md](BOOTSTRAP.md) covers production identities and publishing.

## Documentation

The [documentation index](docs/README.md) links to guides for [HTTP](docs/http.md), [AXE Store](docs/store.md), [Vzik](docs/vzik.md), and [SSH and relay](docs/ssh-relay.md), plus a short [architecture map](docs/architecture.md). Build and publisher procedures are in [BOOTSTRAP.md](BOOTSTRAP.md); release procedures are in [docs/release.md](docs/release.md).
