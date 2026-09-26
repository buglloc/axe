# AXE

AXE puts a shell, `sshd`, core utilities, and on-demand AXE Store tools in one executable. The primary Linux build is a static musl ELF; it needs no system shell, coreutils, or dynamic loader on the target machine.

Run that executable as an interactive [Brush](https://github.com/reubeno/brush) shell, a bundled applet, or a client that fetches signed AXE Store packages.

## Quick start

To build the OSS edition from this checkout on Linux x86_64:

```bash
nix develop .#default
just build
./dist/axe-x86_64-unknown-linux-musl --version
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl --list
env -i HOME=/tmp PATH=/nonexistent AXE_STORE_MODE=off \
  ./dist/axe-x86_64-unknown-linux-musl --no-config --norc --noprofile \
  -c 'commands >/dev/null && ps >/dev/null'
```

`just build` prepares missing development keys and writes `dist/axe-x86_64-unknown-linux-musl`. The version reports edition `oss`, also available at `.axe.edition` in `doctor --json`. `AXE_STORE_MODE=off` keeps these first-run checks independent of Store network and cache state. The last command runs bundled commands without the host's `PATH`. You can copy the binary to another compatible host. To run on-demand Store commands, configure a signed Store snapshot or a previously verified cache and choose a Store mode.

## Downloads

No tagged OSS release has been verified for this checkout yet. Published binaries and SHA-256 hashes will appear here after verification. [Release history](https://github.com/buglloc/axe/releases).

## What's inside

- Interactive Brush shell with Reedline, plus a separate backend for scripts and pipes.
- Rust-based GNU/POSIX-style core utilities, process tools, archives, compression, text processing, and filesystem utilities.
- Linux diagnostics for networking, storage, devices, modules, IPC, and inotify.
- `vzik`, a bounded JSONL collector for host and container evidence.
- Certificate-only SSH/SFTP server and authenticated NAT relay.
- AXE Store, with a signed Index, SHA-256 verification, and local cache.

The executable reports its own exact command inventory (use the path from your build). These examples disable Store so they work offline; after configuring a signed Store, omit `AXE_STORE_MODE=off` to include on-demand commands:

```bash
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl --list
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl commands
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl commands ps
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl doctor --json
```

`commands` returns a versioned JSON inventory. `availability` is `local`, `on_demand`, or `blocked`; `local_path` is the published applet path, if one exists. To inspect shell aliases, functions, and builtins, use `type` and `command -v`. AXE does not replace `command`.

## Command resolution

Within the shell, resolution follows this order:

```text
alias/function → shell builtin → bundled applet → AXE Store → PATH after transient/unavailable Store delivery failure
```

Bundled applets take precedence over AXE Store and `PATH`. If Store delivery fails as transient or unavailable, AXE may try a matching host executable from `PATH`, excluding its own applet bridge and executable. If no usable host executable exists, the delivery failure exits with status 126. Signature, digest, schema, TLS, integrity, and configuration failures block execution with status 126; AXE does not fall back to `PATH`. An unresolved command exits with status 127.

For an asynchronous command, `$!` holds the child PID. When Brush runs a list in the current process, there is no separate PID. Instead, AXE puts a `%N` job specification in `$!`; `wait` accepts it. The in-process job ends with the current shell.

After building, an applet can be called by name through AXE, explicitly via `--applet`, or through a symlink:

```bash
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl jq -- -n '{ok: true}'
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl --applet jq -- -n '{ok: true}'
ln -s "$(pwd)/dist/axe-x86_64-unknown-linux-musl" /tmp/jq
AXE_STORE_MODE=off /tmp/jq -n '{ok: true}'
```

Inside Brush, no `axe` prefix is needed. AXE-managed shells and SSH exec sessions export `AXE=true` even when the best-effort `PATH` bridge is unavailable. Use `doctor --json` for version and runtime capabilities.

```bash
AXE_STORE_MODE=off ./dist/axe-x86_64-unknown-linux-musl --no-config --norc --noprofile \
  -c 'jq -n "{ok: true}"; vzik capabilities'
```

### HTTP applet

By default, `http` emits one versioned `axe_http` JSON document with the request method and URL, final status and URL, resolved `Location`, redirect history, headers, and a bounded response body. UTF-8 body and header values use `{"encoding":"utf8","data":"..."}`; other bytes use Base64. A completed bounded response exits with status `0`, even for HTTP `4xx` or `5xx`. Body-limit and redirect failures can occur after response headers; they return a JSON error with status `1`. Passwords in userinfo are replaced by `[REDACTED]` only in structured URL fields. Raw headers and query parameters remain verbatim: treat the entire evidence document as sensitive.

```bash
http https://example.org/
http -H 'Content-Type: application/json' -d '{"ready":true}' https://example.org/jobs
http --body --max-bytes 1048576 https://example.org/result
```

The response-body limit defaults to 16 MiB; change it with `--max-bytes`. On overflow, the JSON error retains status, URL, version, headers, and redirect history but omits the body and reports `limit_bytes` and `received_at_least_bytes`. `--body` writes the raw body to stdout, which may contain a valid partial prefix on overflow.

Redirects are not followed unless you specify `-L/--follow`. This preserves the original response and prevents requests from silently leaving their intended scope. Inspect `.response.resolved_location` one hop at a time; use `-L` only if the entire possible chain is authorized. Redirected requests do not forward `Authorization` or `Cookie`. The backend does not preserve `POST`, `PUT`, `PATCH`, or `DELETE` across `307/308`. `--data-file -` reads the request body from stdin, `--proxy` specifies an HTTP CONNECT proxy, and `--timeout` bounds the whole request. HTTPS trusts Mozilla roots and any additional CAs configured by the edition.

The HTTP/1.1 request backend supports `GET`, `HEAD`, `POST`, `PUT`, `DELETE`, `CONNECT`, `OPTIONS`, `TRACE`, and `PATCH`. It accepts HTTP/1.0 responses but does not select HTTP/1.0 for requests. Use Store `curl` for WebDAV/extension methods, HTTP/2, preserving method/body across `307/308`, or multipart; use `ncat` for version-specific or malformed requests, request smuggling, and raw protocol probes.

[`docs/runtime-survivability.md`](docs/runtime-survivability.md) describes self-exec backend ordering, the `PATH` bridge, behavior after unlinking, and controlled degradation.

AXE does not place temporary `PATH` bridges or self-exec relays on arbitrary writable mounts. It tries `$XDG_CACHE_HOME/axe` (or the platform cache), then `$XDG_RUNTIME_DIR/axe` and `$TMPDIR/axe-<uid>`. If `AXE_WORK_DIR` is set, AXE requires that root and does not fall back automatically. AXE Store uses a separate storage-root order, described below.

## Installing with Nix

Once a release is available, the root flake exposes `packages.<system>.axe` and `default` for targets listed in [`nix/axe-releases.json`](nix/axe-releases.json). It downloads the immutable binary URL and verifies the recorded SRI hash. The [Downloads](#downloads) table shows human-readable SHA-256 values.

## Building from source

You need Nix with `nix-command` and flakes enabled. The development shell supplies Rust 1.98.0, `just`, Zig, `cargo-zigbuild`, a C toolchain, `ssh-keygen`, `yc`, and validation utilities:

```bash
nix develop .#default
just generate-dev-keys
cargo run -p axe -- --version
```

`generate-dev-keys` creates only missing local keys; it does not replace existing files in `keys/`.

This repository builds the OSS edition. Its identity appears in `axe --version` and `.axe.edition` from `doctor --json`. Other distributions use the same Rust workspace but have separate edition roots; edition configuration, trust material, Store bootstrap, and release outputs must stay separate. An OSS-built executable is not another edition.

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

Linux recipes verify that the result is a static ELF of type `EXEC` with neither `INTERP` nor `DT_NEEDED`. The x86_64 recipe also runs the artifact with an empty `PATH`.

The Darwin recipe uses an SDK in ignored `target/toolchains/`.

See [`BOOTSTRAP.md`](BOOTSTRAP.md) for keys, production configuration, AXE Store, and remote builders.

## Supported software

**Bundled** means code inside `axe`; **AXE Store** means a signed on-demand artifact from its Index. Store inventory does not guarantee that downloads are published or reachable. Run `commands` to inspect the active executable and its configured Store.

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

Nix builds packages; `axe-store` signs metadata and publishes content-addressed objects. Before running a package, the client verifies signatures, manifests, size, and SHA-256. A verified cache works offline. On Linux, AXE can launch a single executable from a sealed `memfd` if no suitable filesystem backend is available.

`AXE_STORE_DIR` selects a preferred storage root. Otherwise AXE tries roots from `config/store.json`, the platform cache, writable persistent mounts, tmpfs, and the platform temporary directory. Metadata is namespaced by normalized Store URL, trusted key IDs, and channel. Different trust identities do not reuse each other's metadata, even on a shared fallback root. Content-addressed objects are identified by SHA-256. Where possible, configure separate `AXE_STORE_DIR` roots to keep editions operationally separate.

Set the mode with `AXE_STORE_MODE` or `sshd --store-mode`:

- `auto`: use the verified cache and refresh from the network according to TTL; transient or unavailable delivery failures permit `PATH` fallback.
- `cache-only`: use only the verified cache; an unavailable cache entry can permit `PATH` fallback.
- `off`: do not initialize Store or register its commands.

`sshd` passes the effective mode to shell and exec sessions; child sessions cannot relax an inherited restriction. `clean-tools` removes metadata for the current Store identity; `refresh-tools` forces an Index refresh. See [`docs/architecture.md`](docs/architecture.md) for storage, verification, and network fallback.

### Adding a package to AXE Store

Category modules under [`store/nix/packages/`](store/nix/packages/) define the package set. Package IDs have the form `<category>/<name>`; each attribute must be unique across that set.

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

For a pinned upstream binary, use `mkUpstreamBinary`; for multiple targets, use `mkUpstreamBinaries`. Every real source needs an immutable URL and Nix hash. The example below is **not** a downloadable artifact:

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

For programs that need a file tree, use `mkNixpkgsPackage` and specify `entrypoint`. The built output must not refer to `/nix/store`; Linux executables undergo additional static validation.

After changing the package set, regenerate the bootstrap metadata and build Store packages:

```bash
just store-bootstrap
just check-store-bootstrap
nix flake check --no-build .
just store-build
```

Keep the AXE Store table above in sync with generated `store/bootstrap.json`. See [`BOOTSTRAP.md`](BOOTSTRAP.md) for signing keys, publication credentials, and builders. Building packages does not publish a Store snapshot or AXE release.

## Vzik

Run the bounded host/container evidence collector explicitly. For example, inside the built AXE shell (probe availability depends on the host):

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

Some probes require host access; on systems without the relevant service (including Porto), they may report unavailable. `collect` uses the `baseline-v3` profile: all INET sockets, listening Unix sockets only, and runtime systemd units excluding automatically created `.device` units. Use `vzik network sockets` and `vzik systemctl list` for full Unix IPC and runtime-unit inventories.

The collector writes bounded protocol-v3 JSONL. `stream_start` declares the complete `planned_capabilities`. A terminal `stream_end` has outcome `complete` only if every planned capability completed; otherwise it reports `degraded` and the process exits with status `3`. `not_started_capabilities` lists probes skipped because of a stream limit. Status `0` means complete; `2` invalid request; `4` internal error; `5` write error; `124` deadline; and `128+signal` signal interruption. Deadline and `SIGINT`/`SIGTERM` are checked cooperatively between bounded operations. Standalone `vzik` emits a terminal `stream_abort` on interruption; a stream without `stream_end` is always incomplete. Process-level errors go to stderr as JSON with `code`, `operation`, `retryable`, `message`, and `details`.

`vzik capabilities` returns a compact machine-readable index of protocol semantics, global limits, and capability IDs. `vzik capabilities CAPABILITY_ID` returns the detailed request schema, data kinds, access class, and possible outcomes for one capability. Both views come from the typed definitions used by the Clap CLI. The collector does not execute host binaries, open INET connections, or write to the target filesystem. Where available, it reads systemd and D-Bus state directly over bounded Unix-socket connections.

`vzik capture` writes a capture, saved stderr, and receipt to specified new files. It refuses to overwrite existing paths and publishes the receipt only after validating the complete capture. The files are not written as a transaction: an interruption or write failure can leave capture or stderr without a receipt. A sealed degraded capture remains valid, but the command returns status `3`.

## SSH server and relay

`sshd` accepts only OpenSSH user certificates issued by a CA listed in `keys/ssh/user_ca_keys`. The username must be allowlisted and match the certificate principal; plain public keys and certificates with critical options are rejected. Set up keys and the allowlist before starting the server (see [`BOOTSTRAP.md`](BOOTSTRAP.md)).

```bash
./dist/axe-x86_64-unknown-linux-musl --applet sshd -- --listen '[::]:6969' --workdir .
```

The first successful interactive PTY session on each SSH transport receives a short welcome line pointing to `skill://axe`, `doctor --json`, and `vzik capabilities`. Later shell channels on the same multiplexed transport, remote exec, SFTP, and forwarding receive no welcome output.

If AXE has not started on a target, the [rescue agent skill](.agents/skills/axe-rescue/SKILL.md) helps an agent find available transfer and execution routes and prepare the smallest operator-run activation step. It does not install or launch AXE remotely on the agent's behalf. After activation, use `skill://axe` for diagnosis. An external edition checkout must expose the skill through its own agent skills directory or configured provider.

After building a **development** binary, run `nix develop .#default --command bash .agents/skills/axe-rescue/scripts/live-smoke.sh "$PWD/dist/axe-x86_64-unknown-linux-musl"` to exercise local first-entry cases without contacting a remote machine. The smoke test creates disposable loopback OpenSSH servers and a network-isolated distroless Podman container. With `AXE_RESCUE_LIVE_AGENT=1`, it also asks the configured DeepSeek model to interpret the results with read-only skill access. Do not supply a production edition binary with embedded credentials to the test containers. This checks runtime mechanics, not release provenance or other editions.

If a target cannot be reached from outside, `sshd` can register outbound with a relay. The OSS edition disables relay by default (`config/relay.json` sets `enabled_by_default` to `false` and configures no endpoints); without `--relay`, no relay task starts. `--relay ENDPOINT` enables the selected transport; `--no-relay` disables it even for editions with a configured default. The flags conflict.

The relay supports both TCP and UDP transports:

| Transport | Control | Authentication | Data plane | When to choose |
| --- | --- | --- | --- | --- |
| TCP+yamux | TCP `6999` | Shared token | One yamux stream per connection | Default; lower CPU use |
| QUIC | UDP `11000` | Separate mTLS identities | Direct bidirectional QUIC stream | Packet loss or NAT rebinding |

TCP requires an `AXE_RELAY_TOKEN` of at least 32 bytes. QUIC client credentials can be embedded by an edition or provided through `AXE_RELAY_QUIC_SERVER_CERT_FILE`, `AXE_RELAY_QUIC_CLIENT_CERT_FILE`, and `AXE_RELAY_QUIC_CLIENT_KEY_FILE`. The standalone `axe-relay` server requires the server private key and both certificates as runtime files; they are not embedded in `axe-relay`.

Use `--relay-transport quic` to select QUIC for `sshd`; TCP is the default. `config/relay.json` controls `enabled_by_default` and optional endpoints. Enabling a default without an endpoint for the selected transport causes configuration to fail before bind/readiness. On registration, the relay assigns a public TCP port and logs registration/disconnection with transport, relay ID, and active client count.

Without `--relay-id`, `sshd` registers as `<pidns>@<user>@<hostname>` using OS identity. `<pidns>` is the inode of its PID namespace (`/proc/self/ns/pid`); the username comes from the OS account database (numeric UID if unavailable), and the hostname from the OS (`unknown` if unavailable). Processes in one PID namespace—typically one container—share the ID, which survives `sshd` restarts, including supervised worker restarts. The namespace inode is not globally unique and becomes `-1` without visible procfs; IDs can then collide across hosts. To target clients uniquely, assign each client of the relay a distinct `--relay-id`.

By default, the standalone `axe-relay` serves TCP control on `6999`, QUIC control on `11000`, and assigned public TCP ports in `3000–4000`. `--public-bind` selects the local listener interface; `--public-host` sets the SSH-reachable address returned to clients. The dashboard/API listens on loopback (`127.0.0.1:7000`); remote access requires an authenticated HTTPS proxy.

Targets behind NAT connect outbound with `axe sshd`. `axe-relay watch [--client-id ID]` follows arrivals and departures; `axe-relay wait --client-id ID` returns one assigned SSH `HOST:PORT`. The read-only dashboard (`/`), JSON status API (`/api/v1/status`), and `status`/`clients` commands show active registrations. See [`BOOTSTRAP.md`](BOOTSTRAP.md#relay-endpoints-and-identities) for deployment.

