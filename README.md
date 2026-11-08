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

Release [v0.16.0](https://github.com/buglloc/axe/releases/tag/v0.16.0). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.16.0/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.0/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.0/x86_64-linux/axe) | `966cb7796ebdd5047b4cd66c48d3eec50d715b99a70d7b5294be5b82cf4791d5` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.0/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.0/aarch64-linux/axe) | `c70f5e202921417192b50fd5ae931238a3c5d4de8ea71599e6edf3377ce904a5` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.0/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.0/aarch64-darwin/axe) | `bcce4f538b475c2962fc09e761f67991514bff3dac05509ab8f659ecf40c0d4f` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.0/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.0/x86_64-linux/axe-relay) | `02e373807c26c53b911c7875cc6ae0f4b226b8f0a06f822a83b3f349af4885a0` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.0/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.0/aarch64-linux/axe-relay) | `128bc73156713546b68ca853df819524d015546b40331a1131dd5fbcb60c8436` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.0/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.0/aarch64-darwin/axe-relay) | `63c884b83ffe829c651932de05ac8f540a05962d21f7961e8b3db8fb8e40ffad` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.0/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.0/x86_64-linux/vzik) | `1fdc2fb64b4ddc017f9dd9edc2c80042db9fa4a7c9a6d82ba2ad1d1bca1969b3` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.0/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.0/aarch64-linux/vzik) | `03c70da005af7a2ca352249ac308dd8da45d5ec7a0e21046bff0e8a93f78795f` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.0/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.0/aarch64-darwin/vzik) | `c89b93bce427c8d82422cd4331962de21c35a93721bd2d8e41b0bb494a3942c1` |

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
