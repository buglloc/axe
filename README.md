# AXE

AXE puts a shell, `sshd`, core utilities, and on-demand AXE Store tools in one executable. The primary Linux build is a static musl ELF; it needs no system shell, coreutils, or dynamic loader on the target machine.

Run that executable as an interactive [Brush](https://github.com/reubeno/brush) shell, a bundled applet, or a client that fetches signed AXE Store packages.

## Quick start

Download the Linux x86_64 release binary:

```bash
curl -fL https://storage.yandexcloud.net/axe-store/axe/releases/v0.4.0/x86_64-linux/axe -o axe
```

Compare `sha256sum axe` with the SHA-256 in [Downloads](#downloads) before running it. For other platforms, choose the corresponding binary below.

```bash
chmod +x axe
./axe --version
./axe --list
```

## Downloads

Release [v0.9.1](https://github.com/buglloc/axe/releases/tag/v0.9.1). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.9.1/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.9.1/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.9.1/x86_64-linux/axe) | `b75e2d43703d1ba259cd42820f637ecfb1669cbaff39377f7fe11f6ee6cd9e07` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.9.1/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.9.1/aarch64-linux/axe) | `5771aa15ee508fe3cdde0ebf52743b20a8d2246d609e88c1bff51dcaaec578e3` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.9.1/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.9.1/aarch64-darwin/axe) | `ccb6c04338bd91a67b2aadb1b762ccec3f3218977aaa318eb1d1a6fdb73fb69a` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.9.1/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.9.1/x86_64-linux/axe-relay) | `c7e8b670ea1e0d1b3e128b6cb3ac22d07e3385274a1ef1163f38cce6a74f1dfb` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.9.1/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.9.1/aarch64-linux/axe-relay) | `e510f36dadebcc1dd1c64a1ecba1c3f68a80a067b244a3f355419bece29446bc` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.9.1/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.9.1/aarch64-darwin/axe-relay) | `f8e915f3d476ae9e580b79e5c994d17cd8bb8727f211973405758f9011e0604d` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.9.1/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.9.1/x86_64-linux/vzik) | `d3b1e3627c0d044174b5aff115dedff5d03e8e9f0ed7a5b33096815f23316ae7` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.9.1/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.9.1/aarch64-linux/vzik) | `7f581788a54ecb2ce6cfe03829b58a0368d9a2f7dc1e1f29b096c9547d28c940` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.9.1/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.9.1/aarch64-darwin/vzik) | `fafb9035165cb9b9924178599a47f77181a1aec666503ef60cfc22ae64dcf8b4` |

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

Install the release binary with `nix profile add github:buglloc/axe`. The flake verifies the download hash.

## Building from source

For the OSS edition on Linux x86_64:

```bash
just shell
just build
install -Dm755 dist/axe-x86_64-unknown-linux-musl "$HOME/.local/bin/axe"
"$HOME/.local/bin/axe" --version
```

Enter the development shell with `just shell`, or `nix develop .#default` if `just` is not installed. To run a single command inside the shell, use `just shell just build`.

`just build` creates missing development keys and writes the Linux x86_64 artifact under `dist/`. Install it as `axe` to use the commands in this guide. For other targets use `just build-linux-arm64`, `just build-darwin-arm64`, or `just build-all`. See [Custom editions](docs/editions.md) for a build with your own identity and Store trust; [BOOTSTRAP.md](BOOTSTRAP.md) covers production identities and publishing.

## Documentation

The [documentation index](docs/README.md) links to guides for [HTTP](docs/http.md), [AXE Store](docs/store.md), [Vzik](docs/vzik.md), and [SSH and relay](docs/ssh-relay.md), plus a short [architecture map](docs/architecture.md). Build and publisher procedures are in [BOOTSTRAP.md](BOOTSTRAP.md); release procedures are in [docs/release.md](docs/release.md).
