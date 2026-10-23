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

Release [v0.6.1](https://github.com/buglloc/axe/releases/tag/v0.6.1). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.6.1/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.6.1/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.6.1/x86_64-linux/axe) | `68ec2f73c97ec7455d50e0e173b6bb24cd4b426716f3266ced5c8dc57f6d4b8e` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.6.1/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.6.1/aarch64-linux/axe) | `b331be3ba54ec0e7dc6946e614e3b6c6a341b48b80e9e445c351b1e08254f25e` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.6.1/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.6.1/aarch64-darwin/axe) | `ce55d0fc59ef41debdb47f7e3181075cfbb0a75ddacc3643b1d3f3ceff8dadb5` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.6.1/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.6.1/x86_64-linux/axe-relay) | `37a342d6f598a6ad1dd7589a20da428603ba2120e00058eadc7a18ded3a1b563` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.6.1/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.6.1/aarch64-linux/axe-relay) | `986096b4a13cbb630e10c258f8b46e117bfdff76360bcc3b4902ef60d9a4edee` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.6.1/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.6.1/aarch64-darwin/axe-relay) | `94f5fd0d826c8b3904fd9eaaf18ca3d5adf98f3e098c1ef1a4a6446baf6a7387` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.6.1/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.6.1/x86_64-linux/vzik) | `039708f2546b88e7809dcda0faad8051c8f438f3f47351415db361e41b8a2d4e` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.6.1/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.6.1/aarch64-linux/vzik) | `8e224b9026feabe111db540bcb65e0a559dd374503f017624e02af1527ffc689` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.6.1/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.6.1/aarch64-darwin/vzik) | `dc2b98f3488af29d0f44f8179c643c99be7a7ea13bd5b3256cad8f664c6be729` |

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
