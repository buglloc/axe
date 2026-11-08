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

Release [v0.16.1](https://github.com/buglloc/axe/releases/tag/v0.16.1). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.16.1/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.1/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.1/x86_64-linux/axe) | `2dc55d768d99c2a4bb94e0971f3e5c40bab65b69f6490249d840e77b5487b4d9` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.1/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.1/aarch64-linux/axe) | `39867241bacd9b5f544ad99dbcff0130fdd1a48b4aa29e58aacf4e81070d7a57` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.1/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.1/aarch64-darwin/axe) | `9d37480548c3a92d96d0102b7e35dc1c21351e9c1c2999ef1d5f8054ad861375` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.1/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.1/x86_64-linux/axe-relay) | `8e15c614df62680a939a12435c8178b027f0ac23b4d6eff815b2d5eee33c4088` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.1/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.1/aarch64-linux/axe-relay) | `7662378046e1d2685277aa1b0cc834e5f824a36271c30eb0634953bdba17fe1b` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.1/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.1/aarch64-darwin/axe-relay) | `d1ae3a059e13ab15943a74910b3027b47bb6b37dea5872986248fe231b99b7f3` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.1/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.1/x86_64-linux/vzik) | `bd6417d94f2afab494c848cee1fc66ab82ae2a9cc7d9048dc968c2d9356ea207` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.1/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.1/aarch64-linux/vzik) | `4cd41fdc24e16224ba271996a050ba8f11e552ff577685d1e79d30dbf02836e3` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.16.1/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.16.1/aarch64-darwin/vzik) | `7caddfac3cbedf427ac5dc82ae892c6e2086c174a642c1ac6f006f93546d4ceb` |

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
