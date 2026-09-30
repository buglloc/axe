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

Release [v0.2.1](https://github.com/buglloc/axe/releases/tag/v0.2.1). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.2.1/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.1/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.1/x86_64-linux/axe) | `d4ea12f4ab77eb61e5dbfea872e57b75eb19d2912070884bda8995d9585a4313` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.1/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.1/aarch64-linux/axe) | `f2d0638a1c55485da913ad872f95cf7a02973dac71e6f884f4bc648a5ee7dbb3` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.1/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.1/aarch64-darwin/axe) | `61832fcfe4c26843876b64429013868625234355b96b15370ed18680829929dc` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.1/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.1/x86_64-linux/axe-relay) | `6178680ae8f5bc8e4da5deafff21bbf4a4da5c35b4d4417eca39fe15c51de655` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.1/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.1/aarch64-linux/axe-relay) | `af126b90aab2b7eccb4fe4b3709f8bde0c6c3a0ed3b364c1eb26539ff86da03c` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.1/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.1/aarch64-darwin/axe-relay) | `c754a4f5f3cdfc220d979a44d98c66f070d70493817c19d80448235e8de85a30` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.1/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.1/x86_64-linux/vzik) | `6af48b933afd1f098484dcf95f56bdc84c9231fa7ad0138a678dc588bfcb9a40` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.1/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.1/aarch64-linux/vzik) | `c770fb0e4732ab9d3d5bffb637c58bb15fb7689d1fd206ee78444b995621fe05` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.1/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.1/aarch64-darwin/vzik) | `7fe5509873fd63511b9d8772d6c52f6c3aced4750743f07f6f6c5f4e8b35383f` |

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
