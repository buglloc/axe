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

Release [v0.2.0](https://github.com/buglloc/axe/releases/tag/v0.2.0). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.2.0/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.0/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.0/x86_64-linux/axe) | `bb92b5ad6bcd153622ea82c6e88a8ce16d75e646b54ecbae5f40a9547f6bfe60` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.0/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.0/aarch64-linux/axe) | `102be469623df449e12411180362a41a24585f86903d6cb48a808095397d287d` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.0/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.0/aarch64-darwin/axe) | `84eac18c9b65620fa5bd7b45532dcddcf9e63ae00d65985f9e82f748afb14635` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.0/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.0/x86_64-linux/axe-relay) | `87451a28ec5bcf551cc961db5cd11812b9d0d2e3e2700b72589bf4d24047cb9e` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.0/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.0/aarch64-linux/axe-relay) | `c3769c51c86c3c82884f45cce56c3da5d326ad57fe4d5e0c72a0f8cd7e72f6a5` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.0/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.0/aarch64-darwin/axe-relay) | `1910b2bfeb8c93b9f390249b85965812ce80ecdaf4de7c9d061ae1c320a43600` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.0/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.0/x86_64-linux/vzik) | `913fd861477dbc7e32420988645300a90db8c8c61484f332e9a16159ed0d5493` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.0/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.0/aarch64-linux/vzik) | `c6f55c63f91df0dac7e0351c8eb028722b8a78486c632a6287949356af9bded0` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.0/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.0/aarch64-darwin/vzik) | `58bfa37f816ee40702ad539719c692cc3ef7fd995e004f0352c364616a46e008` |

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
