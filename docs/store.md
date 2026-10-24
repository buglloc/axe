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

| Command | Store ID | Purpose | Targets |
| --- | --- | --- | --- |
| `7zz` | `archives/7zz` | Create and extract archives with 7-Zip | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `age` | `security/age` | Encrypt and decrypt files with keys or passphrases | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `betterleaks` | `security/betterleaks` | Scan files, stdin, and Git history for secrets with optional validation | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `binwalk` | `debugging/binwalk` | Analyze firmware images and embedded files | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `bpftool` | `debugging/bpftool` | Inspect and manage Linux eBPF objects | `aarch64-linux`, `x86_64-linux` |
| `broot` | `files/broot` | Browse directory trees with fuzzy search and file previews | `aarch64-linux`, `x86_64-linux` |
| `bwrap` | `containers/bwrap` | Run commands in isolated Linux namespaces | `aarch64-linux`, `x86_64-linux` |
| `caddy` | `network/caddy` | Serve HTTP and reverse proxy traffic with automatic HTTPS | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `capsh` | `security/capsh` | Inspect and change Linux capabilities | `aarch64-linux`, `x86_64-linux` |
| `cek` | `containers/cek` | Inspect, copy, and compare files in OCI images without a container runtime | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `curl` | `network/curl` | Transfer data over HTTPS with Mozilla CAs | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `dbus-monitor` | `debugging/dbus-monitor` | Monitor D-Bus messages | `aarch64-linux`, `x86_64-linux` |
| `dig` | `network/dig` | Query DNS records with the BIND client | `aarch64-linux`, `x86_64-linux` |
| `dnsx` | `network/dnsx` | Resolve and enumerate DNS records | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `docker` | `containers/docker` | Manage Docker through a remote daemon | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `dumpcap` | `network/dumpcap` | Capture packets for Wireshark CLI tools | `aarch64-linux`, `x86_64-linux` |
| `ethtool` | `network/ethtool` | Inspect and configure Linux network devices | `aarch64-linux`, `x86_64-linux` |
| `fd` | `files/fd` | Find filesystem entries by name and attributes | `aarch64-darwin`, `x86_64-linux` |
| `ffuf` | `security/ffuf` | Fuzz web application paths and parameters | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `findmnt` | `storage/findmnt` | Locate and describe mounted filesystems | `aarch64-linux`, `x86_64-linux` |
| `fio` | `storage/fio` | Benchmark and verify storage I/O workloads | `aarch64-linux`, `x86_64-linux` |
| `fzf` | `files/fzf` | Interactively filter and select values | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `gdb` | `debugging/gdb` | Debug native programs and processes | `aarch64-linux`, `x86_64-linux` |
| `gdbserver` | `debugging/gdbserver` | Expose native programs to remote GDB | `aarch64-linux`, `x86_64-linux` |
| `getcap` | `security/getcap` | Display Linux file capabilities | `aarch64-linux`, `x86_64-linux` |
| `getpcaps` | `security/getpcaps` | Display Linux process capabilities | `aarch64-linux`, `x86_64-linux` |
| `gobuster` | `security/gobuster` | Enumerate web paths, DNS names, and virtual hosts | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `gori` | `security/gori` | Intercept HTTP traffic and test web applications from the terminal | `aarch64-linux`, `x86_64-linux` |
| `grpcurl` | `network/grpcurl` | Call and inspect gRPC services | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `helm` | `containers/helm` | Install and manage Kubernetes applications with charts | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `httpx` | `network/httpx` | Probe HTTP services and discover live targets | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `interactsh-client` | `security/interactsh-client` | Generate out-of-band testing payloads and collect interactions | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `jq` | `text/jq` | Process JSON | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `katana` | `security/katana` | Crawl web applications and discover URLs and endpoints | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `kubectl` | `containers/kubectl` | Manage Kubernetes clusters | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `layerx` | `containers/layerx` | Inspect container image layers in a terminal UI or CI | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `lsns` | `containers/lsns` | Show Linux namespaces and processes | `aarch64-linux`, `x86_64-linux` |
| `ltrace` | `debugging/ltrace` | Trace Linux library calls | `aarch64-linux`, `x86_64-linux` |
| `masscan` | `network/masscan` | Scan large networks quickly | `aarch64-linux`, `x86_64-linux` |
| `micro` | `text/micro` | Edit text in a terminal with syntax highlighting and familiar key bindings | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `naabu` | `network/naabu` | Find open ports | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ncat`, `nc` | `network/ncat` | Connect, listen, and proxy network traffic | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `nmap` | `network/nmap` | Discover hosts and probe services with NSE | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `nsenter` | `containers/nsenter` | Run a program in another process's namespaces | `aarch64-linux`, `x86_64-linux` |
| `nuclei` | `security/nuclei` | Scan targets with bundled vulnerability templates | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `objdump` | `debugging/objdump` | Disassemble binaries and inspect object files | `aarch64-linux`, `x86_64-linux` |
| `openssl` | `security/openssl` | Inspect certificates and perform cryptographic operations | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ox` | `text/ox` | Edit text in the terminal with Lua configuration and plugins | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `podman` | `containers/podman` | Manage Podman through a remote service | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `pspy` | `debugging/pspy` | Observe Linux processes without root | `aarch64-linux`, `x86_64-linux` |
| `python`, `python3` | `runtime/python` | Static Python with HTTP, WebSocket, and HTML libraries | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `rclone` | `storage/rclone` | Copy and synchronize files with remote storage | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `readelf` | `debugging/readelf` | Inspect ELF headers, sections, symbols, and debug information | `aarch64-linux`, `x86_64-linux` |
| `restic` | `storage/restic` | Back up and restore files in encrypted repositories | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `rg` | `files/rg` | Search file contents with regular expressions | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `rsync` | `storage/rsync` | Synchronize local and remote paths | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `s5cmd` | `storage/s5cmd` | Copy and manage S3 objects with parallel batch operations | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `scp` | `network/scp` | Copy files over OpenSSH | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `setcap` | `security/setcap` | Set Linux file capabilities | `aarch64-linux`, `x86_64-linux` |
| `sftp` | `network/sftp` | Transfer files over OpenSSH SFTP | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `socat` | `network/socat` | Transfer data between sockets and streams | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `spf` | `files/spf` | Browse and manage files in a terminal interface with built-in previews | `aarch64-linux`, `x86_64-linux` |
| `sqlite3` | `text/sqlite3` | Read and modify SQLite databases | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ss` | `network/ss` | Show sockets and network connections | `aarch64-linux`, `x86_64-linux` |
| `ssh` | `network/ssh` | Connect to hosts over OpenSSH | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ssh-add` | `network/ssh-add` | Add keys to an OpenSSH agent | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ssh-agent` | `network/ssh-agent` | Hold OpenSSH private keys for a session | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ssh-keygen` | `network/ssh-keygen` | Create and modify OpenSSH keys | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `ssh-keyscan` | `network/ssh-keyscan` | Collect OpenSSH host keys | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `stern` | `containers/stern` | Tail logs from multiple Kubernetes pods and containers | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `strace` | `debugging/strace` | Trace Linux system calls and signals | `aarch64-linux`, `x86_64-linux` |
| `subfinder` | `network/subfinder` | Enumerate subdomains | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `tcpdump` | `network/tcpdump` | Capture and display network packets | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `tmux` | `terminal/tmux` | Multiplex terminal sessions and windows | `aarch64-linux`, `x86_64-linux` |
| `tshark` | `network/tshark` | Analyze packet captures from the CLI | `aarch64-linux`, `x86_64-linux` |
| `unshare` | `containers/unshare` | Run a program in new Linux namespaces | `aarch64-linux`, `x86_64-linux` |
| `websocat` | `network/websocat` | Relay data between WebSockets and streams | `aarch64-linux`, `x86_64-linux` |
| `xh` | `network/xh` | Interactive-friendly HTTP client | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `yara` | `security/yara` | Match user-supplied YARA rules against files and process memory | `aarch64-linux`, `x86_64-linux` |
| `yarac` | `security/yarac` | Compile user-supplied YARA rules for the YARA scanner | `aarch64-linux`, `x86_64-linux` |
| `yc` | `network/yc` | Manage Yandex Cloud resources | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `yq` | `text/yq` | Process YAML, JSON, and XML | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `zdns` | `network/zdns` | Perform high-speed DNS lookups with optional local recursive resolution | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `zellij` | `terminal/zellij` | Manage terminal sessions, panes, and embedded plugins | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |
| `zgrab2` | `network/zgrab2` | Scan application-layer protocols and collect service metadata | `aarch64-darwin`, `aarch64-linux`, `x86_64-linux` |

The `nuclei` package includes pinned `nuclei-templates`; no separate template download is needed on first run.

`betterleaks` embeds its default detection rules, tokenizer, and word list. Filesystem and stdin scans do not require Git; history and clone modes require `git` on `PATH`. Live credential validation is opt-in with `--validation` and sends detected credentials to provider APIs. Use `--redact` when writing reports that must not expose secrets.

`katana` performs standard crawling and JavaScript endpoint discovery without a browser. Automatic version checks and browser downloads are disabled. Headless and hybrid modes require an installed Chrome/Chromium, `-system-chrome-path`, or `-chrome-ws-url`; no browser is bundled. Katana can return status 0 after browser initialization or crawl failures, so check diagnostics and results rather than exit status alone. Use `-debug` to expose headless browser-launch errors.

Katana's explicit `-kb`, `-filter-page-type`, and `-auto-login` features can download an upstream classification model on first use unless it is already available locally. `-kb-validate-secrets` enables provider requests; `-health-check` runs connectivity probes. Manual `-update` remains available.

`yara` and `yarac` provide the classic YARA scanner and rule compiler. Linux binaries include the `hash`, `pe`, `elf`, `cuckoo`, `dex`, `dotnet`, `macho`, and `magic` modules. The `magic` database is embedded; no host database or separate download is required. Supply your own rules and local JSON reports for `cuckoo`. No rule feed is bundled.

`zgrab2` and `zdns` do not send telemetry or perform automatic version checks, updates, or startup downloads. Network traffic is driven by the requested scans and DNS resolution; `zgrab2 --prometheus` and `ZDNS_PPROF=true` enable optional local monitoring listeners.

`zgrab2` embeds the upstream blocklist as a fallback when `~/.config/zgrab2/blocklist.conf` is absent. Existing user files take precedence; invalid files and missing explicit `-b FILE` paths remain errors. The default list excludes private and loopback networks. Use `-b FILE` for a custom list or `-b ''` to disable it for an authorized internal scan.

`zdns` supports explicit resolvers with `--name-servers` and local recursion with `--iterative`. Select the internal resolver explicitly for private DNS; `--threads` controls concurrency.

The `interactsh-client` package removes automatic version checks and their machine-information telemetry. Explicit authentication, ASN lookups, and manual updates remain opt-in.

The `gori` package disables startup update checks by default. Set `update.check_enabled` to `true` in its settings file to enable them.

The Linux `7zz` build uses the free codec set, without RAR. Darwin uses the pinned upstream universal binary; its archive version and SHA-256 must be updated together and match the Linux version.

`tmux` embeds ncurses fallback records for modern terminals, including Ghostty, Kitty, Foot, Alacritty, and WezTerm. Ghostty and Kitty records come from their upstream sources pinned by `flake.lock`; updating the nixpkgs input refreshes them at build time. Host terminfo remains available for custom definitions, but no terminfo tree is shipped. AXE shell and SSH sessions preserve the client's `TERM`; panes default to `tmux-256color`.

`zellij` embeds its terminal plugins and assets. Its Linux libcurl includes CA roots; explicit CA overrides still apply. The optional browser client loads Google Fonts.

`dig` includes CA roots for certificate-verified DNS-over-TLS and HTTPS queries. Use `+tls-ca` to enable verification, or `+tls-ca=FILE` for an explicit CA file.

`rclone` embeds CA roots and timezone data. Release checks, self-update, the GUI downloader, and cloud SDK telemetry are disabled or removed. Azure Blob inherits disabled SDK telemetry from the shared Azure authentication client options while retaining its gzip request policy. The authenticated RC API and explicit `--rc-files` remain available. Cgo-based mounting is excluded; Go FUSE mounts require host FUSE support and its mount helper.

`s5cmd` uses AWS credentials and profiles, supports custom S3 endpoints with `--endpoint-url`, and runs command files with `run`. It embeds Mozilla CA roots and additional CAs from the selected edition as a fallback when system roots are unavailable. System trust and `SSL_CERT_FILE` custom CA bundles remain available. S3-compatible providers may support only a subset of its operations.

`restic` supports local and remote encrypted repositories. It embeds edition CA roots as a fallback when system trust is unavailable; `--cacert` remains available for explicit trust. SFTP and rclone backends require `ssh` and `rclone` on `PATH`. Mounting requires host FUSE support. Self-update is excluded.

`caddy` includes the standard modules for HTTP serving, reverse proxying, and automatic HTTPS. Use `caddy list-modules --versions` to inspect bundled modules. It embeds edition CA roots as a fallback for outgoing TLS connections. Explicit trust pools remain available. Certificate and configuration state needs a writable directory. Binary-replacement commands (`upgrade`, `add-package`, and `remove-package`) are excluded; additional DNS-provider plugins are not bundled.

`layerx` reads Docker-save and OCI archives without a daemon. Live image inspection uses a Docker-compatible Docker or Podman API; `build` also needs the selected engine's CLI on `PATH`. It provides a terminal UI, JSON analysis, efficiency gates with `ci`, and image comparisons with `compare`.

`cek` reads images directly from registries or a local Docker-compatible daemon. It supports merged-filesystem inspection, file extraction, content-aware comparisons, and OCI exports. Registry authentication uses Docker configuration; configured credential helpers must be on `PATH`. It embeds edition CA roots as a fallback when system trust is unavailable, and honors `SSL_CERT_FILE`.

`bwrap` is Bubblewrap's standalone executable. Unprivileged sandboxing requires user namespaces permitted by the host kernel and security policy.

`websocat` includes CA roots for verified WSS connections. OpenSSL's system trust and `SSL_CERT_FILE` remain available for additional CAs.

`fio` includes Linux AIO and io_uring engines; host kernel and security policy determine whether they can run. The package excludes the libnbd engine and Python plotting tools. Write workloads can overwrite data: use a fresh temporary directory and a regular file for a smoke check, not a block device:

```bash
(
    umask 077
    tmp=$(mktemp -d) || exit 1
    trap 'rm -rf "$tmp"' EXIT
    fio --name=store-smoke --filename="$tmp/data" --size=1m --bs=4k \
        --rw=write --ioengine=sync --verify=crc32c --do_verify=1 \
        --verify_fatal=1 --unlink=1
)
```

`python` includes pinned pure-Python HTTP, WebSocket, and HTML libraries and a certifi CA bundle. The package excludes pip, ensurepip, and virtual-environment tooling; update its interpreter and wheels together in the package definition.

### Text editors

`micro` embeds syntax definitions, themes, help, and its built-in plugins. The linter plugin is disabled by default; set `"linter": true` in `settings.json` to enable it. Backups, history, and configuration retain upstream behavior and need writable storage. Use `-config-dir DIR` or `MICRO_CONFIG_HOME` to select a writable configuration directory.

`ox` embeds its default configuration, syntax highlighting, and the `autoindent`, `pairs`, and `quickcomment` plugins. It offers the upstream configuration assistant when `.oxrc` is absent; decline the assistant to edit without creating a configuration file. The assistant needs a writable home directory to save its settings. Additional plugins can require external commands or network access.

### File managers and read-only filesystems

`broot` and `spf` (superfile) load embedded defaults when user configuration is absent. Ordinary directory navigation and built-in text previews do not require writable configuration, state, cache, or temporary directories. Existing user configuration takes precedence; malformed or unreadable files remain errors. Both need a usable interactive terminal and a readable directory to browse.

| Dependency | `broot` | `spf` |
| --- | --- | --- |
| User configuration | `BROOT_CONFIG_DIR`, otherwise `$XDG_CONFIG_HOME/broot` or `~/.config/broot` | `$XDG_CONFIG_HOME/superfile` or `~/.config/superfile`; `--config-file` and `--hotkey-file` select existing files |
| System data | Git configuration and ignore files affect filtering; missing passwd/group entries use numeric IDs | Missing passwd/group entries do not prevent browsing; `/proc` mount information supplies the optional disk sidebar |
| Terminal data | No terminfo tree required for normal terminal sizing; a failed sizing query can try `tput` | Terminfo is optional; normal key decoding has built-in mappings |
| MIME and syntax data | Embedded signatures, syntax definitions, and highlighting themes | Built-in type detection and text highlighting; no external magic/MIME database |
| Fonts | Terminal font draws the UI; SVG text previews use host fonts | Terminal font draws the UI; Nerd Font icons are optional |
| Temporary storage | Configured preview converters, default Kitty file-based image transmission, and explicit IPC server sockets | PDF, PostScript, and video thumbnails need writable `$TMPDIR` and their external converter |

`broot` does not prompt to install shell integration. Use `broot --print-shell-function bash` for explicit setup. `--write-default-conf` and `--install` write only when requested. Normal startup does not enable file logging; `BROOT_LOG` opts into a log in the working directory. Set `kitty_graphics_transmission: chunks` to avoid temporary files for Kitty image previews. SVG preview fonts and explicit preview converters remain host dependencies. Linux clipboard integration uses X11 when available.

`spf` does not extract configuration or themes, create trash directories, or open a log at startup. Explicit pin/toggle changes use `$XDG_DATA_HOME/superfile`; `debug = true` and `cd_on_quit = true` use `$XDG_STATE_HOME/superfile`. Defaults are `~/.local/share` and `~/.local/state`. Trash operations need a writable trash destination. Config repair flags and chooser output also need writable destinations.

Without `/proc`, `spf` emits disk-discovery diagnostics but directory navigation remains available. On a read-only filesystem, explicit file changes, state persistence, and preview conversions that need temporary files still require an appropriate writable destination; ordinary navigation does not silently create a temporary configuration tree.

Neither package bundles external commands. Editors, file openers, shell commands, and configured `broot` verbs require their programs on `PATH`. Optional `spf` integrations use `exiftool`, `zoxide`, `bat`/`batcat`, and clipboard tools; thumbnail converters use `pdftoppm`, `gs`, or `ffmpeg`. These are not needed for basic navigation or built-in text previews.

Neither tool sends telemetry or checks for application updates automatically. Superfile's update client is removed from the Store build.

## Security source updates

OpenSSL, curl, and NSS CA data come from the nixpkgs revision pinned in `flake.lock`. Update them with `nix flake update nixpkgs`, then regenerate and check the Store bootstrap metadata. The development shell uses the same pin so unmodified toolchain dependencies can be substituted from `cache.nixos.org`.

Library and CA-bundle updates require rebuilding their consumers, including tools whose own version is unchanged. A new AXE executable does not replace published Store payloads. Build and publish the updated packages, then refresh the signed bootstrap snapshot through the publication workflow below.

## Adding a package

Package definitions live in [`store/nix/packages/`](../store/nix/packages/).

Each package belongs to one primary category, shared with bundled commands. Its Store ID is generated as `<category>/<name>`; command aliases use the same package and category. Choose the category by the tool's main purpose, not its implementation or transport:

| Category ID | Label |
| --- | --- |
| `files` | Files & search |
| `text` | Text & data |
| `archives` | Archives & compression |
| `storage` | Storage & backup |
| `process` | Processes |
| `system` | Host & system |
| `network` | Network & remote access |
| `containers` | Containers & orchestration |
| `debugging` | Debugging & binary analysis |
| `security` | Security & permissions |
| `terminal` | Shell & terminal |
| `runtime` | Runtimes |
| `axe` | AXE |

For example, `restic`, `fio`, `findmnt`, `rclone`, `rsync`, and `s5cmd` belong to `storage`, while `binwalk` and `pspy` belong to `debugging`. Terminal multiplexers belong to `terminal`; Python belongs to `runtime`. Define only categories containing packages in the public Store inventory; do not add empty modules or compatibility category aliases. Changing a package's category changes its Store ID and requires regenerating the inventory and the signed snapshot before release.

For CGO-free Go packages whose tests invoke `go build`, set `GO_EXTLINK_ENABLED=0` as well as `CGO_ENABLED=0`; package `ldflags` do not reach those helper builds. The musl Go compiler can otherwise default to external linking. Apply patches that only remove dependency imports after vendoring (`postConfigure`) to preserve the pinned upstream vendor hash.

After changing package definitions, regenerate and check the inventory:

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
