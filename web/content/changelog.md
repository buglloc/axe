+++
title = "Changelog"
+++

## [v0.5.1](https://github.com/buglloc/axe/releases/tag/v0.5.1) — 2026-10-11

### Highlights

- Unified bundled and AXE Store command categories by purpose. Website filters and command inventory rows now use the same category IDs and labels. (`eeb520864bc3`)

### Changes

- Updated category values in `commands` output: for example, `cp` uses `files`, `chmod` uses `security`, `jq` uses `text`, and AXE controls such as `doctor` use `axe`. Aliases retain their canonical command’s category, including `dnsdomainname` → `system` and `gunzip` → `archives`. (`eeb520864bc3`)
- Renamed 15 Store package IDs to match their categories:
  - `files/fd`, `files/fzf`, and `files/rg` replace the corresponding `search/` IDs.
  - `text/jq`, `text/sqlite3`, and `text/yq` replace the corresponding `data/` IDs.
  - `storage/findmnt`, `storage/fio`, `storage/rclone`, `storage/restic`, `storage/rsync`, and `storage/s5cmd` replace IDs under `containers/`, `debugging/`, `network/`, or `archives/`.
  - `terminal/tmux` and `terminal/zellij` replace the corresponding `runtime/` IDs.
  - `debugging/binwalk` replaces `security/binwalk`. (`eeb520864bc3`)
- Website command search now matches the displayed category labels as well as category IDs, command names, synopses, and sources. (`eeb520864bc3`)

## Unreleased

### Changes

- Unified bundled and Store command categories by purpose. Website filters and inventory rows use the same category IDs and labels.
- Renamed 15 Store package IDs to match their categories, including `files/rg`, `text/jq`, `storage/restic`, and `terminal/tmux`. Regenerate the signed Store bootstrap snapshot before an AXE release.

## [v0.5.0](https://github.com/buglloc/axe/releases/tag/v0.5.0) — 2026-10-10

### Highlights

- Added bundled `zstd` compression and decompression, with multithreaded compression, stdout and explicit-file output, and compression levels 1–22. Input files are kept by default. (`72a9eebd7a43`)
- Added passive host overview and targeted process inspection to Vzik. `axe doctor` now reuses passive host observations. (`4f92a25e49db`)
- Expanded the signed Store bootstrap snapshot with 19 tools: `7zz`, `age`, `bwrap`, `caddy`, `cek`, `dig`, `ethtool`, `fio`, `helm`, `layerx`, `objdump`, `rclone`, `readelf`, `restic`, `s5cmd`, `stern`, `tmux`, `websocat`, and `zellij`. Existing snapshot entries are unchanged. (`0e86124ba06f`)

### Changes

- The default shell prompt now shows the current directory. (`340f090767db`)
- Vzik evidence moves to protocol v4, with explicit observation scope and outcomes, degraded interface-fallback reporting, and clearer bus selection. (`4f92a25e49db`)
- Vzik adds version output, pretty-printed metadata documents, and capture into a new private directory. Captures reject destination collisions, preserve JSONL framing, and retain non-UTF-8 artifact paths losslessly. (`4f92a25e49db`)
- Updated agent skills for Vzik protocol v4, passive overview, and one-process inspection. The guidance prefers `jq` selection from a saved snapshot while respecting explicit stdout-only constraints. (`aabb3da9d7fc`)

### Fixes

- Fixed Darwin Go binary output paths in AXE Store builds. (`82a67df718be`)
- Made relay tests independent of edition credentials. This is a test-isolation fix; the supplied evidence does not establish a relay runtime behavior change. (`353d32338173`)

## [v0.4.0](https://github.com/buglloc/axe/releases/tag/v0.4.0) — 2026-10-06

### Highlights

- Added bundled `killall` on Linux (`167cf49a7116ef915df9c0dc79ab934bb5309fef`).
- Added real thread listings and native typed sorting to `ps` (`108b1a754f769329d8c8ce88a34925cb289d175f`).

### Changes

- Release preparation now allows Cargo registry index access when updating workspace dependencies, rather than requiring an offline update (`f9070f5e0143cf91f72378a3eda081f3b71605a9`).

### Fixes

- Fixed BSD-style `ps ax` selection to include other users’ processes without a TTY (`33a81cdf6a47adacf3b46158c0dcdf6cd4ec5b1a`).
- Improved `ps` numeric fields and output-width compatibility (`7bb8ebc81a83fb7cc1d1516a424f81da0717c82c`).
- Fixed an executable-copy race in the Store/PATH fallback test (`97eea583602d62e84075283f33b30190380c621c`).

## [v0.3.0](https://github.com/buglloc/axe/releases/tag/v0.3.0) — 2026-10-04

### Highlights

- Added `gori` 0.7.1 to AXE Store for Linux x86_64 and ARM64. It provides a terminal HTTP intercepting proxy and web application testing tools (`a65ecedb33206b46c9687380086cb07f9f29b4cc`).
- Added `interactsh-client` 1.3.1 to AXE Store for Linux x86_64, Linux ARM64, and macOS ARM64. It generates out-of-band testing payloads and collects interactions (`1b65c0e8678b8dad1f487cd5647ef6486781110e`).

### Changes

- The Store build of `gori` disables automatic startup update checks by default; users can opt in through its settings (`a65ecedb33206b46c9687380086cb07f9f29b4cc`).
- The Store build of `interactsh-client` removes the startup version check (`1b65c0e8678b8dad1f487cd5647ef6486781110e`).

## [v0.2.2](https://github.com/buglloc/axe/releases/tag/v0.2.2) — 2026-10-01

### Highlights

SSH PTY sessions keep accepting input after the channel buffer fills (`58fba6b0463058f816568559be41152be2a93988`). The release also switches the website download instructions to the S3 binary (`7c2065fe9e16bddfb0ae3e82d4dced5b13dd4dbe`) and refreshes release metadata (`34ecc2c831583d1ac0dd277a0c90148b920039f7`).

### Fixes

- Fixed SSH PTY input stalls when the channel buffer fills (`58fba6b0463058f816568559be41152be2a93988`).

## [v0.2.1](https://github.com/buglloc/axe/releases/tag/v0.2.1) — 2026-09-30

### Highlights

- Simplified the managed PATH directory for bundled commands (`81d8c80e4cc2c1ba599c6628665822585c76c075`).

### Fixes

- Prevented concurrent PATH publishers from bypassing an active lock (`81d8c80e4cc2c1ba599c6628665822585c76c075`).
- Updated executable links and removed obsolete command links during PATH publication (`81d8c80e4cc2c1ba599c6628665822585c76c075`).

## [v0.2.0](https://github.com/buglloc/axe/releases/tag/v0.2.0) — 2026-09-29

### Highlights

- The SSH server accepts user certificates signed by ECDSA CAs using NIST P-256, P-384, or P-521 keys. Plain public keys remain rejected. (`1342f9b194ce0fc7c7b578386f5f16ca79b17a9b`)
- `axe-relay` server startup now requires `--config FILE`. Its listener addresses, ports, certificate paths, and token-file path come from that JSON configuration; `--token FILE` can override the configured token file. (`18442077bb9627218ed7a963f6d6721a11d180aa`)

### Changes

- New guides cover AXE architecture and custom editions; the relay guide documents file-backed server configuration and service setup. (`52b1fea340bb6fb0f9adc8fb119fa7453726c01f`)
- The bootstrap guide has been shortened and points to the component guides for build and publication details. (`9f8b3eebd738530bfdfb77c3cfcd2ffb0052ab85`)
- The AXE Store skill documentation distinguishes single-package checks from signed-snapshot verification and publication. (`6fbea0d14f566a946b9afafb40d4a62c1f146110`)

## [v0.1.2](https://github.com/buglloc/axe/releases/tag/v0.1.2) — 2026-09-29

### Highlights

- AXE Store now includes Yandex Cloud CLI (`yc`) 1.25.0 for Linux x86_64 and aarch64, and macOS aarch64. (`6530237d0cfddce1ce13d98c5ef5821e2bd60136`)

### Changes

- The release process now builds and publishes `axe`, `axe-relay`, and `vzik` for all three supported release targets. Release metadata and download-table generation cover all nine artifacts. (`336a06e352e557c766b1b478dc4596c4e8db3381`)

## [v0.1.1](https://github.com/buglloc/axe/releases/tag/v0.1.1) — 2026-09-27

### Highlights

- Added `dnsdomainname`, `rgrep`, and `gzcat` aliases (`cdccfd7f4f79678bff89c7c73f5ec7ffb4128aec`).
- Consolidated applet dispatch and compression drivers (`f34c26d8a7295be0cf70d1141c8ecf1875cb26d8`).

### Changes

- Removed the Cargo applet feature matrix and switched three Brush crates from vendored copies to upstream dependencies. (`3eb50c10435eee4aa0dbe519eb827e03fa8bbe1c`, `e0e74cc8270bf2ef37d9f34d89919034dac0757f`)

### Fixes

- Consolidated Store metadata and revalidated stale indexes (`e62c318276bb28ad4012046cd3ccb45530a363d6`).

## [v0.1.0](https://github.com/buglloc/axe/releases/tag/v0.1.0) — 2026-09-26

### Highlights

- Initial AXE release, Store components, relay, and `vzik`.
