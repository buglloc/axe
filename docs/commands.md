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
| Process | `free`, `hugetop`, `killall`, `pgrep`, `pidof`, `pidwait`, `pkill`, `pmap`, `ps`, `pwdx`, `skill`, `slabtop`, `snice`, `sysctl`, `tload`, `top`, `vmstat`, `w`, `watch` | Linux processes and system state |
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

The bundled `ps` accepts BSD option clusters such as `aux` and `auxww`. `a` selects processes with a terminal, `x` selects the current user's processes including those without a terminal, and `ax` or `aux` selects all processes visible through `/proc`, including other users' processes without a terminal.

`ps -a` selects terminal-attached processes other than session leaders. `T` selects the caller's terminal; `r` restricts the selected set to running or uninterruptible processes. `-U` filters real UIDs, while `-u` and `--user` filter effective UIDs; user names, numeric IDs, and comma- or whitespace-separated lists are accepted. Explicit `-p` lists replace broad selections such as `-e` or `ax`, but combine with UID lists and `T`.

`ps -f` prints `UID PID PPID C STIME TTY TIME CMD`, with effective-user names and `TIME` in `HH:MM:SS` form. A selection with no matching processes prints its headers and exits with status 1. Custom `-o` headers disappear only when every selected field has an empty label, such as `-o pid=,args=`. AXE carries its `ps` fixes in the vendored `procutils-ps` backend.

`ps -L`, `-T`, and BSD `H` (including `axH` and `auxH`) list actual Linux tasks. `PID` and `TGID` identify the process; `TID`, `LWP`, and `SPID` identify the task, including the main thread. CPU time, state, command name, and nice value come from that task's `/proc/PID/task/TID/stat`; memory and `NLWP` belong to the process. `-L` adds `LWP` to fixed formats, and `-T` adds `SPID`. `-m` or BSD `m` prints each process summary before its tasks, with `-` for fields that do not apply to that row. `-p` accepts process IDs, not nonleader task IDs. With `r`, flat thread modes select active tasks; mixed mode selects active processes.

`ps --sort=KEY,-KEY,...` and BSD `k` sort by multiple keys, with `+` or no prefix for ascending order and `-` for descending order. Numeric keys use raw values, not rounded or masked output: `%cpu` uses lifetime utilization, `%mem` uses RSS, and `pri` uses kernel priority rather than the displayed `39 - priority`. User/group names sort as text; UID/GID fields sort numerically. `comm` sorts task names; `args`, `cmd`, and `command` sort full command lines. Keys are case-sensitive; invalid, missing, or repeated sort options exit with status 1. Equal keys retain PID/TID order. Flat thread modes sort all tasks together; mixed mode sorts process groups and then their tasks, keeping summaries first.

```console
axe ps -eL
axe ps -L -p "$PID" -o pid,tid,pcpu,stat,comm --sort=-pcpu
axe ps aux --sort=-rss,pid
axe ps axk-rss,pid
```

AXE keeps all task rows when sorting and reports aggregate process CPU in mixed summaries. It terminates mixed running-only selection even when only a worker is active. These differ from task loss, leader-only summary CPU, and a hang observed in procps-ng 4.0.6. `vsize` sorts actual stat bytes, whereas `vsz` sorts status `VmSize`; the local procps-ng 4.0.6 leaves `vsize` in PID order.

`ps` takes its output width from `COLUMNS` or the terminal; redirected output without `COLUMNS` is unbounded up to its 128 KiB output-buffer limit. `w` or `-w` widens bounded output to at least 132 columns. Two `w` options, including `ww`, `-ww`, or `w -w`, remove the width bound. `--cols WIDTH`, `--columns WIDTH`, and `--width WIDTH` set an explicit width; the last explicit value wins, then the `w` options apply. Command fields are clipped by display columns without splitting UTF-8. Fixed fields and headers are preserved even on a narrow terminal; column padding remains dynamic rather than using procps's fixed minimum widths.

`RSS` comes from `/proc/PID/status`'s `VmRSS` in KiB, with zero for processes without that field. `%MEM` truncates to one decimal place. `%CPU` is a lifetime average using `CLOCK_BOOTTIME` ticks: it truncates to one decimal below 100%, then prints whole percentages without capping multicore usage. `C` truncates to an integer capped at 99. `PRI` is `39 - kernel priority` (`19 - nice` for normal scheduling), and `F` shows the low three legacy flag bits after shifting by six. Elapsed-time fields subtract start ticks before converting to seconds.

Command text supports C and UTF-8 locales. Newlines in command-line arguments become spaces; other control characters become `?`, including terminal escape sequences. Legacy non-UTF-8 character sets are not emulated.

`killall` sends a signal to every process with an exact Linux kernel command name (`comm`, limited to 15 bytes), not a command-line substring or regular expression. The default signal is `TERM`. Supported options are `-s SIGNAL`, `-SIGNAL`, `-NUM`, `-I` for case-insensitive names, `-u USER` for a real-UID filter, `-l` to list signals, `-q`, and `-v`. No match or a failed signal delivery exits with status 1; invalid arguments exit with status 2. It does not support signal 0, full-length executable-name matching, process-group signaling, interactive confirmation, waiting, or the regex, age, namespace, and SELinux filters from psmisc `killall`.
