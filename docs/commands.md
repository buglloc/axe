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
| `files` — Files & search | `basename`, `cp`, `dir`, `dirname`, `find`, `fsnotifywait`, `fsnotifywatch`, `inotifywait`, `inotifywatch`, `install`, `link`, `ln`, `ls`, `mkdir`, `mkfifo`, `mknod`, `mktemp`, `mv`, `namei`, `pathchk`, `pwd`, `readlink`, `realpath`, `rm`, `rmdir`, `stat`, `touch`, `tree`, `truncate`, `unlink`, `vdir` |
| `text` — Text & data | `awk`, `b2sum`, `base32`, `base64`, `basenc`, `cat`, `cksum`, `cmp`, `comm`, `csplit`, `cut`, `diff`, `expand`, `factor`, `fmt`, `fold`, `grep`, `egrep`, `fgrep`, `rgrep`, `head`, `hexdump`, `hd`, `join`, `jq`, `md5sum`, `nl`, `numfmt`, `od`, `paste`, `pr`, `ptx`, `sdiff`, `sed`, `seq`, `sha1sum`, `sha224sum`, `sha256sum`, `sha384sum`, `sha512sum`, `shuf`, `sort`, `split`, `sum`, `tac`, `tail`, `tee`, `tr`, `tsort`, `unexpand`, `uniq`, `wc` |
| `archives` — Archives & compression | `tar`, `gzip`, `gunzip`, `zcat`, `gzcat`, `bzip2`, `bunzip2`, `bzcat`, `xz`, `unxz`, `xzcat`, `zstd` |
| `storage` — Storage & backup | `blkid`, `blockdev`, `dd`, `df`, `du`, `iostat`, `lsblk`, `lslocks`, `mount`, `mountpoint`, `sync`, `umount` |
| `process` — Processes | `hugetop`, `fuser`, `kill`, `killall`, `lsof`, `nice`, `nohup`, `pgrep`, `pidof`, `pidwait`, `pkill`, `pmap`, `ps`, `pstree`, `pwdx`, `skill`, `snice`, `timeout`, `top`, `watch`, `xargs` |
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

## Linux diagnostics

`namei`, `fuser`, `lslocks`, `pstree`, and `lsblk` are Linux-only bundled
applets. Their procfs, sysfs, mount, and process visibility follows the
current namespace and the caller's permissions; inaccessible processes and
devices are not synthesized.

The compatibility baseline is util-linux 2.41.2 for `namei`, `lslocks`, and
`lsblk`, and psmisc 23.7 for `fuser` and `pstree`. `fuser` keeps process IDs on
stdout and sends labels, verbose rows, and diagnostics to stderr. It supports
the upstream signal and write-filter options; `-k` never signals AXE itself.
`lslocks` emits `{"locks": [...]}` for JSON output. `lsblk` emits
`{"blockdevices": [...]}` and returns 32 when all explicit device operands are
missing, or 64 when only some are missing.

A process chooses its own name and arguments, so `pstree`, `fuser`, `lslocks`,
and the `lsof` command field print `comm`, thread names, and `pstree -a`
arguments in one locale-independent form: printable ASCII as is, `\` doubled,
and every other byte, including UTF-8, as `\ooo`. This matches psmisc
`pstree`. It intentionally differs from psmisc `fuser`, which prints bytes
above 0x7f as sign-extended `\37777777777`, and from util-linux `lslocks` and
`lsof`, which print `\xNN`; `lsof` file names keep the `lsof` escaping.


## Filters and paths

`sed` preserves input and script bytes, including invalid UTF-8, in stream and
in-place operation. `y` is byte-oriented in single-byte locales and
character-oriented in UTF-8 locales; locale encoding comes from the active C
library rather than the locale variable name (musl treats unknown locale names
as UTF-8 when its resolver reports that encoding). In-place output is
published only after successful input and output completion; `-i[SUFFIX]`
accepts clustered options and byte-oriented backup suffixes. `w` targets are
created and truncated once at startup, and `addr1,+N`/`addr1,~N` ranges are
supported. In-place publication preserves ordinary and sticky mode bits but
clears set-ID bits. Script and missing-input errors exit with status 2; runtime
stream or in-place I/O failures exit with status 4.

`awk` preserves bytes in records, string values and file operands. Its program
source must be UTF-8. Record separators, field splitting, assignments between
files and `getline` share the same input lifecycle. `strtonum` does not accept
signed IEEE special-value strings such as `+inf` and `+nan`. Fatal input errors
exit with status 2; stdout write or flush failures exit with status 1.

Field counts and field indices are bounded at 10,000,000 to prevent unbounded
allocation; negative, non-finite, or larger values are fatal runtime errors
with status 2.

`jq` keeps one execution state across input files. `input_filename` is the current
input file, `<stdin>` for standard input, and `null` before input is consumed in
null-input mode. JSON input uses jq-compatible number spellings, invalid UTF-8 is
replaced with U+FFFD, and output is valid JSON. `--tab`, `--indent`, `--seq`,
`--raw-output0`, `--jsonargs`, and `$__loc__` are supported; `halt_error` keeps
string payloads raw and terminates non-string JSON payloads with a newline.
`--stream` and `--stream-errors` are not supported.

`diff` computes raw line edit scripts with Myers; `-d`/`--minimal` selects the
minimal variant. It supports normal, unified (`-u`/`-U`), context (`-c`/`-C`),
ed (`-e`), side-by-side (`-y`), and `--horizon-lines`, ASCII case folding,
whitespace modes (`-i`, `-w`, `-b`, `-E`, `-Z`), `-B`, `-I`,
`--strip-trailing-cr`, labels, directory recursion, and
`--unidirectional-new-file`. Side-by-side widths count bytes in the C locale
and locale display columns in UTF-8 locales. `-x`, `-X`, `-D`, `-F`, `-l`, `-p`,
`-H`, `--color`, and custom line-format options are rejected.
`sdiff` prints side-by-side output and merges interactively with `-o FILE`.
`diff`, `cmp`, and `sdiff` distinguish differences or merge decisions from
input errors. Case folding is ASCII-only. `diff -I` accepts a byte-oriented
BRE subset and rejects unsupported constructs, including backreferences.

NUL-delimited pathname output from `find -print0` and `grep -lZ` preserves Unix
filename bytes. `stat` filename formats and `which` PATH lookup also preserve
those bytes. `which` does not parse shell aliases or functions.

`find -printf` and `-fprintf` emit pathname directives byte-for-byte on Unix,
including invalid UTF-8, and `-regex`/`-iregex` accept byte-oriented operands.
Output write failures report an error and return status 1. `xargs` supports
GNU logical EOF options `-E`, legacy `-e`, and `--eof`; the marker is ignored
with `-0` and `-d`.

`tree -h` prints compact human-readable sizes before names, for example
`[4.9K]  big`. It accepts clustered and repeated flags; `--help` prints help.
`-N` prints pathname bytes as-is; `-q` replaces non-printable characters with
`?` in UTF-8 locales. JSON (`-J`), following directory symlinks (`-l`), and
`--charset` are not supported.

`mkdir -m` preserves all requested permission and special bits, including
sticky, set-user-ID and set-group-ID bits. With `-p`, only newly created
ancestors use the parent policy; the final directory receives the requested
mode.
`hexdump`/`hd` do not implement the custom format languages of `-e` and `-f`.

`file` uses bounded content heuristics, not the complete libmagic database. It
samples at most 64 KiB from a regular file; stdin JSON detection consumes the
complete input. ELF descriptions include the header class, byte order, machine,
link mode, ABI note, build ID, debug information, and symbol-table stripping
when those metadata are available. It supports `-E`, `-N`, `--mime-encoding`,
and `-h`/`--no-dereference`, but not `-F` or `-k`. Empty regular files use
`inode/x-empty`; empty stdin uses `application/x-empty`. Valid JSON scalars are
recognized as JSON. `strings --unicode` is not supported; its JSON output is
an AXE extension.

## Archives and compression

The bundled `zstd` keeps input files by default. It supports compression and
decompression (`-d`), stdout (`-c`), explicit output (`-o FILE`), integrity checks
(`-t`), overwrite (`-f`), input removal (`--rm`), levels 1–19, `--ultra` levels
20–22, `--fast[=N]`, and worker counts (`-T N`; zero selects available CPUs).
Compression includes a checksum; decompression accepts concatenated frames.
Successful `-t` reports the decoded byte count on stderr unless quiet.

File-to-file compression and decompression use private temporary output and
publish it only after the codec and temporary-file finalization succeed. A failed
decode retains an existing destination. This does not protect a file truncated
by shell stdout redirection. ACL, extended-attribute and ownership preservation
are not promised.
After a failed decode, the amount of partial stdout can differ from the original
codec; always check the exit status.

`gzip -dcf` copies non-gzip input to stdout. Force mode does not rename plain
files during file-to-file decompression, and `-l` still requires gzip data.
`-S SUF`/`--suffix=SUF` selects an additional compressed-file suffix (and the
suffix used for compression); `-r`/`--recursive` processes directory trees for
gzip and XZ. Empty suffixes and suffixes containing `/` are rejected.
Concatenated members are decoded in order; trailing non-gzip bytes produce status
2 unless force-to-stdout mode copies them. XZ plaintext passthrough requires
explicit stdout mode (`-c` or a cat alias), not merely stdin's default output.
Bzip2 forced plaintext stdin is preserved even where the reference emits none.
File-to-file outputs preserve source ownership when the operating system permits
it; ACL and extended-attribute preservation are not promised. Unsupported
options include bzip2 `-s`/`-T`, XZ `-e`/`--check`/`--single-stream`, and
Zstd `-l`; these are rejected, not silently ignored.

`tar` accepts old-style clusters such as `tar cfz archive.tgz directory`;
argument-taking options consume following words, and `-C` is positional.
`-T FILE`/`--files-from=FILE` adds byte-exact names from a newline-delimited list;
`--null` switches lists to NUL delimiters. List entries may include `-C DIR`
or `--directory=DIR` directives, and files-from operands combine with positional
members and `--exclude`.
Listings stream as entries are read. `--strip-components=N` affects extraction,
not listing. Extraction checks member paths, parent directories and hardlink
targets against the destination. Missing requested members exit with status 2.
Independent entries continue after a `--keep-old-files` collision; existing
directories retain their metadata in this mode.

Tar creation uses GNU headers; reading accepts GNU and PAX metadata. `-a`, `-h`,
`--wildcards`, `--zstd`, and `-m`/`--touch` are supported. `--format`, including
PAX creation, is not supported. Globs operate on bytes with ASCII character
classes, not locale collation.

## Storage reports

`df` supports `-m`, `--output`, and `--type=TYPE`. Without `-a`, it selects one representative mount for each device, including bind aliases. Block-size options follow command-line order.

`blkid -l -t NAME=value` prints the first match; `-L LABEL` and `-U UUID` print
only the matching device path. Tokens are case-sensitive. `-p` includes
filesystem version metadata; an ambiguous low-level probe exits with status 8.
LUKS1 detection requires a complete probe-readable header and rejects
truncated images.
Partition-table metadata is separate from filesystem type detection. A matching
magic number alone does not identify a truncated or structurally invalid image.
These are identity probes, not complete filesystem consistency checks.

`blockdev --report` accepts an optional device list. With no devices, `--report` must be the first argument; it reads `/proc/partitions`, skips zero-capacity entries, and continues quietly after device errors. An empty list succeeds; any device failure makes the report unsuccessful.

`mountpoint PATH` exits with status 0 for a mountpoint, 32 for an existing
non-mountpoint, and 1 when the path cannot be resolved. Linux mount detection
uses `/proc/self/mountinfo`, including bind mounts on the same filesystem.

`inotifywait` and `inotifywatch` use inotify. `fsnotifywait` and
`fsnotifywatch` use fanotify and support `-S`/`--filesystem` and `-M`/`--mount`
scopes. All four are nonrecursive unless `-r` is requested. `@PATH` operands
exclude paths. Recursive monitoring installs watches or marks on new
directories using internal events without emitting or counting those events
unless requested. Inaccessible or vanished subdirectories are skipped during
recursive setup. Requested event filters also apply to unmount events where the
backend provides them.

## Mounting filesystems

The bundled `mount` follows util-linux `mount(8)`. Without arguments it lists mounts; with `-t`/`-o` it mounts a filesystem. It also supports remount, `--bind`/`--rbind`, `--move`, `--make-*` propagation, single-operand lookup in `/etc/fstab`, `-a` with `-t` and `-O` filters, and `/sbin/mount.<type>` helpers.

A remount changes only the flags you name. Other flags, such as `nosuid`, `nodev`, and the atime mode, keep their current values.

`umount` accepts mountpoints or sources, including explicit `--target` and `--source` operands. `-A`/`--all-targets` requires an operand and selects every mountpoint of that filesystem. `-R` follows the mount-ID graph, including overmount stacks; a failure stops that chain while independent operands are still attempted. `-a` preserves `/` and excludes `proc`, `devfs`, `devpts`, `sysfs`, `rpc_pipefs`, `nfsd`, and `selinuxfs`; `-t` replaces those exclusions. Type (`-t`) and option (`-O`) filters apply to `-a`, not explicit targets.

Unmount options include force (`-f`), lazy detach (`-l`), read-only fallback (`-r`), fake execution (`--fake`), disabled canonicalization (`-c`), disabled helpers (`-i`), quiet missing-target errors (`-q`), and a mount namespace (`-N PID|PATH`). `--fake` performs no unmount or read-only remount. Verbose messages (`-v`) go to stderr.

When the filesystem rejects an option, `mount` prints the kernel's message, for example `fsconfig() failed: tmpfs: Unknown parameter 'size2'`. On kernels without the `fsopen` API it falls back to `mount(2)` and prints a generic error.

Image files mount through loop devices. `mount` attaches one for `-o loop`, `loop=/dev/loopN`, `offset=`, or `sizelimit=`, and for any regular file mounted as a block filesystem. The device detaches after the last unmount. If the same file and range is already attached, `mount` reuses that device. It refuses an overlapping range, and it attaches a write-protected file read-only.

Without an explicit filesystem type, `mount` rejects ambiguous filesystem
signatures with status 32 before attempting filesystem mounts or helpers.
Genuinely unidentified sources retain the kernel-filesystem fallback; `-t`
remains an explicit type selection.

For `mount`, `-o encryption`, `-N`, `-T`, and `-F` are not supported.

## Process and network compatibility

The bundled `ip` renders tunnel link addresses as IPv4 or IPv6 addresses, matching iproute2 in text and JSON output. Ethernet and other hardware addresses use colon-separated hexadecimal bytes.

`ip` provides read-only inspection with text and JSON output. It accepts
`-j`/`-json`, pretty JSON (`-p`), brief (`-br`/`-brief`), oneline (`-o`/`-oneline`),
and numeric names (`-N`). `route get` accepts `from`, `mark`, `uid`, `dev` and
`oif` selectors without loading a full route dump; `iif` is not supported.
Mutation verbs, detail/resolve options and unsupported address families are
rejected. `arp` displays the kernel's `/proc/net/arp` table with net-tools ATF flags.

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

`RSS` is in KiB; `%MEM` truncates to one decimal place. `%CPU` is a lifetime average, not recent utilization: it truncates to one decimal below 100%, then prints whole percentages without capping multicore usage. Use interval samples from `top` for recent CPU activity.

Command text supports C and UTF-8 locales. Newlines in command-line arguments become spaces; other control characters become `?`, including terminal escape sequences. Legacy non-UTF-8 character sets are not emulated.

`killall` sends a signal to every process with an exact Linux kernel command name (`comm`, limited to 15 bytes), not a command-line substring or regular expression. The default signal is `TERM`. Supported options are `-s SIGNAL`, `-SIGNAL`, `-NUM`, `-I` for case-insensitive names, `-u USER` for a real-UID filter, `-l` to list signals, `-q`, and `-v`. No match, failed signal delivery or an unknown option exits with status 1; missing names and invalid signals exit with status 2. It does not support signal 0, full-length executable-name matching, process-group signaling, interactive confirmation, waiting, or the regex, age, namespace, and SELinux filters from psmisc `killall`.

`free` reports used memory as `MemTotal - MemAvailable`, with the legacy
free/buffers/cache calculation on kernels without `MemAvailable`. Its columns
use procps-style headings and widths.

`vmstat` uses memory values from `/proc/meminfo`, including reclaimable slab in
cache, and scales swap rates to the selected unit. Its first context-switch rate
uses elapsed uptime; it does not reproduce procps 4.0.7's CPU-tick divisor.
`top` reports processes, not a `-H` thread view, and does not implement `-o`
ordering. Its first CPU display is cumulative; later samples use interval deltas.
Its `RES` and `%MEM` use resident KiB from `/proc/PID/status`; `SHR` is
`RssFile + RssShmem` in KiB, without another page-size conversion.
`hugetop` has an AXE interface: `-b` selects batch mode and `-n COUNT`
sets the sample count; it does not emulate procps NUMA, once or human-size modes.
`tload` uses the current terminal, not a TTY pathname operand.

`lsof` inspects visible `/proc` cwd, root, executable, memory-mapped file and
descriptor entries. Like lsof, it lists each mapped file once per device and
inode, skips the executable, reports a deleted mapped file as `DEL`, and appends
`(stat: ...)` or `(path dev=...,inode=...)` to the name when the path no longer
names the mapped file; unlike lsof, a mapped path keeps embedded whitespace.
`-a` intersects requested selection classes; otherwise they are unioned. It
supports
the `-F` field subset `p`, `c`, `f`, `n`, `t`, `u`, `L`, `i`, `D`, `s`, `P`,
`a`, `o`, `r` and the NUL terminator selector `0`; other fields are rejected.

`iostat` uses uptime for the first interval and interval deltas thereafter.
Extended `-x` output is limited to device, `await`, `aqu-sz` and `%util` fields;
`-o JSON` is not supported. `lspci` uses visible sysfs entries, not `/proc`
fallback or PCI configuration-space reads; `-mm` and full verbose capabilities
are not supported. `lsscsi` does not implement host, address-filter, NVMe or
transport modes. `ipcs -b` and `--human` are not supported.

`ping` reports reply TTL/hop-limit data from received packets. `traceroute`
matches actual replies and error-queue messages to its probes; unresponsive
hops remain `*`. `host` and `nslookup` retain the responding DNS server's
provenance and retry truncated UDP replies over TCP.

`ipcalc` supports numeric IPv4/IPv6 information and validation, not DNS, GeoIP,
random-network generation, splitting or deaggregation.

## Compatibility boundaries

Bundled applets are not a universal GNU/Linux drop-in replacement. Supported
standard fields and layouts matter even when their numeric data agrees.
Help/version identity, free-form diagnostics, vendor databases, terminal
wrapping and generic CLI-error statuses can differ. In particular, awk/sed
generic usage failures use status 2; tar CLI failures use 2 rather than GNU 64.
The Linux musl artifact reports `Linux` for `uname -o`, not `GNU/Linux`.

`logname` does not use `LOGNAME` or `USER` as login identity. Linux musl builds
resolve the audit UID from `/proc/self/loginuid` through the passwd database;
missing, unset or unresolved login identity exits with status 1. Other targets
use the native login lookup.

`printf` floating-point conversion follows the target C `long double` ABI,
including its precision and range; it is not arbitrary-precision decimal
arithmetic. `more` copies input without a terminal; its interactive layout and
`stty -a` wrapping are not exact replicas of the reference.
`stty -g` preserves active control characters and independent Linux baud rates.
Linux `stty` accepts arbitrary decimal speeds through termios2/BOTHER, and
recognizes the GNU `exta`/`extb` aliases. Other targets accept their standard
baud-rate names and reject unsupported numeric rates.

Safe divergences are retained: compression preserves existing output after a
failed decode, `strings` reports stdout failures, and valid JSON scalars remain
JSON even where libmagic chooses text. Custom commands such as HTTP, Vzik and
Goblin retain their own interfaces. Goblin's JSON path display is not a lossless
Unix pathname transport; see its [output schema](../skills/axe/references/goblin-output-schema.md).
