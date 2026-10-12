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

Release [v0.5.2](https://github.com/buglloc/axe/releases/tag/v0.5.2). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.5.2/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.2/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.2/x86_64-linux/axe) | `4c4bcb1d210769a08e6fa2fdeb123cd7d829b6a4c51417810c4730bf7278d65b` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.2/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.2/aarch64-linux/axe) | `93b103ff7e7425fa61afeaca3b090e09239b3fe13c4c585d26adee43206dc97f` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.2/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.2/aarch64-darwin/axe) | `bdb63bc4971f4314b4ba8abadeee9e4d35fb07416ede5cfba5e4d03a5b6dab84` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.2/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.2/x86_64-linux/axe-relay) | `10403b8128cd9dcb5ec8468b8a41074afc2a856b13e264038e4e1eb92ca856d8` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.2/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.2/aarch64-linux/axe-relay) | `d1324ac8d1658ab7d7019d8d8fac8675d613d75cf8c0177d1befb49caaebf302` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.2/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.2/aarch64-darwin/axe-relay) | `36426bd2851c324768cdce803ef3593ae30c2990d2630812ede1d86729b2dcb1` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.2/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.2/x86_64-linux/vzik) | `c2ce0a41338b42dbb836708446560c92260afec01a48d08dddbc41de9c4e8283` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.2/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.2/aarch64-linux/vzik) | `47c2ba7cc1b1e390abe43ee76b4946342dba16a422e549886bd71d237e970aac` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.5.2/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.5.2/aarch64-darwin/vzik) | `8f1228cb4850c15a8ac0f388a95a855e3b4e7681fa537e9734c90706bd41fdb3` |

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
