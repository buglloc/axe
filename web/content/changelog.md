+++
title = "Changelog"
+++

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
