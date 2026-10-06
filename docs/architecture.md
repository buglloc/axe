# Architecture

AXE combines a Brush shell, bundled commands, and optional AXE Store commands in one executable. Custom editions select defaults, identities, and trust material at build time.

## Entry and command resolution

Run `axe` as a shell or invoke a command directly. Inside the shell, aliases and functions resolve first, followed by builtins, bundled commands, and Store commands. A host executable on `PATH` can substitute after a transient Store failure; trust, integrity, and configuration failures block execution.

AXE also exposes bundled command names through a managed PATH directory when it can create one. Direct invocation remains available without that directory. See [Commands](commands.md) for syntax and exit statuses.

## Process boundary

Shell builtins, aliases, functions, and `doctor` remain available if AXE can no longer start child processes. Most bundled commands, daemon workers, and SSH shell or exec sessions require child execution.

Linux AXE can keep launching children after its executable is deleted or replaced. Filesystem and execution restrictions can still prevent this. See [Runtime survivability](runtime-survivability.md) for platform limits and diagnosis.

## Store trust boundary

`axe-store` builds and publishes signed packages; `axe` consumes them using public trust material. AXE verifies metadata and payloads before execution, including cached packages. Store mode controls network access and offline use. Publisher signing credentials stay outside the executable. See [AXE Store](store.md).

## Remote access and evidence

The bundled `sshd` serves shell, exec, and SFTP sessions using user certificates from an approved CA. For targets without inbound access, it can register with the separate `axe-relay` service. See [SSH and relay](ssh-relay.md).

The bundled `vzik` collects bounded host and container evidence and reports unavailable probes. Capture mode validates a collection before publishing a receipt. See [Vzik](vzik.md).
