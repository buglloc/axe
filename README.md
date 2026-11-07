# AXE

AXE puts a shell, `sshd`, core utilities, and on-demand AXE Store tools in one executable. The primary Linux build is a static musl ELF; it needs no system shell, coreutils, or dynamic loader on the target machine.

Run that executable as an interactive [Brush](https://github.com/reubeno/brush) shell, a bundled applet, or a client that fetches signed AXE Store packages.

## Quick start

Download the stable Linux x86_64 release binary:

```bash
curl -fL https://storage.yandexcloud.net/axe-store/axe/stable/x86_64-linux/axe -o axe
```

Compare `sha256sum axe` with the SHA-256 in [Downloads](#downloads) before running it. For other platforms, choose the corresponding binary below.

```bash
chmod +x axe
./axe --version
./axe --list
```

## Downloads

Release [v0.15.0](https://github.com/buglloc/axe/releases/tag/v0.15.0). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.15.0/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.0/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.0/x86_64-linux/axe) | `97efb904a3cd289418874233fb521584bfd2ee7f2705ae0d4d1647740351b0ea` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.0/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.0/aarch64-linux/axe) | `9cf61acce73d1c43bc9d5ebe31280fcd1e8264aaa5034519173270a50737d8d1` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.0/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.0/aarch64-darwin/axe) | `23a47e9d6b0dce6486187c1bc492eed7c6e28bf505b38742f31d92635e2cf129` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.0/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.0/x86_64-linux/axe-relay) | `393d4d540f8929a3ebb3e87a7b51d0a0c4bf446cec592029681070fcf68f0e5a` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.0/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.0/aarch64-linux/axe-relay) | `dcee3b545671acd2230b61fb46c9328f0fff7885f7b26d6d1c366a4968cc8535` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.0/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.0/aarch64-darwin/axe-relay) | `56839e525b3573765fa3e96ddeeca43ef332176b932cc7de38c04fd07d3572a4` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.0/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.0/x86_64-linux/vzik) | `84809ee75e1c3f3299cab52b6d8457e50e250a3b75d0e66c6dcbb32fb6ddbfd5` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.0/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.0/aarch64-linux/vzik) | `8440273b9aa3837f53f4149045533978783c28c6f36f3f9ac9e7bf4d12110f3d` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.0/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.0/aarch64-darwin/vzik) | `ae95ed99b80488b6e75e383b37d29cb0f7414f0a2a5dad884c25b76e5dedfed0` |

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
