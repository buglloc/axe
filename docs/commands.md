# Commands and command resolution

Run these commands with an installed `axe` binary to inspect bundled applets and the configured Store:

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
alias/function → shell builtin → bundled applet → AXE Store → PATH
```

Bundled applets take precedence over AXE Store and `PATH`. Commands not registered by AXE use normal `PATH` lookup. For a Store command, AXE tries a matching host executable only after a transient or unavailable delivery failure, excluding its own applet bridge and executable. If none is usable, the delivery failure exits with status 126. Signature, digest, schema, TLS, integrity, and configuration failures also exit with status 126, without falling back to `PATH`. An unresolved command exits with status 127.

For an asynchronous command, `$!` holds the child PID. When Brush runs a list in the current process, there is no separate PID. Instead, AXE puts a `%N` job specification in `$!`; `wait` accepts it. The in-process job ends with the current shell.

Call a bundled applet by name, explicitly with `--applet`, or from an AXE shell:

```bash
axe jq -- -n '{ok: true}'
axe --applet jq -- -n '{ok: true}'
axe -c 'jq -n "{ok: true}"; vzik capabilities'
```

A symlink named after a bundled applet also runs that applet; for example, a link named `jq` to the installed `axe` binary runs the bundled `jq`. Inside Brush, no `axe` prefix is needed. AXE-managed shells and SSH exec sessions export `AXE=true`.

The applet PATH bridge is best-effort; bundled commands still resolve without it. See [Runtime survivability](runtime-survivability.md) for child execution and behavior when the executable is removed, and [AXE Store](store.md) for Store storage and delivery.

## Interactive prompt

The default `PS1` is `axe \w\$ `: the current directory, with `~` for the home
directory, followed by `$` for a regular user or `#` for root. The prompt is
independent of the executable's filename.

An inherited `PS1`, including an empty value, takes precedence over the default.
Startup files can override it. For an ordinary interactive shell, AXE reads
`~/.bashrc` and then `~/.brushrc`; put AXE-specific settings in `~/.brushrc`:

```bash
PS1='\u@\h:\w\$ '
```

A login shell (`axe -l`) reads the first available file among `~/.bash_profile`,
`~/.bash_login`, and `~/.profile`. Set `PS1` there or source `~/.brushrc` from it.
`--norc` skips rc files; `--noenv` ignores inherited environment variables.
Non-interactive shells do not receive the default `PS1`.

## Bundled commands

Bundled commands are built into `axe`; Store commands are signed on-demand artifacts. An inventory entry does not guarantee a reachable download. Linux-only commands are absent from Darwin builds.

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
| Evidence | `vzik` | Passive host overview, targeted process inspection, bounded security evidence in JSONL, and sealed private captures |

For the bundled HTTP applet, see [HTTP requests](http.md).

For passive host context and targeted evidence, see [Vzik](vzik.md).

## Process and network compatibility

The bundled `ip` renders tunnel link addresses as IPv4 or IPv6 addresses, matching iproute2 in text and JSON output. Ethernet and other hardware addresses use colon-separated hexadecimal bytes.

The bundled `ps` accepts BSD option clusters such as `aux` and `auxww`. `a` selects processes with a terminal, `x` selects the current user's processes including those without a terminal, and `ax` or `aux` selects all processes visible through `/proc`, including other users' processes without a terminal.

`ps -a` selects terminal-attached processes other than session leaders. `T` adds processes on the caller's terminal; `r` restricts the selected set to running or uninterruptible processes. `-U` selects real UIDs, while `-u` and `--user` select effective UIDs; user names, numeric IDs, and comma- or whitespace-separated lists are accepted. Explicit `-p` lists replace broad selections such as `-e` or `ax`, but combine additively with UID lists and `T`.

`ps -f` prints `UID PID PPID C STIME TTY TIME CMD`, with effective-user names and `TIME` in `HH:MM:SS` form. With no matching processes, it prints the headers and exits with status 1. Custom `-o` headers disappear only when every field has an empty label, as in `-o pid=,args=`.

`ps -L`, `-T`, and BSD `H` list threads, including the main thread. `PID` and `TGID` identify the process; `TID`, `LWP`, and `SPID` identify the thread. Memory and `NLWP` describe the process. `-L` adds `LWP` to fixed formats; `-T` adds `SPID`. `-m` or BSD `m` prints each process summary before its threads, with `-` for inapplicable fields. `-p` accepts process IDs, not nonleader thread IDs. With `r`, flat thread modes select active threads; mixed mode selects active processes.

`ps --sort=KEY,-KEY,...` and BSD `k` accept multiple case-sensitive keys: `+` or no prefix means ascending, `-` means descending. Sorting uses values before display rounding or clipping. `%cpu` is lifetime utilization, `%mem` sorts RSS, and `pri` sorts kernel priority. `comm` sorts thread names; `args`, `cmd`, and `command` sort full command lines. Invalid, missing, or repeated sort options exit with status 1. Equal keys retain PID/TID order. Flat thread modes sort threads together; mixed mode keeps each summary before its sorted threads.

```console
axe ps -eL
axe ps -L -p "$PID" -o pid,tid,pcpu,stat,comm --sort=-pcpu
axe ps aux --sort=-rss,pid
axe ps axk-rss,pid
```

Output width comes from `COLUMNS` or the terminal. Redirected output without `COLUMNS` has no screen-width bound, but retains a 128 KiB output-buffer limit. `w` or `-w` widens bounded output to at least 132 columns; two occurrences, including `ww`, `-ww`, or `w -w`, remove the width bound. `--cols`, `--columns`, and `--width` set an explicit width; the last value wins, then `w` options apply. Command fields are clipped without splitting UTF-8; fixed fields and headers survive narrow widths.

`RSS` is in KiB; `%MEM` truncates to one decimal place. `%CPU` is a lifetime average, not recent utilization: it truncates to one decimal below 100%, then prints whole percentages without capping multicore usage. Use `top` for recent CPU activity.

Command text supports C and UTF-8 locales. Newlines in command-line arguments become spaces; other control characters become `?`, including terminal escape sequences. Legacy non-UTF-8 character sets are not emulated.

`killall` sends a signal to every process with an exact Linux kernel command name (`comm`, limited to 15 bytes), not a command-line substring or regular expression. The default signal is `TERM`. Supported options are `-s SIGNAL`, `-SIGNAL`, `-NUM`, `-I` for case-insensitive names, `-u USER` for a real-UID filter, `-l` to list signals, `-q`, and `-v`. No match or a failed signal delivery exits with status 1; invalid arguments exit with status 2. It does not support signal 0, full-length executable-name matching, process-group signaling, interactive confirmation, waiting, or the regex, age, namespace, and SELinux filters from psmisc `killall`.
