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

Release [v0.11.0](https://github.com/buglloc/axe/releases/tag/v0.11.0). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.11.0/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.11.0/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.11.0/x86_64-linux/axe) | `6f1fb0fadbf6355b5e1f7093ca4c2667fc2b7e8f4a6025f3f69c3ce234e65639` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.11.0/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.11.0/aarch64-linux/axe) | `9a69be41b34e65a2c400ac9dc640fe73f60aa48613d03debc94ddcdd128c5b23` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.11.0/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.11.0/aarch64-darwin/axe) | `30c1ecf805240bf0ed3722361f14c09e59b54a92530618dca0ee45b69c49b57e` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.11.0/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.11.0/x86_64-linux/axe-relay) | `a4c4b84069a74755cc02e3c44341f57530588d6f11ec6d0ee9ec0fb6d94c80c4` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.11.0/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.11.0/aarch64-linux/axe-relay) | `0b1c2755983912666dec06c2ff4ff20f9470659d3d40ca6e31e07ca0df4c0b08` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.11.0/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.11.0/aarch64-darwin/axe-relay) | `2b411d424efd4f1a4218656bcf4d118e665357bf3ec87e9908457d9b54ab94d2` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.11.0/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.11.0/x86_64-linux/vzik) | `76da69d353a58cc787d22700867fedc33582f7656dff402dea3c514e0febdbeb` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.11.0/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.11.0/aarch64-linux/vzik) | `1c5ccde201844f65d19f294cff7951390e8fac6675c21f136f42d4e2be0c9713` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.11.0/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.11.0/aarch64-darwin/vzik) | `f71e124e320df6ebb7db77547d292b4fc53b357590bc62dd13a810690851f790` |

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
