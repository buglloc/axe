+++
title = "Store patches"
description = "Downstream patches applied to AXE Store tools for deterministic, offline-safe behavior."
eyebrow = "Downstream policy"
+++

AXE Store carries a small set of downstream patches where upstream defaults conflict with a portable rescue environment. Each patch is scoped to a concrete runtime requirement: no implicit network activity, no dependency on the target system, or a fully static executable.

## Privacy and network defaults

### `dnsx`

`store/nix/patches/dnsx-disable-automatic-update-check.patch`

Makes `-disable-update-check` / `-duc` default to `true`. Automatic version checks no longer run during normal startup. The flag remains accepted and the explicit `-update` / `-up` command remains functional.

### ProjectDiscovery `cdncheck`

`store/nix/patches/cdncheck-disable-ipv6-probe.patch`

Removes the vendored package's import-time IPv6 connectivity probe. Without this patch, merely loading `cdncheck` calls UDP `connect()` against Google's public IPv6 resolver. The patch is applied after Go vendor setup to:

- `dnsx`
- `httpx`
- `naabu`
- `nuclei`
- `subfinder`

The resolver lists remain available to code that explicitly needs them; only the package initialization side effect is removed.

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

The TLS patches receive a build-time bundle composed of the pinned Mozilla roots and, when selected by an external edition, its additional CA bundle. The resulting bundle replaces the host trust store; it is not merged with system certificates.

## Application model

Top-level source patches are appended to the package's existing nixpkgs patch list. The shared `cdncheck` patch is applied to `vendor/github.com/projectdiscovery/cdncheck` in `postConfigure`, after `buildGoModule` materializes the vendor tree. This keeps the upstream module graph and vendor hashes unchanged while applying the same policy to every affected binary and target architecture.
