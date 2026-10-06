+++
title = "Store patches"
description = "Patches to AXE Store tools for offline operation and static Linux builds."
eyebrow = "Store patches"
+++

AXE Store patches upstream tools when their defaults require network access at startup, depend on the target system, or prevent a static Linux build. The changes are listed below.

## Privacy and network defaults

### `dnsx`

Makes `-disable-update-check` / `-duc` default to `true`. Automatic version checks no longer run during normal startup. The flag remains accepted and the explicit `-update` / `-up` command remains functional.

### ProjectDiscovery `cdncheck`

Disables the startup IPv6 connectivity probe in these tools:

- `dnsx`
- `httpx`
- `naabu`
- `nuclei`
- `subfinder`

### `httpx`

Disables the automatic version check and built-in DNS resolver fallback. User-supplied resolvers and normal target traffic are unchanged.

### `naabu`

Removes automatic version checks. The legacy update flag remains accepted as a no-op in the Store build.

### `nuclei`

Disables binary and template update checks, points the runtime at bundled templates, and keeps the Store package independent of a writable user template installation.

`subfinder` disables automatic version checks while preserving its explicit update command.

### `gori` and `interactsh-client`

`gori` disables startup version checks by default; users can opt in through its settings. `interactsh-client` removes the startup version check.

## Portability patches

| Tool | Change |
| --- | --- |
| `naabu` | Removes runtime packet-capture dependencies for static Linux builds. |
| `xh` | Uses AXE's bundled CA roots instead of the host trust store. |
| `grpcurl` | Uses AXE's bundled CA roots by default; `-cacert` replaces them. |
| `gobuster` | Uses AXE's bundled CA roots when TLS verification is enabled. |
| `capsh` | Uses the AXE-provided shell instead of requiring a host shell. |

These TLS tools use bundled Mozilla roots plus any additional CA bundle selected by the edition. They do not merge certificates from the host trust store.
