+++
title = "Changelog"
+++

## [v0.4.0](https://github.com/buglloc/axe/releases/tag/v0.4.0) — 2026-10-06

### Highlights

- Added bundled `killall` on Linux (`167cf49a7116ef915df9c0dc79ab934bb5309fef`).
- Added real thread listings and native typed sorting to `ps` (`108b1a754f769329d8c8ce88a34925cb289d175f`).

### Changes

- Release preparation now allows Cargo registry index access when updating workspace dependencies, rather than requiring an offline update (`f9070f5e0143cf91f72378a3eda081f3b71605a9`).

### Fixes

- Fixed BSD-style `ps ax` selection to include other users’ processes without a TTY (`33a81cdf6a47adacf3b46158c0dcdf6cd4ec5b1a`).
- Improved `ps` numeric fields and output-width compatibility. The attached evidence does not specify which fields or width cases changed (`7bb8ebc81a83fb7cc1d1516a424f81da0717c82c`).
- Fixed executable preparation in the Store/PATH fallback test: copying now runs in an owned subprocess that exits before execution, preventing an `ETXTBSY` race with parallel tests (`97eea583602d62e84075283f33b30190380c621c`).

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

- PTY sessions stalled once russh's per-channel data queue filled: the server queued every data packet before calling the handler, and the shell read through the handler, so the unused receiver blocked the connection. The handler now drops that receiver when it starts the shell, and input keeps flowing. A regression test sends 128 keystrokes past the buffer and expects `INPUT_OK` (`58fba6b0463058f816568559be41152be2a93988`).

## [v0.2.1](https://github.com/buglloc/axe/releases/tag/v0.2.1) — 2026-09-30

### Highlights

- AXE now uses a shared `.axe-bridge/bin` directory for bundled commands instead of publishing separate bridge generations. Applet links point to the bridge’s `axe` link. (`81d8c80e4cc2c1ba599c6628665822585c76c075`)

### Fixes

- Bridge publication now uses a file lock rather than deleting a lock based on its age, so an old timestamp cannot let another publisher take an active lock. (`81d8c80e4cc2c1ba599c6628665822585c76c075`)
- When republishing, AXE updates the executable link and removes applet links no longer in the bundled inventory. (`81d8c80e4cc2c1ba599c6628665822585c76c075`)

## [v0.2.0](https://github.com/buglloc/axe/releases/tag/v0.2.0) — 2026-09-29

### Highlights

- The SSH server accepts user certificates signed by ECDSA CAs using NIST P-256, P-384, or P-521 keys. Plain public keys remain rejected. (`1342f9b194ce0fc7c7b578386f5f16ca79b17a9b`)
- `axe-relay` server startup now requires `--config FILE`. Its listener addresses, ports, certificate paths, and token-file path come from that JSON configuration; `--token FILE` can override the configured token file. (`18442077bb9627218ed7a963f6d6721a11d180aa`)

### Changes

- New guides cover AXE architecture and custom editions; the relay guide documents file-backed server configuration and service setup. (`52b1fea340bb6fb0f9adc8fb119fa7453726c01f`)
- The bootstrap guide has been shortened and points to the component guides for build and publication details. (`9f8b3eebd738530bfdfb77c3cfcd2ffb0052ab85`)
- The AXE Store skill documentation distinguishes single-package checks from signed-snapshot verification and publication. (`6fbea0d14f566a946b9afafb40d4a62c1f146110`)

### Fixes

The supplied evidence does not establish a separate user-visible bug fix in this release.

## [v0.1.2](https://github.com/buglloc/axe/releases/tag/v0.1.2) — 2026-09-29

### Highlights

- AXE Store now includes Yandex Cloud CLI (`yc`) 1.25.0 for Linux x86_64 and aarch64, and macOS aarch64. (`6530237d0cfddce1ce13d98c5ef5821e2bd60136`)

### Changes

- The release process now builds and publishes `axe`, `axe-relay`, and `vzik` for all three supported release targets. Release metadata and download-table generation cover all nine artifacts. (`336a06e352e557c766b1b478dc4596c4e8db3381`)

### Fixes

- The supplied evidence does not establish a user-visible bug fix in this release.

## [v0.1.1](https://github.com/buglloc/axe/releases/tag/v0.1.1) — 2026-09-27

### Highlights

- Added `dnsdomainname`, `rgrep`, and `gzcat` aliases. The supplied evidence does not establish their exact invocation behavior. (`cdccfd7f4f79678bff89c7c73f5ec7ffb4128aec`)
- Consolidated applet dispatch and compression drivers. The supplied evidence does not establish a user-visible behavior change from this refactor. (`f34c26d8a7295be0cf70d1141c8ecf1875cb26d8`)

### Changes

- Removed the Cargo applet feature matrix and switched three Brush crates from vendored copies to upstream dependencies. (`3eb50c10435eee4aa0dbe519eb827e03fa8bbe1c`, `e0e74cc8270bf2ef37d9f34d89919034dac0757f`)

### Fixes

- Store metadata was consolidated and stale indexes are revalidated. The supplied evidence does not establish which user-visible failure this fixes. (`e62c318276bb28ad4012046cd3ccb45530a363d6`)

## [v0.1.0](https://github.com/buglloc/axe/releases/tag/v0.1.0) — 2026-09-26

### Highlights

- Initial AXE release, Store components, relay, and `vzik`.
