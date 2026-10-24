# Commands and command resolution

Run these commands with an installed `axe` binary to inspect bundled applets and the configured Store:

```bash
axe --list
axe commands
axe commands ps
axe doctor --json
```

`commands` returns a versioned JSON inventory. `availability` is `local`, `on_demand`, or `blocked`; `local_path` is the published applet path, if one exists. Use the shell's `type`, `command`, and `command -v` builtins to inspect aliases, functions, and builtins.

Each command has one primary `category`, shared by bundled and Store commands.
Aliases inherit their target's category. Categories describe purpose; `source`
separately identifies bundled or Store delivery. The website uses the same
category IDs for filters and inventory rows.

## Command resolution

Within the shell, resolution follows this order:

```text
alias/function → shell builtin → bundled applet → AXE Store → PATH
```

Bundled applets take precedence over AXE Store and `PATH`. Commands not registered by AXE use normal `PATH` lookup. For a Store command, AXE tries a matching host executable only after a transient or unavailable delivery failure, excluding its own applet bridge and executable. If none is usable, the delivery failure exits with status 126. Signature, digest, schema, TLS, integrity, and configuration failures also exit with status 126, without falling back to `PATH`. An unresolved command exits with status 127.

For an asynchronous command, `$!` holds the child PID. When Brush runs a list in the current process, there is no separate PID. Instead, AXE puts a `%N` job specification in `$!`; `wait` accepts it. The in-process job ends with the current shell.

A pipeline stage or background list running in a subshell returns only its exit status to the parent. `exit`, `return`, `break`, and expansion or builtin errors end that subshell alone; `errexit` and `pipefail` still determine how the parent handles a failing status. With `lastpipe` enabled, control flow in a final stage running in the parent applies to the parent.

Call a bundled applet by name, explicitly with `--applet`, or from an AXE shell:

```bash
axe jq -- -n '{ok: true}'
axe --applet jq -- -n '{ok: true}'
axe -c 'jq -n "{ok: true}"; vzik capabilities'
```

The first `--` after an applet name belongs to AXE. To pass an option terminator to the applet itself, use two: `axe tree -- -- -h` lists a path named `-h`. Applet symlinks and commands inside an AXE shell use the applet's ordinary argument syntax.

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

Brush builtins handle shell state, jobs, history, completion, and POSIX/Bash-style control flow. The inventory below groups bundled applets by their primary purpose.

| Category | Bundled commands |
| --- | --- |
| `files` — Files & search | `basename`, `cp`, `dir`, `dirname`, `find`, `inotifywait`, `inotifywatch`, `install`, `link`, `ln`, `ls`, `mkdir`, `mkfifo`, `mknod`, `mktemp`, `mv`, `pathchk`, `pwd`, `readlink`, `realpath`, `rm`, `rmdir`, `stat`, `touch`, `tree`, `truncate`, `unlink`, `vdir` |
| `text` — Text & data | `awk`, `b2sum`, `base32`, `base64`, `basenc`, `cat`, `cksum`, `cmp`, `comm`, `csplit`, `cut`, `diff`, `diff3`, `expand`, `factor`, `fmt`, `fold`, `grep`, `egrep`, `fgrep`, `rgrep`, `head`, `hexdump`, `hd`, `join`, `jq`, `md5sum`, `nl`, `numfmt`, `od`, `paste`, `pr`, `ptx`, `sed`, `seq`, `sha1sum`, `sha224sum`, `sha256sum`, `sha384sum`, `sha512sum`, `shuf`, `sort`, `split`, `sum`, `tac`, `tail`, `tee`, `tr`, `tsort`, `unexpand`, `uniq`, `wc` |
| `archives` — Archives & compression | `tar`, `gzip`, `gunzip`, `zcat`, `gzcat`, `bzip2`, `bunzip2`, `bzcat`, `xz`, `unxz`, `xzcat`, `zstd` |
| `storage` — Storage & backup | `blkid`, `blockdev`, `dd`, `df`, `du`, `iostat`, `mount`, `mountpoint`, `sync`, `umount` |
| `process` — Processes | `hugetop`, `kill`, `killall`, `lsof`, `nice`, `nohup`, `pgrep`, `pidof`, `pidwait`, `pkill`, `pmap`, `ps`, `pwdx`, `skill`, `snice`, `timeout`, `top`, `watch`, `xargs` |
| `system` — Host & system | `arch`, `date`, `dmesg`, `free`, `hostid`, `hostname`, `dnsdomainname`, `ipcs`, `last`, `logname`, `lsmod`, `lspci`, `lsscsi`, `lsusb`, `modinfo`, `nproc`, `pinky`, `slabtop`, `sysctl`, `tload`, `uname`, `uptime`, `users`, `vmstat`, `vzik`, `w`, `who` |
| `network` — Network & remote access | `http`, `arp`, `ifconfig`, `ip`, `ipaddr`, `iplink`, `ipneigh`, `iproute`, `iprule`, `ipcalc`, `host`, `nslookup`, `ping`, `ping6`, `traceroute`, `traceroute6` |
| `containers` — Containers & orchestration | `chroot` |
| `debugging` — Debugging & binary analysis | `file`, `goblin`, `strings` |
| `security` — Security & permissions | `chgrp`, `chmod`, `chown`, `groups`, `id`, `shred`, `whoami` |
| `terminal` — Shell & terminal | `[`, `dircolors`, `echo`, `env`, `expr`, `false`, `more`, `printenv`, `printf`, `sleep`, `stty`, `test`, `true`, `tty`, `which`, `yes` |
| `runtime` — Runtimes | Store only; see [Store inventory](store.md#package-inventory) |
| `axe` — AXE | `commands`, `doctor`, `clean-tools`, `refresh-tools`, `sshd` |

For the bundled HTTP applet, see [HTTP requests](http.md).

For passive host context and targeted evidence, see [Vzik](vzik.md).

The bundled `zstd` uses the linked Zstandard codec and keeps input files by default. It supports compression/decompression (`-d`), stdout (`-c`), explicit output (`-o FILE`), integrity checks (`-t`), overwrite (`-f`), input removal (`--rm`), levels 1–19, `--ultra` levels 20–22, `--fast[=N]`, and worker counts (`-T N`; zero selects available CPUs). Compression includes a checksum; decompression accepts concatenated frames. Run `axe zstd -- --help` for its supported CLI options.

`tar` accepts old-style clusters such as `tar cfz archive.tgz directory`; options that take arguments consume the following words. Listings stream as entries are read. `--strip-components=N` affects extraction, not listing, and extraction checks member paths, parent directories, and hardlink targets against the destination. Missing requested members produce an error and exit status 2.

`gzip -dcf` copies non-gzip input to stdout. Force mode does not rename plain files during file-to-file decompression, and `-l` still requires gzip data. Concatenated members are decoded in order; trailing non-gzip bytes produce status 2 unless force-to-stdout mode copies them.

`tree -h` prints compact human-readable sizes before names, for example `[4.9K]  big`. It accepts clustered and repeated flags; `--help` prints help. `file` supports `-E` for failing status on inspection errors, `-N` to disable name padding, `--mime-encoding`, and `-h`/`--no-dereference`. Empty regular files use `inode/x-empty`; empty stdin uses `application/x-empty`.

## Storage reports

`df` supports `-m`, `--output`, and `--type=TYPE`. Without `-a`, it selects one representative mount for each device, including bind aliases. Block-size options follow command-line order.

`blkid -l -t NAME=value` prints the first match; `-L LABEL` and `-U UUID` print only the matching device path. Tokens are case-sensitive. `-p` includes filesystem version metadata. Partition-table metadata is separate from filesystem type detection.

`blockdev --report` accepts an optional device list. With no devices, `--report` must be the first argument; it reads `/proc/partitions`, skips zero-capacity entries, and continues quietly after device errors. An empty list succeeds; any device failure makes the report unsuccessful.

`mountpoint PATH` exits with status 0 for a mountpoint, 32 for an existing
non-mountpoint, and 1 when the path cannot be resolved. Linux mount detection
uses `/proc/self/mountinfo`, including bind mounts on the same filesystem.

## Mounting filesystems

The bundled `mount` follows util-linux `mount(8)`. Without arguments it lists mounts; with `-t`/`-o` it mounts a filesystem. It also supports remount, `--bind`/`--rbind`, `--move`, `--make-*` propagation, single-operand lookup in `/etc/fstab`, `-a` with `-t` and `-O` filters, and `/sbin/mount.<type>` helpers.

A remount changes only the flags you name. Other flags, such as `nosuid`, `nodev`, and the atime mode, keep their current values.

`umount` accepts mountpoints or sources, including explicit `--target` and `--source` operands. `-A`/`--all-targets` requires an operand and selects every mountpoint of that filesystem. `-R` follows the mount-ID graph, including overmount stacks; a failure stops that chain while independent operands are still attempted. `-a` preserves `/` and excludes `proc`, `devfs`, `devpts`, `sysfs`, `rpc_pipefs`, `nfsd`, and `selinuxfs`; `-t` replaces those exclusions. Type (`-t`) and option (`-O`) filters apply to `-a`, not explicit targets.

Unmount options include force (`-f`), lazy detach (`-l`), read-only fallback (`-r`), fake execution (`--fake`), disabled canonicalization (`-c`), disabled helpers (`-i`), quiet missing-target errors (`-q`), and a mount namespace (`-N PID|PATH`). `--fake` performs no unmount or read-only remount. Verbose messages (`-v`) go to stderr.

When the filesystem rejects an option, `mount` prints the kernel's message, for example `fsconfig() failed: tmpfs: Unknown parameter 'size2'`. On kernels without the `fsopen` API it falls back to `mount(2)` and prints a generic error.

Image files mount through loop devices. `mount` attaches one for `-o loop`, `loop=/dev/loopN`, `offset=`, or `sizelimit=`, and for any regular file mounted as a block filesystem. The device detaches after the last unmount. If the same file and range is already attached, `mount` reuses that device. It refuses an overlapping range, and it attaches a write-protected file read-only.

For `mount`, `-o encryption`, `-N`, `-T`, and `-F` are not supported.

## Process and network compatibility

The bundled `ip` renders tunnel link addresses as IPv4 or IPv6 addresses, matching iproute2 in text and JSON output. Ethernet and other hardware addresses use colon-separated hexadecimal bytes.

`ip` provides read-only inspection with text and JSON output. It accepts `-j`/`-json`, pretty JSON (`-p`), brief (`-br`/`-brief`), oneline (`-o`/`-oneline`), and numeric names (`-N`). `route get` supports device, source, mark, and interface selectors without loading a full route dump. Unsupported mutation verbs, detail/resolve options, and address families are rejected. `arp` displays the kernel's `/proc/net/arp` table with net-tools ATF flags.

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
