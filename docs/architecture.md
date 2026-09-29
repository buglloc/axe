# Architecture

AXE combines a Brush shell, bundled commands, and optional AXE Store commands in the `axe` executable. The shell and direct command entry share a command inventory, but they do not always use the same execution path: direct entry dispatches bundled commands in the current process, while most bundled commands invoked from the shell start another `axe` process.

## Entry and command resolution

An invocation can enter a command through `axe --applet`, a positional command name, or an applet name used to launch the executable (including an AXE-managed PATH bridge). Otherwise, AXE starts Brush. It selects the Store mode and assembles the command inventory before dispatching the command or entering the shell. Bundled names take precedence over Store names when that inventory is assembled.

Inside Brush, aliases and functions resolve first, then shell builtins, bundled commands, and Store commands. A matching host executable on `PATH` can substitute after a transient or unavailable Store delivery failure; a trust, integrity, or configuration failure blocks execution instead. The managed PATH bridge makes applet names usable by path-based launchers when AXE can publish it, but the in-process inventory does not depend on that bridge. [Commands](commands.md) describes resolution and the active command inventory.

## Process boundary

Direct applet entry, shell aliases, functions, and builtins run in-process. The `doctor` shell command also reads live shell state in-process. Most other bundled commands invoked from Brush launch a child copy of `axe`; daemon workers and SSH shell or exec sessions also need to launch child processes. If starting another AXE process becomes impossible, the current shell can still use its in-process commands, but bundled child commands cannot silently turn into host commands.

On Linux, AXE retains access to its executable image so child launches can use a descriptor even after its original pathname is removed. Checked filesystem paths and a private executable copy provide fallbacks where a path is required or descriptor execution is blocked. Publishing a PATH bridge is best-effort and does not determine whether descriptor-aware child launches work. [Runtime survivability](runtime-survivability.md) explains the platform differences and failure limits.

## Store trust boundary

`axe-store` builds and publishes on-demand packages; `axe` consumes them. The publisher signs metadata and distributes content-addressed objects. The consumer carries public trust material and a signed bootstrap Index, not the publisher's signing credentials. Before running a Store payload, it verifies signed metadata, manifest information, size, and SHA-256. Cached content remains subject to verification and can be used offline. Store mode controls whether AXE can refresh over the network, use only the verified cache, or omit Store commands. [AXE Store](store.md) covers cache identity and mode behavior.

## Remote access and evidence

The bundled `sshd` serves shell, exec, and SFTP sessions using user certificates from an approved CA; it does not accept ordinary public-key login. For targets that cannot accept inbound connections, `sshd` can register outbound with the separate `axe-relay` service. Relay registration is optional, and SSH shell and exec sessions still cross the child-process boundary described above. The network relay is distinct from the private executable copy used for self-exec. See [SSH and relay](ssh-relay.md).

The bundled `vzik` command collects bounded host and container evidence. Inaccessible probes report unavailable rather than implying a complete view. Its JSONL output marks a complete collection with `stream_end`; capture mode validates the stream before publishing a receipt. See [Vzik](vzik.md) for the evidence format and collection commands.
