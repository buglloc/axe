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

Release [v0.3.0](https://github.com/buglloc/axe/releases/tag/v0.3.0). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.3.0/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.3.0/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.3.0/x86_64-linux/axe) | `1f93e91b9d435146bfc609c01b2094a42117ce33ca10c5d30966daa772b5e53b` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.3.0/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.3.0/aarch64-linux/axe) | `bc954dec16baa76ff87ee4e327e7c0f044e4dd26467bb3ad569c324b1f986046` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.3.0/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.3.0/aarch64-darwin/axe) | `73af292e0895dd96274191e8cfd361ae9dd546d57fd4729dc42792b456694dbb` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.3.0/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.3.0/x86_64-linux/axe-relay) | `c3ca1f09859c30651ff6b7eab2c68f8a262258e79180cf373b85d2f841f37533` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.3.0/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.3.0/aarch64-linux/axe-relay) | `426dee3f0088270d1d2d4dc61c373a9a0b22bb73889e21ee11ccb37b6aa15ff8` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.3.0/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.3.0/aarch64-darwin/axe-relay) | `ffb31ff23a1c2356d1c51789a4b1ee5f986ba7d498c72e7498f27a2b4461f53b` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.3.0/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.3.0/x86_64-linux/vzik) | `af9a7d7f4d00446bcfe69ceee7114102f5617bfdcd838c724182579c339fbc82` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.3.0/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.3.0/aarch64-linux/vzik) | `638b2e369d2a437b9f5d6608d35831756edd44acf6c593373921d5bf8e62595a` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.3.0/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.3.0/aarch64-darwin/vzik) | `81c7da5b3f2830904b90c8a1c3386ba2ac047652234a80c34474f8272a2cb791` |

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
