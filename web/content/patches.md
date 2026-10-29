+++
title = "Store patches"
description = "Patches to AXE Store tools for offline operation and static Linux builds."
+++

AXE Store patches an upstream tool when it contacts the network at startup, depends on files from the target system, or cannot be built as a static Linux binary.

## Network access at startup

### `dnsx`

`-disable-update-check` / `-duc` defaults to `true`, so version checks do not run at startup. The flag is still accepted, and `-update` / `-up` still update the tool.

### ProjectDiscovery `cdncheck`

The startup IPv6 connectivity probe is disabled in `dnsx`, `httpx`, `naabu`, `nuclei` and `subfinder`.

### `httpx`

The automatic version check and the built-in DNS resolver fallback are disabled. Resolvers you pass and requests to targets are unchanged.

### `naabu`

Automatic version checks are removed. The old update flag is still accepted and does nothing.

### `nuclei`

Binary and template update checks are disabled. Nuclei uses the bundled templates and does not need a writable template directory.

### `subfinder`

Automatic version checks are disabled. The explicit update command still works.

### `gori`

Startup version checks are off by default and can be enabled in its settings.

### `interactsh-client`

The startup version check is removed.

## Portability

| Tool | Change |
| --- | --- |
| `naabu` | Packet-capture runtime dependencies are removed so it builds as a static Linux binary. |
| `xh` | Uses AXE's bundled CA roots instead of the host trust store. |
| `grpcurl` | Uses AXE's bundled CA roots by default; `-cacert` replaces them. |
| `gobuster` | Uses AXE's bundled CA roots when TLS verification is enabled. |
| `capsh` | Uses the AXE shell, so the host needs no shell of its own. |

The bundled roots for `xh`, `grpcurl` and `gobuster` are the Mozilla set plus any CA bundle the edition adds. Certificates from the host trust store are not used.
