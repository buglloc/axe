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

Release [v0.15.1](https://github.com/buglloc/axe/releases/tag/v0.15.1). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.15.1/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.1/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.1/x86_64-linux/axe) | `c8c90ba921faeb718805d958fbcc6637c908dcb9e19de7f6ada3a8e932fcfe3f` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.1/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.1/aarch64-linux/axe) | `4edd5a34f6ba25f58177b59b165bdcb1d626e2df20a119376ed83daeb6d432a4` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.1/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.1/aarch64-darwin/axe) | `7203daf23f6a14940285ad4f4c0f8d549469a129d75da3de11c82f819ab899b3` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.1/axe-relay-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.1/x86_64-linux/axe-relay) | `4e8d9a351abce1a7a03d32be82045f8878e9b70dde460ba23721a383e31a57c4` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.1/axe-relay-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.1/aarch64-linux/axe-relay) | `a70257ce8c09d56f8b3135fa84f10b26784d9d406a0c89d6d14e6a8d09d2ad07` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.1/axe-relay-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.1/aarch64-darwin/axe-relay) | `f6a6de0346600d708d5b7f4dd059f5a6025f09d38e3f75dec120f5df90c7af68` |
| vzik x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.1/vzik-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.1/x86_64-linux/vzik) | `1e7944a507807b8f0963396ec0fd72aa8fd495cc69c4a8286ac7bb39846b10c4` |
| vzik aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.1/vzik-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.1/aarch64-linux/vzik) | `5b7d24287c868a209c9a2b00f221e935c7e7c6665df1196263f269df4ed5783a` |
| vzik aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.15.1/vzik-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.15.1/aarch64-darwin/vzik) | `bfd778263abd3ceb89c7bbe771402ed86e9d7f79eab9a637bb2605701747617b` |

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
