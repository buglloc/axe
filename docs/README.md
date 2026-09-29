# Documentation

Start with the [project README](../README.md) for downloads, a first run, and a brief software overview.

## Using AXE

- [Commands](commands.md) — shell resolution, applet invocation, and bundled command inventory.
- [HTTP applet](http.md) — request options, JSON output, redirects, and limits.
- [AXE Store](store.md) — verified on-demand packages, modes, cache, and package inventory.
- [Vzik](vzik.md) — bounded host and container evidence.
- [SSH server and relay](ssh-relay.md) — certificate login, NAT access, and systemd setup for the relay.

## Building and maintaining AXE

- [Architecture](architecture.md) — command execution, Store trust, and remote-access boundaries.
- [Custom editions](editions.md) — build-input bundle, OSS Store reuse, and deployment identities.
- [Runtime survivability](runtime-survivability.md) — self-exec and degradation when the executable or procfs is unavailable.
- [Bootstrapping](../BOOTSTRAP.md) — edition setup, identities, Store publishing, and relay credentials.
- [Local releases](release.md) — checking and publishing OSS release artifacts.
