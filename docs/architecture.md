# Architecture

AXE is one executable with a Brush shell, bundled applets, and optional on-demand commands. `crates/axe/src/main.rs` initializes the executable capability and parses the direct entry route once: hidden dispatch, a PATH bridge or BusyBox-style symlink name, `--applet`, or a positional command. That route selects the Store mode before the command registry is built. The registry is then installed into Brush, and the same route either dispatches an applet from it or falls through to the shell.

## Commands and execution

`crates/axe/src/registry.rs` builds the bundled command registry and adds Store commands without overriding bundled names. Brush holds the only installed copy; direct entry, shell shims, and nested AXE children resolve names through it. In the shell, aliases/functions and builtins precede bundled applets; Store precedes the host `PATH` only under the failure rules in [Commands](commands.md). Brush runs interactive TTY sessions through Reedline and non-terminal execution through its minimal backend.

Most bundled commands launched by the shell start another AXE process. `crates/axe/src/executable.rs` retains the means to launch that process, and `path_bridge.rs` publishes a best-effort command path. The in-process registry still works if the bridge cannot be published. See [Runtime survivability](runtime-survivability.md) for descriptor execution, checked paths, and failure cases.

## On-demand software

`crates/axe-store-client` handles Store consumption; `crates/axe-store` builds and publishes packages. Package definitions are in `store/nix/packages/`, with generated inventory in `store/bootstrap.json`. The executable embeds public trust and a signed bootstrap Index, not publisher credentials. Before execution, Store metadata and payloads are verified; see [AXE Store](store.md) for modes and cache behavior. [BOOTSTRAP](../BOOTSTRAP.md) covers building and publishing a Store.

## Remote access and evidence

The bundled `sshd` serves shell, exec, and SFTP sessions with certificate-only user authentication. A target behind NAT can make an outbound connection to the separate `axe-relay` service in `crates/axe-relay`. [SSH and relay](ssh-relay.md) covers runtime use; [BOOTSTRAP](../BOOTSTRAP.md#relay-endpoints-and-identities) covers deployment. The bundled `vzik` collector (`crates/vzik`) writes bounded host/container evidence; see [Vzik](vzik.md).
