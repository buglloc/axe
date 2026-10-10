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

Release [v0.5.0](https://github.com/buglloc/axe/releases/tag/v0.5.0). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.5.0/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.0/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.0/x86_64-linux/axe) | `b3a089339ab4335ae4ff5f3be4f11a7ed97bdfb28c998ed47e21b2c6b77f5988` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.0/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.0/aarch64-linux/axe) | `edf200b6115eb0ced2cbd2e53ed2cbffe23eb570fec50a05036091002e5f45ba` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.0/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.0/aarch64-darwin/axe) | `c15042d7add396673dbc6f9760cba649b7353bcb4a0b660283ef0b596647fba1` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.0/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.0/x86_64-linux/axe-relay) | `d790ce38261a8d9a76c8b9b9c13bd366bac7a6c62c573b8cd592f9595076862a` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.0/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.0/aarch64-linux/axe-relay) | `58efef700ea7bf8c0b2b9bccce4535ab1dd990997320558fc42619b1fa6134bc` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.0/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.0/aarch64-darwin/axe-relay) | `2125d227f7e82e395e6d99756a1b14bdee09e86519db4ccfc71c5da27fae6a0f` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.0/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.0/x86_64-linux/vzik) | `648f49aac3452100dcfe35fda79d6b97fbdcc3051e68820da3b60cc6dec1296c` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.0/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.0/aarch64-linux/vzik) | `6aa64ed59953f83580eb6d92bce0ed767e2f76081ab745d900954e2b5e672649` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.0/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.0/aarch64-darwin/vzik) | `506a3098bc7314c5f38a509ee5ef699eedfc323a6f80a853bd07a1e9af1e541d` |

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
