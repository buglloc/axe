+++
title = "Store patches"
description = "Patches to AXE Store tools for offline operation and static Linux builds."
eyebrow = "Store patches"
+++

AXE Store patches upstream tools when their defaults require network access at startup, depend on the target system, or prevent a static Linux build. The changes are listed below.

## Privacy and network defaults

### `dnsx`

`store/nix/patches/dnsx-disable-automatic-update-check.patch`

Makes `-disable-update-check` / `-duc` default to `true`. Automatic version checks no longer run during normal startup. The flag remains accepted and the explicit `-update` / `-up` command remains functional.

### ProjectDiscovery `cdncheck`

`store/nix/patches/cdncheck-disable-ipv6-probe.patch`

Removes an IPv6 connectivity probe that runs when the vendored `cdncheck` package loads. Without the patch, importing the package calls UDP `connect()` against Google's public IPv6 resolver. It is applied after Go vendor setup to:

- `dnsx`
- `httpx`
- `naabu`
- `nuclei`
- `subfinder`

Resolver lists remain available for explicit use. The patch removes only the initialization-time probe.

### `httpx`

`store/nix/patches/httpx-axe-defaults.patch`

Disables the automatic version check and removes the built-in fastdialer resolver fallback. User-supplied resolvers and normal target traffic are unchanged.

### `naabu`

`store/nix/patches/naabu-disable-updates.patch`

Removes automatic version checks. The legacy update flag remains accepted as a no-op in the Store build.

### `nuclei`

`store/nix/patches/nuclei-axe-defaults.patch`

Disables binary and template update checks, points the runtime at bundled templates, and keeps the Store package independent of a writable user template installation.

`subfinder` also inherits nixpkgs' `disable-update-check.patch`, which disables its automatic check while preserving the explicit update command.

## Portability patches

| Patch | Tool | Purpose |
| --- | --- | --- |
| `naabu-fully-static.patch` | `naabu` | Replaces runtime packet-capture dependencies and keeps Linux builds fully static. |
| `xh-bundled-roots.patch` | `xh` | Uses the CA roots embedded by AXE instead of merging certificates from the host. |
| `grpcurl-bundled-roots.patch` | `grpcurl` | Uses the embedded AXE CA bundle by default while preserving `-cacert` as an explicit replacement. |
| `gobuster-bundled-roots.patch` | `gobuster` | Uses the embedded AXE CA bundle whenever TLS verification is enabled. |
| `capsh-use-axe-shell.patch` | `capsh` | Uses the AXE-provided shell path instead of assuming a host shell exists. |

The TLS patches use a build-time bundle of pinned Mozilla roots plus any additional CA bundle selected by an external edition. They replace the host trust store rather than merging with it.

## Application model

Top-level source patches are added to the package's nixpkgs patch list. The shared `cdncheck` patch is applied to `vendor/github.com/projectdiscovery/cdncheck` in `postConfigure`, after `buildGoModule` creates the vendor tree. The upstream module graph and vendor hashes remain unchanged; the patch applies to each affected binary and target architecture.
