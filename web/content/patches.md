+++
title = "Store patches"
description = "Changes AXE Store makes to upstream tools: update checks, first-start behavior, host files, CA roots and static builds."
+++

AXE Store patches an upstream tool when it contacts the network on its own, depends on files from the target system, creates configuration on first start, or cannot be built as a static Linux binary.

## Update checks and downloads

### `dnsx`

`-disable-update-check` / `-duc` defaults to `true`, so version checks do not run at startup. The flag is still accepted, and `-update` / `-up` still update the tool.

### ProjectDiscovery `cdncheck`

The startup IPv6 connectivity probe is disabled in `dnsx`, `httpx`, `naabu`, `nuclei` and `subfinder`.

### `httpx`

The automatic version check is removed. httpx does not add the built-in public resolvers (`1.1.1.1`, `1.0.0.1`, `8.8.8.8`, `8.8.4.4`) and resolves names through the host's resolver configuration. Resolvers you pass and requests to targets are unchanged.

### `naabu`

Automatic version checks are removed. `-update` / `-up` are still accepted and do nothing.

### `nuclei`

Binary and template update checks are disabled. Nuclei uses the templates shipped next to it in the Store package and does not need a writable template directory.

### `katana`

The startup version check is disabled, and katana does not download a browser. Headless and hybrid crawling need an installed Chrome or Chromium, `-system-chrome-path`, or `-chrome-ws-url`.

### `gori`

Startup version checks are off by default and can be enabled in its settings.

### `interactsh-client`

The startup version check is removed.

### `superfile` (`spf`)

The startup update check is removed.

### `rclone`

`rclone version --check`, `selfupdate` and the WebGUI are removed.

### `caddy`

The `caddy upgrade` command is removed.

## First start and host files

| Tool | Change |
| --- | --- |
| `superfile` (`spf`) | Uses its embedded default configuration and does not create configuration files on first start. |
| `ox` | The configuration assistant runs only with `--config-assist`, not whenever the configuration file is missing. |
| `broot` | Does not offer to install its shell integration or write missing configuration files. Installation follows `--install`; `BR_INSTALL` is ignored. |
| `micro` | The built-in linter is off by default. |
| `less` | Built with static ncurses and compiled-in descriptions for common terminals, including Kitty and Ghostty, so it does not depend only on the host terminfo database. |
| `yara` | The `magic` module uses an embedded file-identification database. |
| `zgrab2` | `--blocklist-file -` uses an embedded default blocklist when `$HOME/.config/zgrab2/blocklist.conf` does not exist. |

## OpenSSH client

The Store OpenSSH client does not read or update `known_hosts` files and accepts any host key: `StrictHostKeyChecking` defaults to `no`, and the system and user known-hosts files default to `/dev/null`. Weak-crypto warnings are off. To verify host keys, pass `-o StrictHostKeyChecking=yes -o UserKnownHostsFile=PATH` or set these options in an SSH config file.

## CA roots

The bundled roots are the Mozilla set plus any CA bundle the edition adds.

| Tool | Change |
| --- | --- |
| `xh` | Uses only the bundled roots. |
| `grpcurl` | Uses only the bundled roots by default; `-cacert` replaces them. |
| `gobuster` | Uses only the bundled roots when TLS verification is enabled. |
| `usql` | PostgreSQL connections with `sslmode=verify-full` use the bundled roots when no root certificate is given. |
| `valkey-cli` | Uses the bundled roots when no CA path is configured. |
| `websocat` | Trusts the bundled roots in addition to the host trust store. |
| `betterleaks`, `caddy`, `cek`, `katana`, `restic`, `s5cmd` | Use the bundled roots only when the host has no usable system roots. |

## Portability

| Tool | Change |
| --- | --- |
| `naabu` | Packet-capture runtime dependencies are removed so it builds as a static Linux binary. |
| `capsh` | Started from an AXE shell, runs that shell instead of `/bin/sh`, so the host needs no shell of its own. |
