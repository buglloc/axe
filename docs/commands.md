# Commands and command resolution

From the repository root, the executable reports its own exact command inventory (use the path from your build). These examples disable Store so they work offline; after configuring a signed Store, omit `AXE_STORE_MODE=off` to include on-demand commands:

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

[Runtime survivability](runtime-survivability.md) covers self-exec backend ordering, the `PATH` bridge, behavior after unlinking, and controlled degradation. AXE does not place temporary `PATH` bridges or self-exec relays on arbitrary writable mounts. It tries `$XDG_CACHE_HOME/axe` (or the platform cache), then `$XDG_RUNTIME_DIR/axe` and `$TMPDIR/axe-<uid>`. If `AXE_WORK_DIR` is set, AXE requires that root and does not fall back automatically. AXE Store uses a separate storage-root order; see the [Store documentation](store.md).

## Bundled commands

**Bundled** means code inside `axe`; **AXE Store** means a signed on-demand artifact from its Index. Store inventory does not guarantee that downloads are published or reachable. Run `commands` to inspect the active executable and its configured Store.

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

Linux-only commands are not registered in Darwin builds. For the bundled HTTP applet, see [HTTP requests](http.md).
