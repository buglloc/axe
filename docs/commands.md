# Commands and command resolution

Run these commands with an installed `axe` binary. The inventory reflects its bundled applets and configured Store; there is no need to disable Store to list commands or run bundled applets:

```bash
axe --list
axe commands
axe commands ps
axe doctor --json
```

`commands` returns a versioned JSON inventory. `availability` is `local`, `on_demand`, or `blocked`; `local_path` is the published applet path, if one exists. Use the shell's `type`, `command`, and `command -v` builtins to inspect aliases, functions, and builtins.

## Command resolution

Within the shell, resolution follows this order:

```text
alias/function → shell builtin → bundled applet → AXE Store → PATH after transient/unavailable Store delivery failure
```

Bundled applets take precedence over AXE Store and `PATH`. If Store delivery fails as transient or unavailable, AXE may try a matching host executable from `PATH`, excluding its own applet bridge and executable. If no usable host executable exists, the delivery failure exits with status 126. Signature, digest, schema, TLS, integrity, and configuration failures block execution with status 126; AXE does not fall back to `PATH`. An unresolved command exits with status 127.

For an asynchronous command, `$!` holds the child PID. When Brush runs a list in the current process, there is no separate PID. Instead, AXE puts a `%N` job specification in `$!`; `wait` accepts it. The in-process job ends with the current shell.

Call a bundled applet by name, explicitly with `--applet`, or from an AXE shell:

```bash
axe jq -- -n '{ok: true}'
axe --applet jq -- -n '{ok: true}'
axe -c 'jq -n "{ok: true}"; vzik capabilities'
```

A symlink named after a bundled applet also dispatches it using the link name as `argv[0]`; for example, a link named `jq` to the installed `axe` binary runs the bundled `jq`. Inside Brush, no `axe` prefix is needed. AXE-managed shells and SSH exec sessions export `AXE=true` even when the best-effort `PATH` bridge is unavailable. Use `doctor --json` for version and runtime capabilities.

The applet PATH bridge is best-effort; bundled commands still resolve without it. [Runtime survivability](runtime-survivability.md) explains child execution, the bridge, and behavior when the executable is unlinked. AXE Store uses separate storage roots; see [AXE Store](store.md).

## Bundled commands

**Bundled** means code inside `axe`; **AXE Store** means a signed on-demand artifact from its Index. Store inventory does not guarantee that downloads are published or reachable. Run `commands` to inspect the active executable and its configured Store.

| Group | Commands | Purpose |
| --- | --- | --- |
| Shell | Brush builtins | Shell state, jobs, history, completion, and POSIX/Bash-style control flow |
| Coreutils | `uutils/coreutils`, including `hostname` and its `dnsdomainname` alias | Basic file, text, process, and environment operations |
| Process | `free`, `hugetop`, `pgrep`, `pidof`, `pidwait`, `pkill`, `pmap`, `ps`, `pwdx`, `skill`, `slabtop`, `snice`, `sysctl`, `tload`, `top`, `vmstat`, `w`, `watch` | Linux processes and system state |
| System | `dmesg`, `hexdump`, `last`, `mountpoint` | Kernel log, hex dumps, login history, and mount points |
| Text | `awk`, `grep`, `egrep`, `fgrep`, `rgrep`, `sed`, `find`, `xargs`, `diff`, `cmp`, `diff3` | Search, transform, and compare data |
| Data | `jq` | JSON queries and transformations |
| Archives | `tar`, `gzip`, `gunzip`, `zcat`, `gzcat`, `bzip2`, `bunzip2`, `bzcat`, `xz`, `unxz`, `xzcat` | Archives and compressed streams |
| Binary inspection | `file`, `goblin`, `strings` | Identify and inspect binary formats |
| Network | `http`, `arp`, `ifconfig`, `ip`, `ipaddr`, `iplink`, `ipneigh`, `iproute`, `iprule`, `ipcalc`, `host`, `nslookup`, `ping`, `ping6`, `traceroute`, `traceroute6` | Bounded HTTP requests, Linux networking, DNS, and connectivity |
| Storage | `blkid`, `blockdev`, `mount` | Block devices and mounts; bundled `mount` is read-only |
| Inspection | `iostat`, `ipcs`, `lsmod`, `lsof`, `lspci`, `lsscsi`, `lsusb`, `modinfo` | I/O, IPC, modules, open files, and devices |
| Filesystem | `inotifywait`, `inotifywatch`, `tree`, `which` | File events, directory trees, and executable lookup |
| AXE control | `commands`, `doctor`, `clean-tools`, `refresh-tools` | Command inventory, diagnostics, and AXE Store cache |
| Services | `sshd` | Certificate-only SSH/SFTP server |
| Evidence | `vzik` | Bounded host/container evidence in JSONL |

Linux-only commands are not registered in Darwin builds. For the bundled HTTP applet, see [HTTP requests](http.md).

The bundled `ip` renders tunnel link addresses as IPv4 or IPv6 addresses, matching iproute2 in text and JSON output. Ethernet and other hardware addresses use colon-separated hexadecimal bytes.
