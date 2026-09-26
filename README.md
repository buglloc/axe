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

Release [v0.1.0](https://github.com/buglloc/axe/releases/tag/v0.1.0). Verify the binary's SHA-256 before installation; [SHA256SUMS](https://github.com/buglloc/axe/releases/download/v0.1.0/SHA256SUMS) lists every asset.

| Target | Binary | SHA-256 |
| --- | --- | --- |
| axe x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-x86_64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.1.0/x86_64-linux/axe) | `b66b71323b794edcce6a77db17aba9bda9c408b6ee0d649429d4bc9426004586` |
| axe aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-aarch64-unknown-linux-musl) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.1.0/aarch64-linux/axe) | `a5426ff7d2d5db63c3f0a47d5ac0ff4e84a23dc338e343b39ecdff5e0749b781` |
| axe aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-aarch64-apple-darwin) · [S3](https://storage.yandexcloud.net/axe-store/axe/releases/v0.1.0/aarch64-darwin/axe) | `85a979757f24cc5f439da4179e7914e683522ab15b7eb1dc2f9c70556c09dd22` |
| axe-relay x86_64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-relay-x86_64-unknown-linux-musl) | `2cfae05b8bab2c46f97a3d2add9c37b3c5b7f93aa831eac0100c031e91cfe3f7` |
| axe-relay aarch64-linux | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-relay-aarch64-unknown-linux-musl) | `52b6b851568d3a0a8e343768211d644b8082f303be398c453ac4527074320b95` |
| axe-relay aarch64-darwin | [GitHub](https://github.com/buglloc/axe/releases/download/v0.1.0/axe-relay-aarch64-apple-darwin) | `aeec80e823c65df997d94e45c8756abc22fe1c57f06581fd58498df6bf59c3d7` |

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

Enter `nix develop .#default`, then run `just build` for the Linux x86_64 binary shown in [Quick start](#quick-start). The development shell supplies Rust, Zig, and the build tools. The build creates missing development SSH and relay identities; public Store trust and the signed bootstrap Index are tracked. `just generate-dev-keys` is only for a new development Store, not for a fresh OSS checkout without its publisher's private signing key. To build your own edition against the existing OSS Store, follow the steps in [`BOOTSTRAP.md`](BOOTSTRAP.md#use-an-existing-axe-store).

For other targets, use `just build-linux-arm64`, `just build-darwin-arm64`, or `just build-all`. Artifacts go to `dist/`; Linux builds are static ELF binaries. See [`BOOTSTRAP.md`](BOOTSTRAP.md) for build and signing setup.

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

`sshd` passes the effective mode to shell and exec sessions; child sessions cannot relax an inherited restriction. `clean-tools` removes metadata for the current Store identity from accessible cache roots; it skips automatically discovered roots that cannot be traversed but reports a permission error for the explicit `AXE_STORE_DIR`. `refresh-tools` forces an Index refresh.

### Adding a package to AXE Store

Package definitions live in [`store/nix/packages/`](store/nix/packages/). After changing them, regenerate `store/bootstrap.json` with `just store-bootstrap` and check it with `just check-store-bootstrap`. See [`BOOTSTRAP.md`](BOOTSTRAP.md#axe-store-from-scratch) for signing, building, and publishing packages.

## Vzik

Run the bounded host/container evidence collector inside AXE:

```bash
vzik collect
vzik network listeners --max-items 4096
vzik security posture
vzik capabilities
vzik capabilities porto.list
vzik capture collect --output baseline.jsonl --receipt baseline.receipt.json --stderr baseline.stderr.jsonl
```

Probes that cannot access a host service report unavailable. `vzik capabilities` lists probes and their request schemas. `collect` writes protocol-v3 JSONL: `stream_end` marks a complete stream; degraded collection exits with status `3`. A stream without `stream_end` is incomplete. `vzik capture` refuses to overwrite output files and publishes its receipt only after validating the capture.

## SSH server and relay

`sshd` accepts only OpenSSH user certificates issued by a CA listed in `keys/ssh/user_ca_keys`. The username must be allowlisted and match the certificate principal; plain public keys and certificates with critical options are rejected. Set up keys and the allowlist before starting the server (see [`BOOTSTRAP.md`](BOOTSTRAP.md)).

```bash
./dist/axe-x86_64-unknown-linux-musl --applet sshd -- --listen '[::]:6969' --workdir .
```

The first successful interactive PTY session on each SSH transport receives a short welcome line pointing to `skill://axe`, `doctor --json`, and `vzik capabilities`. Later shell channels on the same multiplexed transport, remote exec, SFTP, and forwarding receive no welcome output.

If a target cannot be reached from outside, `sshd` can register outbound with a relay. The OSS edition disables relay by default (`config/relay.json` sets `enabled_by_default` to `false` and configures no endpoints); without `--relay`, no relay task starts. `--relay ENDPOINT` enables the selected transport; `--no-relay` disables it even for editions with a configured default. The flags conflict.

TCP is the default relay transport and requires `AXE_RELAY_TOKEN` (at least 32 bytes). Use `--relay-transport quic` for QUIC; its mTLS credentials can be embedded by an edition or provided through `AXE_RELAY_QUIC_SERVER_CERT_FILE`, `AXE_RELAY_QUIC_CLIENT_CERT_FILE`, and `AXE_RELAY_QUIC_CLIENT_KEY_FILE`. See [`BOOTSTRAP.md`](BOOTSTRAP.md#relay-endpoints-and-identities) for endpoint and server setup.

Without `--relay-id`, `sshd` uses `<pidns>@<user>@<hostname>`; this ID can collide across hosts or containers that share a PID namespace. Assign a distinct `--relay-id` when clients must be addressed uniquely. The relay dashboard/API listens on loopback by default; remote access requires an authenticated HTTPS proxy.

Targets behind NAT connect outbound with `axe sshd`. `axe-relay watch [--client-id ID]` follows arrivals and departures; `axe-relay wait --client-id ID` returns one assigned SSH `HOST:PORT`. The read-only dashboard (`/`), JSON status API (`/api/v1/status`), and `status`/`clients` commands show active registrations. See [`BOOTSTRAP.md`](BOOTSTRAP.md#relay-endpoints-and-identities) for deployment.

