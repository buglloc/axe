# AXE Store

AXE Store delivers tools on demand. Nix builds the packages; `axe-store` signs their metadata and publishes them. AXE verifies signatures, sizes, and SHA-256 hashes before execution. Previously cached tools can run offline. On Linux, supported single-file tools can run even without a writable executable filesystem.

`AXE_STORE_DIR` sets the preferred cache root. If it is unavailable, AXE tries configured roots, the platform cache, writable persistent mounts, tmpfs, and the temporary directory. Store metadata is isolated by URL, trust keys, and channel. Use separate roots for co-installed editions.

Set the mode with `AXE_STORE_MODE` or `sshd --store-mode`:

- `auto` (default): use the verified cache and refresh stale metadata from the network.
- `cache-only`: use embedded metadata and the verified cache without network requests.
- `off`: do not initialize Store or register its commands.

In `auto` and `cache-only`, transient delivery failures or an unavailable tool can permit `PATH` fallback. Signature, integrity, and configuration errors do not. `sshd` passes its mode to shell and exec sessions; child sessions cannot relax an inherited restriction.

`axe refresh-tools` fetches and verifies the current Index; it requires `auto` mode. `axe clean-tools` removes metadata for the current Store identity, not shared content-addressed objects. It skips inaccessible automatically discovered roots but reports permission errors for an explicit `AXE_STORE_DIR`.

## Package inventory

This table comes from [`store/bootstrap.json`](../store/bootstrap.json).

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
| `gori` | Intercept HTTP traffic and test web applications from the terminal | `aarch64-linux`, `x86_64-linux` |
| `grpcurl` | Call and inspect gRPC services | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `httpx` | Probe HTTP services and discover live targets | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `interactsh-client` | Generate out-of-band testing payloads and collect interactions | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
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
| `yc` | Manage Yandex Cloud resources | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `yq` | Process YAML, JSON, and XML | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |

The `nuclei` package includes pinned `nuclei-templates`; no separate template download is needed on first run.

The `interactsh-client` package removes automatic version checks and their machine-information telemetry. Explicit authentication, ASN lookups, and manual updates remain opt-in.

The `gori` package disables startup update checks by default. Set `update.check_enabled` to `true` in its settings file to enable them.

## Adding a package

Package definitions live in [`store/nix/packages/`](../store/nix/packages/). After changing them, regenerate and check the inventory:

```bash
just store-bootstrap
just check-store-bootstrap
```

See [BOOTSTRAP.md](../BOOTSTRAP.md) for identities and Store access.

## Publication

Run Store recipes from the development shell on the trusted publisher. They use Podman by default; set `CONTAINER_RUNTIME` to use another compatible runtime. Check `AXE_EDITION_ROOT` and its `config/store.json` before uploading.

```bash
just store-build    # build and sign packages without uploading
just store-publish  # upload the staged packages and update the Index
```

To regenerate the inventory, build, publish, and replace the edition's signed bootstrap snapshot in one step:

```bash
just store-sync
```

Publication rejects removal of previously published tool, channel, or target entries. For an intentional removal, review the affected consumers before running `AXE_STORE_ALLOW_TARGET_REMOVAL=1 just store-sync`. This override permits removal; it does not migrate installed binaries.

Review and commit the updated `store/bootstrap.json` and `store/bootstrap-index.cbor.zst` together before an AXE release. Store publication does not publish AXE binaries; follow [Local releases](release.md) for that.
