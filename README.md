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

Release [v0.2.2](https://github.com/buglloc/axe/releases/tag/v0.2.2). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.2.2/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.2/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.2/x86_64-linux/axe) | `a368b59e07ed1ad75c6cadace9399f02f837ae9c9926304c064772713d616351` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.2/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.2/aarch64-linux/axe) | `0d2eabd0dca5df082d8f0c1ea0c5badcd98a308e0f24604df25709456b86ef2b` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.2/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.2/aarch64-darwin/axe) | `f7ff12b9ef5c29ef601b74e1cc173d16514f0af91291beefd98b52e6aaff7428` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.2/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.2/x86_64-linux/axe-relay) | `b409b884a618bb9808a1063fb634fe880cc21633dd0a80d6f2ce793aece42a23` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.2/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.2/aarch64-linux/axe-relay) | `75eed2bdfe6c328c73dc5281e7a4d41415b4dad15eaef55a5bfbfa00384835c1` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.2/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.2/aarch64-darwin/axe-relay) | `57add2a7093787d11c1780c2cc212ae9671a4f362c9b1a0947a879682e309537` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.2/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.2/x86_64-linux/vzik) | `0d77bbd35be074eba46f578d0c12a5c0372ea6dce311d02bd7e8dadb91777672` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.2/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.2/aarch64-linux/vzik) | `4cf036a90ae19fde1e4f0316baac7f6adbb719a8e227da82d1fd0fdba63ad7f2` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.2.2/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.2.2/aarch64-darwin/vzik) | `31130be035bdb655f093ae385a0821ad7a11b28515a71ce0b4af84f98b7ec302` |

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
