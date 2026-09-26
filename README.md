# AXE

AXE packages a shell, `sshd`, core utilities, and on-demand tools from AXE Store into one executable. Its primary Linux build is a static musl ELF: the target machine does not need a system shell, coreutils, or a dynamic loader.

The same executable can start an interactive [Brush](https://github.com/reubeno/brush) shell, run a bundled applet, or fetch a signed package from AXE Store.

## Quick start

**There is no published AXE release yet.** [`nix/axe-releases.json`](nix/axe-releases.json) currently contains no release metadata, so there is no downloadable binary or Nix release package to install. Older pre-split stable artifacts cannot be reused because they lack verifiable edition identity. To try the OSS edition on Linux x86_64, build from this checkout:

```bash
nix develop .#default
just build
./dist/axe-x86_64-unknown-linux-musl --version
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl --list
env -i HOME=/tmp PATH=/nonexistent AXE_STORE_MODE=off \
  ./dist/axe-x86_64-unknown-linux-musl --no-config --norc --noprofile \
  -c 'commands >/dev/null && ps >/dev/null'
```

The version identifies this build as edition `oss`; `doctor --json` also exposes that identity at `.axe.edition`. `just build` prepares missing development keys and produces `dist/axe-x86_64-unknown-linux-musl`. `AXE_STORE_MODE=off` makes these first-run checks independent of Store network and cache state; the last command runs bundled commands without relying on the host's `PATH`. The binary can then be copied to another compatible host. To use on-demand Store commands, configure a signed Store snapshot or a previously verified cache and select an appropriate Store mode.

## What's inside

- Interactive Brush shell with Reedline, plus a separate backend for scripts and pipes.
- Rust-based GNU/POSIX-style core utilities, process tools, archives, compression, text processing, and filesystem utilities.
- Linux diagnostics for networking, storage, devices, modules, IPC, and inotify.
- `vzik`, a bounded JSONL collector for host and container evidence.
- Certificate-only SSH/SFTP server and authenticated NAT relay.
- AXE Store, with a signed Index, SHA-256 verification, and local cache.

The executable reports the exact inventory of its own build (use the path produced by your build). These offline examples disable Store; once a signed Store is configured, omit `AXE_STORE_MODE=off` to inspect on-demand commands too:

```bash
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl --list
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl commands
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl commands ps
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl doctor --json
```

`commands` returns a versioned JSON inventory. `availability` is `local`, `on_demand`, or `blocked`; `local_path` is the published applet path when one exists. For shell aliases, functions, and builtins, use the ordinary `type` and `command -v` builtins; AXE does not replace `command`.

## Command resolution

Within the shell, resolution follows this order:

```text
alias/function → shell builtin → bundled applet → AXE Store → PATH after transient/unavailable Store delivery failure
```

A bundled applet takes precedence over AXE Store and `PATH`. If Store delivery fails with a transient or unavailable classification, AXE may try a matching host executable from `PATH` (excluding its own applet bridge and executable). With no usable host candidate, that delivery failure exits with status 126. Signature, digest, schema, TLS, integrity, and configuration failures block execution with status 126 rather than falling back to `PATH`. A command not found through resolution exits with status 127.

For an asynchronous command, `$!` contains the child PID. If Brush runs a list inside the current process, there is no separate PID: AXE puts a `%N` job specification in `$!`, which `wait` accepts. Such an in-process job lives only as long as the current shell.

After building, an applet can be called by name through AXE, explicitly via `--applet`, or through a symlink:

```bash
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl jq -- -n '{ok: true}'
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl --applet jq -- -n '{ok: true}'
ln -s "$(pwd)/dist/axe-x86_64-unknown-linux-musl" /tmp/jq
AXE_STORE_MODE=off /tmp/jq -n '{ok: true}'
```

Inside Brush, no `axe` prefix is needed. AXE-managed shells and SSH exec sessions export the stable marker `AXE=true`, independently of the best-effort `PATH` bridge; version and runtime capabilities are available through `doctor --json`.

```bash
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl --no-config --norc --noprofile \
  -c 'jq -n "{ok: true}"; vzik capabilities'
```

### HTTP applet

By default, `http` emits one versioned `axe_http` JSON document containing the request method and URL, final status and URL, resolved `Location`, redirect history, headers, and a bounded response body. UTF-8 body and header values use `{"encoding":"utf8","data":"..."}`; other bytes use Base64. A completed bounded response, including HTTP `4xx` or `5xx`, exits with status `0`. Body-limit and redirect failures can occur after response headers and return a JSON error with status `1`. Passwords in userinfo are replaced by `[REDACTED]` only in structured URL fields. Raw headers and query parameters are retained verbatim: treat the entire evidence document as sensitive.

```bash
http https://example.org/
http -H 'Content-Type: application/json' -d '{"ready":true}' https://example.org/jobs
http --body --max-bytes 1048576 https://example.org/result
```

The default response-body limit is 16 MiB; change it with `--max-bytes`. On overflow, the JSON error retains status, URL, version, headers, and redirect history but omits the body; it reports `limit_bytes` and `received_at_least_bytes`. `--body` writes the raw body to stdout, which may contain a valid partial prefix on overflow.

Redirects are not followed unless `-L/--follow` is explicit, so the original response is preserved and requests do not silently leave their intended scope. Inspect `.response.resolved_location` one hop at a time; use `-L` only if the entire possible chain is authorized. Redirected requests do not forward `Authorization` or `Cookie`. The backend does not preserve `POST`, `PUT`, `PATCH`, or `DELETE` across `307/308`. `--data-file -` reads the request body from stdin, `--proxy` specifies an HTTP CONNECT proxy, and `--timeout` bounds the whole request. HTTPS trusts Mozilla roots and any additional CAs configured by the edition.

The HTTP/1.1 request backend supports `GET`, `HEAD`, `POST`, `PUT`, `DELETE`, `CONNECT`, `OPTIONS`, `TRACE`, and `PATCH`. It accepts HTTP/1.0 responses but does not select HTTP/1.0 for requests. Use Store `curl` for WebDAV/extension methods, HTTP/2, preserving method/body across `307/308`, or multipart; use `ncat` for version-specific or malformed requests, request smuggling, and raw protocol probes.

[`docs/runtime-survivability.md`](docs/runtime-survivability.md) describes self-exec backend ordering, the `PATH` bridge, behavior after unlinking, and controlled degradation.

Temporary `PATH` bridges and self-exec relays are not placed on arbitrary writable mounts. AXE tries `$XDG_CACHE_HOME/axe` (or the platform cache), then `$XDG_RUNTIME_DIR/axe` and `$TMPDIR/axe-<uid>`. If `AXE_WORK_DIR` is set, it is the required root with no automatic fallback. AXE Store has a separate storage-root order, described below.

## Installing with Nix

After the first edition-specific publication, the root flake will expose `packages.<system>.axe` and `default` from an immutable URL and SRI hash recorded in [`nix/axe-releases.json`](nix/axe-releases.json). An empty metadata file intentionally creates no release package and does not substitute an older pre-split stable artifact.

## Building from source

You need Nix with `nix-command` and flakes enabled. The development shell supplies Rust 1.98.0, `just`, Zig, `cargo-zigbuild`, a C toolchain, `ssh-keygen`, `yc`, and validation utilities:

```bash
nix develop .#default
just generate-dev-keys
cargo run -p axe -- --version
```

`generate-dev-keys` creates only missing local keys; it does not replace existing files in `keys/`.

This repository builds the OSS edition. Its identity appears in `axe --version` and `.axe.edition` from `doctor --json`. Other distributions use the same Rust workspace with a separate edition root; edition configuration, trust material, Store bootstrap, and release outputs must remain separate. Do not treat an OSS-built executable as another edition.

The default `just build` builds Linux x86_64:

```bash
just build
file dist/axe-x86_64-unknown-linux-musl
```

Other targets can be built separately:

```bash
just build-linux-arm64
just build-linux
just build-darwin-arm64
just build-darwin
just build-all
```

Artifacts are staged in `dist/`:

| Target | Artifact |
| --- | --- |
| `x86_64-linux` | `dist/axe-x86_64-unknown-linux-musl` |
| `aarch64-linux` | `dist/axe-aarch64-unknown-linux-musl` |
| `aarch64-darwin` | `dist/axe-aarch64-apple-darwin` |

Linux recipes check that the result is a static ELF of type `EXEC`, with neither `INTERP` nor `DT_NEEDED`; the x86_64 recipe also runs the artifact with an empty `PATH`.

Compare Linux x86_64 release sizes and startup times for `opt-level` values `z`, `s`, `2`, and `3` with:

```bash
just benchmark-opt-level
```

The Darwin recipe uses an SDK in ignored `target/toolchains/`.

### Development container

Without Nix and Rust installed on the host, open the checkout in VS Code with the **Dev Containers** extension and select **Dev Containers: Reopen in Container**. `.devcontainer/devcontainer.json` installs Nix 2.31.2, enters `nix develop .#default`, and creates missing development keys.

See [`BOOTSTRAP.md`](BOOTSTRAP.md) for keys, production configuration, AXE Store, and remote builders.

## Supported software

**Bundled** means code inside `axe`; **AXE Store** means a signed on-demand artifact from its Index. Store inventory is not a promise that downloads are currently published or reachable. For the active executable and its configured Store, consult `commands`.

### Bundled commands

| Group | Commands | Purpose |
| --- | --- | --- |
| Shell | Brush builtins | Shell state, jobs, history, completion, and POSIX/Bash-style control flow |
| Coreutils | `uutils/coreutils` | Basic file, text, process, and environment operations |
| Process | `free`, `hugetop`, `pgrep`, `pidof`, `pidwait`, `pkill`, `pmap`, `ps`, `pwdx`, `skill`, `slabtop`, `snice`, `sysctl`, `tload`, `top`, `vmstat`, `w`, `watch` | Linux processes and system state |
| System | `dmesg`, `hexdump`, `last`, `mountpoint` | Kernel log, hex dumps, login history, and mount points |
| Text | `awk`, `grep`, `sed`, `find`, `xargs`, `diff`, `cmp`, `diff3` | Search, transform, and compare data |
| Data | `jq` | JSON queries and transformations |
| Archives | `tar`, `gzip`, `gunzip`, `zcat`, `bzip2`, `bunzip2`, `bzcat`, `xz`, `unxz`, `xzcat` | Archives and compressed streams |
| Binary inspection | `file`, `goblin`, `strings` | Identify and inspect binary formats |
| Network | `http`, `arp`, `ifconfig`, `ip`, `ipaddr`, `iplink`, `ipneigh`, `iproute`, `iprule`, `ipcalc`, `host`, `nslookup`, `ping`, `ping6`, `traceroute`, `traceroute6` | Bounded HTTP requests, Linux networking, DNS, and connectivity |
| Storage | `blkid`, `blockdev`, `mount` | Block devices and mounts; bundled `mount` is read-only |
| Inspection | `iostat`, `ipcs`, `lsmod`, `lsof`, `lspci`, `lsscsi`, `lsusb`, `modinfo` | I/O, IPC, modules, open files, and devices |
| Filesystem | `inotifywait`, `inotifywatch`, `tree`, `which` | File events, directory trees, and executable lookup |
| AXE control | `commands`, `doctor`, `clean-tools`, `refresh-tools` | Command inventory, diagnostics, and AXE Store cache |
| Services | `sshd` | Certificate-only SSH/SFTP server |
| Evidence | `vzik` | Bounded host/container evidence in JSONL |

Linux-only commands are not registered in Darwin builds.

### AXE Store

This table comes from [`store/bootstrap.json`](store/bootstrap.json).

| Command | Purpose | Targets |
| --- | --- | --- |
| `binwalk` | Analyze firmware images and embedded files | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `bpftool` | Inspect and manage Linux eBPF objects | `aarch64-linux`, `x86_64-linux` |
| `capsh` | Inspect and change Linux capabilities | `aarch64-linux`, `x86_64-linux` |
| `curl` | Transfer data over HTTPS with Mozilla CAs | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `dbus-monitor` | Monitor D-Bus messages | `aarch64-linux`, `x86_64-linux` |
| `dnsx` | Resolve and enumerate DNS records | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `docker` | Manage Docker through a remote daemon | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `dumpcap` | Capture packets for Wireshark CLI tools | `aarch64-linux`, `x86_64-linux` |
| `fd` | Find filesystem entries by name and attributes | `aarch64-darwin`, `x86_64-linux` |
| `ffuf` | Fuzz web application paths and parameters | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `findmnt` | Locate and describe mounted filesystems | `aarch64-linux`, `x86_64-linux` |
| `fzf` | Interactively filter and select values | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `gdb` | Debug native programs and processes | `aarch64-linux`, `x86_64-linux` |
| `gdbserver` | Expose native programs to remote GDB | `aarch64-linux`, `x86_64-linux` |
| `getcap` | Display Linux file capabilities | `aarch64-linux`, `x86_64-linux` |
| `getpcaps` | Display Linux process capabilities | `aarch64-linux`, `x86_64-linux` |
| `gobuster` | Enumerate web paths, DNS names, and virtual hosts | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `grpcurl` | Call and inspect gRPC services | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `httpx` | Probe HTTP services and discover live targets | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `jq` | Process JSON | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `kubectl` | Manage Kubernetes clusters | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `lsns` | Show Linux namespaces and processes | `aarch64-linux`, `x86_64-linux` |
| `ltrace` | Trace Linux library calls | `aarch64-linux`, `x86_64-linux` |
| `masscan` | Scan large networks quickly | `aarch64-linux`, `x86_64-linux` |
| `naabu` | Find open ports | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ncat`, `nc` | Connect, listen, and proxy network traffic | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `nmap` | Discover hosts and probe services with NSE | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `nsenter` | Run a program in another process's namespaces | `aarch64-linux`, `x86_64-linux` |
| `nuclei` | Scan targets with bundled vulnerability templates | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `openssl` | Inspect certificates and perform cryptographic operations | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `podman` | Manage Podman through a remote service | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `pspy` | Observe Linux processes without root | `aarch64-linux`, `x86_64-linux` |
| `python`, `python3` | Static Python with HTTP, WebSocket, and HTML libraries | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `rg` | Search file contents with regular expressions | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `rsync` | Synchronize local and remote paths | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `scp` | Copy files over OpenSSH | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `setcap` | Set Linux file capabilities | `aarch64-linux`, `x86_64-linux` |
| `sftp` | Transfer files over OpenSSH SFTP | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `socat` | Transfer data between sockets and streams | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `sqlite3` | Read and modify SQLite databases | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ss` | Show sockets and network connections | `aarch64-linux`, `x86_64-linux` |
| `ssh` | Connect to hosts over OpenSSH | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ssh-add` | Add keys to an OpenSSH agent | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ssh-agent` | Hold OpenSSH private keys for a session | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ssh-keygen` | Create and modify OpenSSH keys | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ssh-keyscan` | Collect OpenSSH host keys | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `strace` | Trace Linux system calls and signals | `aarch64-linux`, `x86_64-linux` |
| `subfinder` | Enumerate subdomains | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `tcpdump` | Capture and display network packets | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `tshark` | Analyze packet captures from the CLI | `aarch64-linux`, `x86_64-linux` |
| `unshare` | Run a program in new Linux namespaces | `aarch64-linux`, `x86_64-linux` |
| `xh` | Interactive-friendly HTTP client | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `yq` | Process YAML, JSON, and XML | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |

The `nuclei` package includes pinned `nuclei-templates`; no separate template download is needed on first run.

## How AXE Store works

Nix builds packages; `axe-store` signs metadata and publishes content-addressed objects. Before execution, the client verifies signatures, manifests, size, and SHA-256. A verified cache works offline. On Linux, a single executable can launch from a sealed `memfd` when no suitable filesystem backend is available.

`AXE_STORE_DIR` selects a preferred storage root. Otherwise AXE tries roots from `config/store.json`, the platform cache, writable persistent mounts, tmpfs, and the platform temporary directory. Metadata is namespaced by the normalized Store URL, trusted key IDs, and channel; different trust identities do not reuse each other's metadata, even when sharing a fallback root. Content-addressed objects are identified by SHA-256. For operational separation of editions, configure separate `AXE_STORE_DIR` roots when possible.

Set the mode with `AXE_STORE_MODE` or `sshd --store-mode`:

- `auto`: use the verified cache and refresh from the network according to TTL; transient or unavailable delivery failures permit `PATH` fallback.
- `cache-only`: use only the verified cache; an unavailable cache entry can permit `PATH` fallback.
- `off`: do not initialize Store or register its commands.

`sshd` propagates the effective mode to shell and exec sessions; child sessions cannot relax an inherited restriction. `clean-tools` removes metadata for the current Store identity, while `refresh-tools` forces an Index refresh. See [`docs/architecture.md`](docs/architecture.md) for storage, verification, and network fallback.

### Adding a package to AXE Store

Category modules under [`store/nix/packages/`](store/nix/packages/) are the source of truth. Package IDs have the form `<category>/<name>`, and each attribute must be unique across the package set.

For one executable from nixpkgs, use `mkNixpkgsBinary` (the package name below is illustrative and must be replaced with a real nixpkgs attribute):

```nix
{
  mkNixpkgsBinary,
  portableSystems,
  packageSetFor,
  ...
}: {
  example = mkNixpkgsBinary {
    name = "example";
    synopsis = "Inspect example data";
    systems = portableSystems;
    packageFor = system: pkgs: (packageSetFor system pkgs).example;
  };
}
```

For a pinned upstream binary, use `mkUpstreamBinary`; for multiple targets, use `mkUpstreamBinaries`. Every real source needs an immutable URL and Nix hash. This is illustrative, **not** a downloadable artifact:

```nix
example = mkUpstreamBinaries {
  name = "example";
  version = "1.2.3";
  synopsis = "Inspect example data";
  sources = {
    x86_64-linux = {
      url = "https://example.invalid/example-1.2.3-linux-amd64";
      hash = "sha256-...";
    };
  };
};
```

When a program needs a file tree, use `mkNixpkgsPackage` and specify `entrypoint`. The built output must not refer to `/nix/store`; Linux executables undergo additional static validation.

After changing the package set, regenerate the bootstrap metadata and build Store packages:

```bash
just store-bootstrap
just check-store-bootstrap
nix flake check --no-build .
just store-build
```

Keep the AXE Store table above in sync with generated `store/bootstrap.json`. See [`BOOTSTRAP.md`](BOOTSTRAP.md) for signing keys, publication credentials, and builders. Building packages does not publish a Store snapshot or AXE release.

## Vzik

Run the bounded host/container evidence collector explicitly. From inside the built AXE shell, for example (each probe's availability depends on the host):

```bash
vzik collect
vzik network listeners --max-items 4096
vzik security posture
vzik container inspect self
vzik systemctl list --all-users
vzik dbus inspect org.freedesktop.systemd1
vzik portoctl inspect self --socket /run/portod.socket
vzik filesystem privilege-surfaces . --max-items 4096 --max-entries 50000 --max-depth 16
vzik capabilities
vzik capabilities porto.list
vzik capture collect --output baseline.jsonl --receipt baseline.receipt.json --stderr baseline.stderr.jsonl
```

Some probes require host access or may report unavailable on systems without the corresponding service (including Porto). `collect` uses the `baseline-v3` profile: all INET sockets, listening Unix sockets only, and runtime systemd units excluding automatically created `.device` units. Use `vzik network sockets` and `vzik systemctl list` for full Unix IPC and runtime-unit inventories.

The collector writes bounded protocol-v3 JSONL. `stream_start` declares the complete `planned_capabilities`. A terminal `stream_end` has outcome `complete` only if every planned capability completed; otherwise the outcome is `degraded` and process status is `3`. `not_started_capabilities` lists probes skipped because of a stream limit. Status `0` means complete; `2` invalid request; `4` internal error; `5` write error; `124` deadline; and `128+signal` signal interruption. Deadline and `SIGINT`/`SIGTERM` are checked cooperatively between bounded operations. Standalone `vzik` emits a terminal `stream_abort` on interruption; a stream without `stream_end` is always incomplete. Process-level errors go to stderr as JSON with `code`, `operation`, `retryable`, `message`, and `details`.

`vzik capabilities` returns a compact machine-readable index of protocol semantics, global limits, and capability IDs. `vzik capabilities CAPABILITY_ID` returns one capability's detailed request schema, data kinds, access class, and possible outcomes. Both views come from the typed definitions used by the Clap CLI. The collector does not execute host binaries, open INET connections, or write to the target filesystem. Where available, systemd and D-Bus state is read directly over bounded Unix-socket connections.

`vzik capture` writes a capture, saved stderr, and receipt to specified new files. It refuses to overwrite existing paths and publishes the receipt only after validating the complete capture. This is not a multi-file transaction: an interruption or write failure can leave capture or stderr without a receipt. A sealed degraded capture remains a valid artifact, but the command returns status `3`.

## SSH server and relay

`sshd` accepts only OpenSSH user certificates issued by a CA listed in `keys/ssh/user_ca_keys`. The username must be allowlisted and match the certificate principal; plain public keys and certificates with critical options are rejected. Configure keys and the allowlist before starting a server (see [`BOOTSTRAP.md`](BOOTSTRAP.md)).

```bash
./dist/axe-x86_64-unknown-linux-musl --applet sshd -- --listen '[::]:6969' --workdir .
```

The first successful interactive PTY session on each SSH transport receives a compact welcome line pointing to `skill://axe`, `doctor --json`, and `vzik capabilities`. Subsequent shell channels on the same multiplexed transport, remote exec, SFTP, and forwarding do not receive welcome output.

For an incident where AXE has not yet started, the [rescue agent skill](.agents/skills/axe-rescue/SKILL.md) helps an agent establish available transfer and execution routes and prepare the smallest operator-run activation step. It does not install or launch AXE remotely on the agent's behalf; after activation, use `skill://axe` for diagnosis. An external edition checkout must expose the skill through its own agent skills directory or configured provider.

To exercise its local first-entry cases without touching a remote machine, run `nix develop .#default --command bash .agents/skills/axe-rescue/scripts/live-smoke.sh "$PWD/dist/axe-x86_64-unknown-linux-musl"` after building a **development** binary. The smoke test creates disposable loopback OpenSSH servers and a network-isolated distroless Podman container; `AXE_RESCUE_LIVE_AGENT=1` additionally asks the configured DeepSeek model to interpret the observed results with read-only skill access. Do not supply a production edition binary with embedded credentials to the test containers. This checks runtime mechanics, not release provenance or other editions.

When a target cannot be reached from outside, `sshd` can establish an outbound registration with a relay. The OSS edition has relay disabled by default (`config/relay.json` sets `enabled_by_default` to `false` and has no configured endpoints): without `--relay`, no relay task starts. `--relay ENDPOINT` enables the selected transport; `--no-relay` disables it even in an edition with a configured default. These flags conflict.

The relay supports both TCP and UDP transports:

| Transport | Control | Authentication | Data plane | When to choose |
| --- | --- | --- | --- | --- |
| TCP+yamux | TCP `6999` | Shared token | One yamux stream per connection | Default; lower CPU use |
| QUIC | UDP `11000` | Separate mTLS identities | Direct bidirectional QUIC stream | Packet loss or NAT rebinding |

TCP requires an `AXE_RELAY_TOKEN` of at least 32 bytes. QUIC client credentials can be embedded by an edition or provided through `AXE_RELAY_QUIC_SERVER_CERT_FILE`, `AXE_RELAY_QUIC_CLIENT_CERT_FILE`, and `AXE_RELAY_QUIC_CLIENT_KEY_FILE`. The standalone `axe-relay` server requires the server private key and both certificates as runtime files; they are not embedded in `axe-relay`.

Select the `sshd` transport explicitly with `--relay-transport quic` for QUIC; TCP is the default. `config/relay.json` controls `enabled_by_default` and optional endpoints. If a default is enabled without an endpoint for the selected transport, configuration fails before bind/readiness. On registration the relay assigns a public TCP port and logs registration/disconnection with transport, relay ID, and active client count.

Without `--relay-id`, `sshd` registers as `<pidns>@<user>@<hostname>` built from OS identity: `<pidns>` is the inode of its PID namespace (`/proc/self/ns/pid`), the username comes from the OS account database (numeric UID if unavailable), and the hostname from the OS (`unknown` if unavailable). Processes in one PID namespace — typically one container — share the ID, and it survives `sshd` restarts including supervised worker restarts. The namespace inode is not globally unique and degrades to `-1` without a visible procfs, which can collide across such hosts; for unique targeting, the operator must assign each client of the relay a distinct `--relay-id`.

The standalone `axe-relay` serves TCP control on `6999`, QUIC control on `11000`, and assigned public TCP ports in `3000–4000` by default. `--public-bind` selects the local listener interface; `--public-host` sets the SSH-reachable address returned to clients. The dashboard/API listens on loopback (`127.0.0.1:7000`); remote access requires an authenticated HTTPS proxy.

Targets behind NAT connect outbound with `axe sshd`. `axe-relay watch [--client-id ID]` follows arrivals and departures; `axe-relay wait --client-id ID` returns one assigned SSH `HOST:PORT`. The read-only dashboard (`/`), JSON status API (`/api/v1/status`), and `status`/`clients` commands show active registrations. See the [`BOOTSTRAP.md`](BOOTSTRAP.md#relay-endpoints-and-identities) for deployment.

