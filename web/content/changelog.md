+++
title = "Changelog"
+++

## [v0.16.1](https://github.com/buglloc/axe/releases/tag/v0.16.1) — 2026-11-08

### Changes

- The release workflow now commits and pushes generated release metadata automatically after publication and can resume an interrupted metadata push. ([bcf6f2d](https://github.com/buglloc/axe/commit/bcf6f2d6688bc9b5571c52da3ff91524ad9c20cd))

## [v0.16.0](https://github.com/buglloc/axe/releases/tag/v0.16.0) — 2026-11-08

### Highlights

- Added `ast-grep` 0.51.0 to the AXE Store for syntax-aware source-code search, linting, and rewriting on supported targets. ([5f2be1b](https://github.com/buglloc/axe/commit/5f2be1beac075e6f40ef60a9bca4ce2758088afc))

### Changes

- Improved `axe-relay` error messages with API endpoints, URLs, request context, and relay bind configuration details. ([38210e9](https://github.com/buglloc/axe/commit/38210e93ac668173e9093e7785f5b65bcf3df35d))

## [v0.15.1](https://github.com/buglloc/axe/releases/tag/v0.15.1) — 2026-11-07

### Changes

- `axe-relay` now accepts its API endpoint through `AXE_RELAY_API_URL`; without this variable, it continues to use `http://127.0.0.1:7000`. ([d389dd2](https://github.com/buglloc/axe/commit/d389dd277c0729a233e0d4e84c3e089ca6432b12))

## [v0.15.0](https://github.com/buglloc/axe/releases/tag/v0.15.0) — 2026-11-07

### Highlights

- Relay API endpoints now require a matching `Authorization: Bearer <token>` header; unauthorized requests return HTTP 401. ([266787c](https://github.com/buglloc/axe/commit/266787c4463abb0951ff908775943b6640cd4fc0))
- The relay dashboard now accepts a bearer token and uses it for API requests. ([266787c](https://github.com/buglloc/axe/commit/266787c4463abb0951ff908775943b6640cd4fc0))

### Changes

- The dashboard/API can bind beyond loopback, with bearer-token protection for API access. Relay API clients accept HTTP and HTTPS origins. ([266787c](https://github.com/buglloc/axe/commit/266787c4463abb0951ff908775943b6640cd4fc0))

## [v0.14.0](https://github.com/buglloc/axe/releases/tag/v0.14.0) — 2026-11-06

### Highlights

- Interactive AXE shells now use the `[axe] \u@\H:\w\n\$ ` prompt and start without user Bash or Brush startup files. ([e8ad3d0](https://github.com/buglloc/axe/commit/e8ad3d04f7ba2490c370f2562fb400a581b724ca))
- Added `gix` 0.59.0 to the AXE Store for `aarch64-darwin`, `aarch64-linux`, and `x86_64-linux`. ([d1ffedc](https://github.com/buglloc/axe/commit/d1ffedc899664f0d9440902f941ea3cda57948e6))

### Changes

- SSH-launched shell sessions no longer pass `--norc`; they continue to pass `--noprofile` and `--no-config`. ([e8ad3d0](https://github.com/buglloc/axe/commit/e8ad3d04f7ba2490c370f2562fb400a581b724ca))

## [v0.13.1](https://github.com/buglloc/axe/releases/tag/v0.13.1) — 2026-11-04

### Highlights

- Updated the AXE Store package set. The refreshed packages include `bpftool` 7.8.1, `dig` 9.20.30, `docker` 29.9.2, `gori` 0.9.1, `podman` 6.1.4, Python 3.14.10-web.1, `strace` 7.3, and `xh` 0.26.3. ([53476fc](https://github.com/buglloc/axe/commit/53476fc7ca72bb116f2a9f0520bcec50be5c9317))
- Improved `xh` portability by rewriting build-configuration paths and enabling its Rustls ring provider. ([53476fc](https://github.com/buglloc/axe/commit/53476fc7ca72bb116f2a9f0520bcec50be5c9317), [1fb23b5](https://github.com/buglloc/axe/commit/1fb23b540193c6a6ba567609e1a9a1e865cde3e9))

### Changes

- Refreshed Store bootstrap metadata for the updated package versions. ([53476fc](https://github.com/buglloc/axe/commit/53476fc7ca72bb116f2a9f0520bcec50be5c9317))
- Updated the `gori` package to Crystal 1.22.2 and refreshed its pinned Crystal shard dependencies. ([53476fc](https://github.com/buglloc/axe/commit/53476fc7ca72bb116f2a9f0520bcec50be5c9317))
- Updated the Caddy packaging patch to restore the local embedded-CA implementation and copy the bundled AXE CA bundle during configuration. ([53476fc](https://github.com/buglloc/axe/commit/53476fc7ca72bb116f2a9f0520bcec50be5c9317))

### Fixes

- Fixed the `xh` build. ([a359ea2](https://github.com/buglloc/axe/commit/a359ea2fa83d04a1233c0f9f92a87b7e81b20d33))
- Fixed portable `xh` binary paths. ([1fb23b5](https://github.com/buglloc/axe/commit/1fb23b540193c6a6ba567609e1a9a1e865cde3e9))

## [v0.13.0](https://github.com/buglloc/axe/releases/tag/v0.13.0) — 2026-11-01

### Highlights

- Vzik now uses protocol v5. Valid UTF-8 byte fields are emitted as JSON strings; invalid byte fields use `{display, base64}` so their original bytes remain available. Vzik JSON output includes `schema` and `schema_version`. ([7957f8f](https://github.com/buglloc/axe/commit/7957f8f618caefa46f1e4b89aeb85a2552e2e001))
- Added modular Vzik monitor commands for files, processes, and sockets. ([aa5e4c3](https://github.com/buglloc/axe/commit/aa5e4c33e5daf31013660a8bc0c683b275df111a))
- Added `tracee` to the AXE Store package set. ([cf4e8c0](https://github.com/buglloc/axe/commit/cf4e8c0b05256977a650713708a7bffd0ec75dd1))
- Moved `ip` from the bundled command set to the AXE Store. ([108b3d4](https://github.com/buglloc/axe/commit/108b3d4e3cf5863a7bdab2c55589fb30c7df3918))

### Changes

- Vzik diagnostics no longer include the redundant `status` field. Source failure codes no longer use the `source_` prefix, and process-source availability uses the same codes. The collect profile is now named `baseline`. ([7957f8f](https://github.com/buglloc/axe/commit/7957f8f618caefa46f1e4b89aeb85a2552e2e001))
- Monitor records now include `seq` and use `provider` instead of `source`. `stream_start` records use `classes`; `completion` and `checkpoint_sequence` were removed. Each inotify setup gap is emitted once. ([7957f8f](https://github.com/buglloc/axe/commit/7957f8f618caefa46f1e4b89aeb85a2552e2e001))
- `axe doctor` now emits schema v5 for cgroup path encoding. ([7957f8f](https://github.com/buglloc/axe/commit/7957f8f618caefa46f1e4b89aeb85a2552e2e001))
- Native bootstrap loader bytes are embedded in `bootstrap.json`. ([d5aa376](https://github.com/buglloc/axe/commit/d5aa376b3dbcf244361928b5514d4fa4785152eb))
- Website bootstrap instructions now separate curl or wget commands, pasted scripts, and the Native ELF loader, and show command lengths. ([939937a](https://github.com/buglloc/axe/commit/939937ad977cd5ab3b16b60c989b993a46c0dbf1))
- The website now prioritizes x86_64 in its bootstrap presentation. ([bc41482](https://github.com/buglloc/axe/commit/bc41482e988a92f9c26cb8025ff8771478abd4d2))

### Fixes

- Corrected the README quick-start stable download command. ([6d1fa4c](https://github.com/buglloc/axe/commit/6d1fa4c79ec2637765ea0c2a0093ba14802a2178))
- Corrected the documented Native bootstrap ELF layout. ([660c607](https://github.com/buglloc/axe/commit/660c60772345dff8d93155f4cf4170b5a5fa5a84))

## [v0.12.0](https://github.com/buglloc/axe/releases/tag/v0.12.0) — 2026-10-29

### Highlights

- Added bootstrap loaders for hosts without AXE: universal Shell, Python, Node.js, and Perl scripts, plus freestanding Native ELF loaders for Linux x86_64 and aarch64. Each loader downloads and runs the corresponding static `axe` release. ([956b142](https://github.com/buglloc/axe/commit/956b142e1f9552e7a0209609b8ad8da349d78bb2))
- Bootstrap loaders connect to pinned Store addresses, prefer IPv6 without DNS, verify the expected payload size and digest, and do not execute a mismatching download. Scripts use SHA-256; Native ELF loaders use Ascon-Hash256. ([956b142](https://github.com/buglloc/axe/commit/956b142e1f9552e7a0209609b8ad8da349d78bb2))

### Changes

- Added release assets under immutable `releases/v<VERSION>/b/` keys with stable `b/` aliases: Shell, Python, Node.js, Perl, and per-target Native ELF loaders. ([956b142](https://github.com/buglloc/axe/commit/956b142e1f9552e7a0209609b8ad8da349d78bb2))
- Added configurable bootstrap `default_args` for invocations without arguments. ([956b142](https://github.com/buglloc/axe/commit/956b142e1f9552e7a0209609b8ad8da349d78bb2))
- Added bootstrap commands to the website, with target and runtime selection and displayed artifact sizes. ([956b142](https://github.com/buglloc/axe/commit/956b142e1f9552e7a0209609b8ad8da349d78bb2), [040b65a](https://github.com/buglloc/axe/commit/040b65a59d78206670bfd89a99cc5ca7523b1f52))
- The website command list moved to `/tools/`. It shows command aliases, links to each command, and keeps search and filters in the page URL. ([8a688c0](https://github.com/buglloc/axe/commit/8a688c03c03d11c00d7e3b2c30c0879e093bb553), [040b65a](https://github.com/buglloc/axe/commit/040b65a59d78206670bfd89a99cc5ca7523b1f52))
- The release publisher now verifies that the plain-HTTP Native payload endpoint serves the object directly without redirecting to HTTPS. ([956b142](https://github.com/buglloc/axe/commit/956b142e1f9552e7a0209609b8ad8da349d78bb2))
- Python cache directories are now ignored by version control. ([34bba4e](https://github.com/buglloc/axe/commit/34bba4e2e3d6e07c1e0afe2878d135689368202a))

### Fixes

- `axe NAME` now names the applet in diagnostics and `--version` output instead of `axe` or the full executable path. For example, `axe cat missing` reports `cat: missing: No such file or directory`, and `axe dmesg --version` prints `dmesg 0.0.1`. ([029cef6](https://github.com/buglloc/axe/commit/029cef60a2caf5a09e653b199842e32882ff3a93))

## [v0.11.0](https://github.com/buglloc/axe/releases/tag/v0.11.0) — 2026-10-28

### Highlights

- `lsof` now lists file-backed memory mappings once per device and inode, reports deleted mappings, preserves embedded whitespace in mapped paths, and supports selecting mapped files by device and inode. ([8a18722](https://github.com/buglloc/axe/commit/8a18722df48734cc5a15db64c91ef5c750b4e8a9))
- AXE Store adds the `fsmon` package. ([8846059](https://github.com/buglloc/axe/commit/8846059a374e1cf60decf2b009cc89630db837d7))
- Vzik expands process inspection and exposes socket-owner file descriptors. ([dbca256](https://github.com/buglloc/axe/commit/dbca25687315f7e53fb13bd0b1619a0dd9dea607), [143e024](https://github.com/buglloc/axe/commit/143e0243d09bea2a0e699aaeac6cdaabfa3c9f3f))

### Changes

- Linux process and mount introspection now shares byte-oriented parsing primitives through `vzik::procfs`. ([f3574d4](https://github.com/buglloc/axe/commit/f3574d4ec48f19ee5e16d4dc6b68489e6f8c6c29))
- `pstree`, `fuser`, `lslocks`, `lsblk`, and `lsof` use locale-independent escaping for process, thread, argument, and mount-point bytes. ([f3574d4](https://github.com/buglloc/axe/commit/f3574d4ec48f19ee5e16d4dc6b68489e6f8c6c29))
- `lsof -F` now prints the access field for every file set, including a space when no access mode is present. ([8a18722](https://github.com/buglloc/axe/commit/8a18722df48734cc5a15db64c91ef5c750b4e8a9))
- SSH session environment handling was normalized, and OpenSSH client defaults for AXE Store were patched. ([b117621](https://github.com/buglloc/axe/commit/b117621df60c5a5c6df5b0104f8341f480a4f098), [e590813](https://github.com/buglloc/axe/commit/e5908130027806f6f23089b30f173bcfec8c9e21))
- The OpenSSH client `LogLevel` override was removed. ([47f56cd](https://github.com/buglloc/axe/commit/47f56cde9446124472b3ca84970d0b3273905b43))
- The Store bootstrap index was refreshed. ([a3bc1b2](https://github.com/buglloc/axe/commit/a3bc1b2bd94ae7f99ab56731011750df7fc9f0c9))
- Valkey tests were disabled. ([c6b3954](https://github.com/buglloc/axe/commit/c6b39549a47871bcf6f26c8fca7c67f0c3f8968c))

### Fixes

- `pstree` no longer drops processes and their subtrees when process names are not UTF-8. `lsof` no longer reports UID 0 for such processes. ([f3574d4](https://github.com/buglloc/axe/commit/f3574d4ec48f19ee5e16d4dc6b68489e6f8c6c29))
- `fuser -m`, `lslocks`, and `lsblk` now match non-ASCII mount points using byte-oriented mountinfo parsing. ([f3574d4](https://github.com/buglloc/axe/commit/f3574d4ec48f19ee5e16d4dc6b68489e6f8c6c29))
- Executable memory mappings are skipped only when their device and inode identify the process executable. ([6daa8a6](https://github.com/buglloc/axe/commit/6daa8a6f4792316e8a7114b2ee973a4d0f7495ad))

## [v0.10.0](https://github.com/buglloc/axe/releases/tag/v0.10.0) — 2026-10-27

### Highlights

- Added five bundled Linux diagnostic applets: `namei`, `fuser`, `lslocks`, `pstree`, and `lsblk`. ([7549d69](https://github.com/buglloc/axe/commit/7549d6933f5bc56bc3ff1664b5c5bb0d2341e688))

### Changes

- Added `namei` support for resolving pathname components, displaying symbolic links, and reporting modes, owners, mount points, and resolution errors. ([7549d69](https://github.com/buglloc/axe/commit/7549d6933f5bc56bc3ff1664b5c5bb0d2341e688))
- Added `fuser` support for identifying processes using files, filesystems, and TCP or UDP sockets, with user, verbose, namespace, filtering, and signal options. ([7549d69](https://github.com/buglloc/axe/commit/7549d6933f5bc56bc3ff1664b5c5bb0d2341e688))
- Added `lslocks` support for listing locks from `/proc/locks` with configurable columns, PID filtering, raw and byte output, and JSON output. ([7549d69](https://github.com/buglloc/axe/commit/7549d6933f5bc56bc3ff1664b5c5bb0d2341e688))
- Added `pstree` support for displaying the process hierarchy from `/proc`, including PID or user selection, arguments, process groups, threads, namespace information, UID changes, and security contexts. ([7549d69](https://github.com/buglloc/axe/commit/7549d6933f5bc56bc3ff1664b5c5bb0d2341e688))
- Added `lsblk` support for discovering Linux block devices from sysfs, showing device relationships, mount points, filesystem information, topology, discard data, and JSON or key-value output. ([7549d69](https://github.com/buglloc/axe/commit/7549d6933f5bc56bc3ff1664b5c5bb0d2341e688))

## [v0.9.1](https://github.com/buglloc/axe/releases/tag/v0.9.1) — 2026-10-27

### Highlights

- Added bundled `fsnotifywait` and `fsnotifywatch` applets for waiting on filesystem events and collecting fanotify event statistics. ([49051c4](https://github.com/buglloc/axe/commit/49051c485f779cfd4dc886a95467794896aa167b))
- Added fanotify selection and scope options to `inotifywait` and `inotifywatch`, including `--fanotify`, `--inotify`, `--filesystem`, and `--mount`. ([49051c4](https://github.com/buglloc/axe/commit/49051c485f779cfd4dc886a95467794896aa167b))

### Changes

- `file` now reports more ELF details, including class, byte order, file type, machine, version, ABI, linkage, interpreter, build ID, debug information, and stripped status when available. ([a325a23](https://github.com/buglloc/axe/commit/a325a23701ba82d39e10e71292f3316155853c28))
- Recursive inotify setup now skips inaccessible subtrees while continuing to watch accessible paths. ([49051c4](https://github.com/buglloc/axe/commit/49051c485f779cfd4dc886a95467794896aa167b))
- Refreshed the AXE Store bootstrap index. ([269096b](https://github.com/buglloc/axe/commit/269096b8ff174868e1f0990d39e1c492165843e1))

### Fixes

- `grep` now enables line buffering when its output is a terminal, unless `--line-buffered` was already specified, so matching output is available before the process exits. ([13544bd](https://github.com/buglloc/axe/commit/13544bd768af8ca45d2ba4921d69488111c5133f))
- `ox` no longer starts the configuration assistant solely because the user has no configuration file. The assistant runs only with `--config-assist`, and bootstrap now tolerates an unset `HOME`. ([4b6beb0](https://github.com/buglloc/axe/commit/4b6beb0da8f169ea3fed35be7afb31378aefbf5e))

## [v0.9.0](https://github.com/buglloc/axe/releases/tag/v0.9.0) — 2026-10-26

### Highlights

- Added `usql` to the AXE Store for `aarch64-darwin`, `aarch64-linux`, and `x86_64-linux`. It supports PostgreSQL, MySQL, SQLite, and ClickHouse databases. ([2046208](https://github.com/buglloc/axe/commit/204620842df00e584b455610db53a09588ffd572))
- Added `valkey-cli` to the AXE Store for `aarch64-linux` and `x86_64-linux`. It connects to Valkey and Redis-compatible servers with TLS support. ([2046208](https://github.com/buglloc/axe/commit/204620842df00e584b455610db53a09588ffd572))

### Changes

- `usql` embeds the AXE CA bundle and uses it for PostgreSQL connections with `sslmode=verify-full` when no explicit root certificate is provided. ([2046208](https://github.com/buglloc/axe/commit/204620842df00e584b455610db53a09588ffd572))
- `valkey-cli` embeds the AXE CA bundle and uses it for TLS certificate verification when no external CA path is configured. ([2046208](https://github.com/buglloc/axe/commit/204620842df00e584b455610db53a09588ffd572))
- Refreshed the signed Store bootstrap index and inventory to include `usql` version `0.21.6` and `valkey-cli` version `9.1.2`. ([7486bf1](https://github.com/buglloc/axe/commit/7486bf188f3583188a75e3720fa2722989367c84))

## [v0.8.1](https://github.com/buglloc/axe/releases/tag/v0.8.1) — 2026-10-26

### Highlights

- Added `less` to the AXE Store for `aarch64-linux` and `x86_64-linux`. It pages through files and command output. ([dc312c4](https://github.com/buglloc/axe/commit/dc312c4675db6dad8c85fc528af02980ac72ed26))

### Changes

- Built the Store `less` package with static ncurses and compiled-in descriptions for common terminals, including Kitty and Ghostty, so it does not depend solely on a host terminfo database. ([dc312c4](https://github.com/buglloc/axe/commit/dc312c4675db6dad8c85fc528af02980ac72ed26))
- Refreshed the signed Store bootstrap index and inventory to include `less` version `710`. ([59ec18f](https://github.com/buglloc/axe/commit/59ec18fd95773eaf107529cf12d81224ef3f8658))

## [v0.8.0](https://github.com/buglloc/axe/releases/tag/v0.8.0) — 2026-10-25

### Highlights

- Added bundled `sdiff` with side-by-side comparison, merge mode, interactive conflict selection, and atomic output publication. ([41ccfcc](https://github.com/buglloc/axe/commit/41ccfcc009c4b45c3f34ab57b4c346ddacdefd8e))
- Improved `diff` compatibility with GNU context horizons, `--horizon-lines`, shifted hunk boundaries, missing-final-newline handling, and byte- or display-width-aware side-by-side output. ([d500cde](https://github.com/buglloc/axe/commit/d500cdef41302380584ccbfbeef3c1e499e9eac1))
- Added the portable `difft` Store package based on difftastic. ([22ba6b2](https://github.com/buglloc/axe/commit/22ba6b29cd7adfaef7c8f79064b68cc4ce5786f6))
- Updated all vendored Brush crates to upstream revision `e37b3c91`, including fixes for builtin help output, empty-array expansion, and globbing after empty quotes. ([741c1f4](https://github.com/buglloc/axe/commit/741c1f482956058f50bce2817686ac5a49ad4b40))
- Added portable Store packages for Vim, asciinema, micro, ox, broot, and spf. ([a72afa7](https://github.com/buglloc/axe/commit/a72afa76188f832dd0d47382a865169c99cabe83), [ce45980](https://github.com/buglloc/axe/commit/ce45980e8edaf6eba7aef9568b33384267ad079b), [9a17438](https://github.com/buglloc/axe/commit/9a17438f663f76fc6cfff1dd74c66d1c157e6752), [89229ea](https://github.com/buglloc/axe/commit/89229eabd540f4f46392aab2938be255f59a0d21))

### Changes

- Removed the bundled `diff3` applet because its generic merge implementation misclassified conflicts and could panic on valid input. ([4bed2f4](https://github.com/buglloc/axe/commit/4bed2f40d8092427bbdbcccc354fc142686d27ca))
- `diff` now supports `-E`, `-Z`, `--strip-trailing-cr`, `-d`/`--minimal`, `--unidirectional-new-file`, `-T`, `--left-column`, and `--suppress-common-lines`; its edit scripts use gix-imara-diff with raw-byte line tokenization. ([10e3b08](https://github.com/buglloc/axe/commit/10e3b08e14f9ca141ece7ccce88d344334bda6fb))
- `tar` now accepts byte-oriented `--files-from` lists, including `-T -`, `--null`, non-UTF-8 names, and in-list `-C`/`--directory` directives. ([548cf28](https://github.com/buglloc/axe/commit/548cf28f6f5a422f9b0ffb3fdada442ce57f56c6))
- `gzip` and XZ compression now preserve source ownership where permitted, clean up staging files after termination signals, support `-S`/`--suffix`, and recursively process directories with `-r`. ([cf7214d](https://github.com/buglloc/axe/commit/cf7214d368bfac119f51377f42fa26b9876eea7b))
- `awk` now skips missing `ARGV` entries and bounds dynamically sized fields and field indices, returning a runtime error instead of hanging or panicking on extreme values. ([b16904f](https://github.com/buglloc/axe/commit/b16904f92ffc16cfffae4df3136f873a37c56c63))
- `jq` input handling now preserves a lazy byte stream across files and stdin, validates JSON boundaries, canonicalizes numbers, and aligns `halt_error` status and payload behavior with jq. ([3258b2b](https://github.com/buglloc/axe/commit/3258b2be38e6fb1d32d394f0f0b9f3cba894c264))
- `sed` now keeps scripts, operands, patterns, replacements, transliteration tables, labels, and filenames byte-oriented; resolves transliteration behavior from the active locale; and opens `w`/`W` and substitution output files at startup. ([debc6ee](https://github.com/buglloc/axe/commit/debc6ee942bfb65c6f8a24e549e2f07d3996a11e))
- `xargs` now supports GNU logical EOF options `-E`, legacy `-e`, and `--eof`, including raw markers and the documented record-boundary behavior. ([c89784d](https://github.com/buglloc/axe/commit/c89784d302904d9bf4c0faebb918306850b535d8))
- The release adds a vendored provenance audit and a `vendor-test` target covering vendored packages against the locked workspace dependency graph. ([2aa9fbb](https://github.com/buglloc/axe/commit/2aa9fbb5a11aab9c851bfbb68786eeeec1530ff3))

### Fixes

- AXE now preserves inherited SIGPIPE behavior across standalone applets, shell builtins, external children, and bundled self-exec children; in-process broken-pipe failures map to shell status 141 without an extra diagnostic. ([5036f04](https://github.com/buglloc/axe/commit/5036f0478e64d6bd6f6affaac60239e10a5dd7b1))
- `cmp` now reports stdout write failures with status 2 and flushes its `-l` output before returning. ([ffa7e9e](https://github.com/buglloc/axe/commit/ffa7e9e8727da6b0bb2aa11ce5a23c4365e686af))
- `mkdir` restores all special mode bits through a pinned parent-relative descriptor, avoiding pathname substitution races and preserving the sticky bit. ([914aba9](https://github.com/buglloc/axe/commit/914aba98440ac18c3a3005420c61ca4edc21ab5f))
- `blkid` now rejects truncated LUKS1 probes and reports swap, LUKS, exFAT, GPT, MBR, FAT12, and partition-table identifiers according to the documented compatibility behavior. ([678fa26](https://github.com/buglloc/axe/commit/678fa26cab0434c43da27d5b61c4e91a192e1dd9), [7d785f1](https://github.com/buglloc/axe/commit/7d785f1690afb738939574620920ebbb2f0ce261))
- `stty` now accepts arbitrary Linux `termios2` speeds through BOTHER while retaining standard names and aliases. ([a995af5](https://github.com/buglloc/axe/commit/a995af5a70aae630c604470bbb37d2ac73cf3d2f))
- `find` preserves raw Unix pathname bytes in formatters and regular-expression operands, and propagates output write failures instead of panicking. ([899ad63](https://github.com/buglloc/axe/commit/899ad633700a7217e5955f91e88a1f69a0cfa979))
- `tree` now supports raw-name and question-mark modes, locale-specific escaping, and `-N`; its `-h` behavior is also aligned with GNU tree. ([30fd83b](https://github.com/buglloc/axe/commit/30fd83be49feb62259f98d70a482e1ae59d7fafd), [237f3bd](https://github.com/buglloc/axe/commit/237f3bd7cde2dabdeeb3cb5a809975b9082ab7ba))
- `ps`, `ip`, `arp`, and `ifconfig` received compatibility fixes for field widths, route and address rendering, option bundling, missing-device errors, and broken-pipe handling. ([c404ed3](https://github.com/buglloc/axe/commit/c404ed3ff25cc3531c1b3277607f7a588033b9ae), [d82cb06](https://github.com/buglloc/axe/commit/d82cb0683f37c57cddbcae7cc27731b6ae3edcdb), [2228565](https://github.com/buglloc/axe/commit/2228565a05765577a107c0944de4c9083324c0f0))
- `umount` now supports recursive and filtered unmount selection, lazy and forced modes, read-only remount on busy targets, and verbose reporting. ([b1492ea](https://github.com/buglloc/axe/commit/b1492ead8cca02676ebd7d1cf7cc3262942b5e4b))
- `ping` now includes mean deviation in its `rtt min/avg/max/mdev` summary. ([de48a9d](https://github.com/buglloc/axe/commit/de48a9d3290fa6c21c2a93d8366675d915b14865))
- `gzip -f` and `zcat -f` now pass non-gzip input through unchanged with status 0; without `-f`, invalid gzip input remains an error. ([a4bfcc5](https://github.com/buglloc/axe/commit/a4bfcc50fd19d738b827951e47a1ed490d614e4a))
- `blockdev --report` now lists available block devices when no device is specified, and `--setro`/`--setrw` pass the ioctl argument in the expected form. ([ff0a898](https://github.com/buglloc/axe/commit/ff0a8987371bb7c07afdcb31b6487da335dff281))

## [v0.7.0](https://github.com/buglloc/axe/releases/tag/v0.7.0) — 2026-10-23

### Highlights

- The bundled `mount` now mounts filesystems as well as listing them. ([f9b342d](https://github.com/buglloc/axe/commit/f9b342d1faed4fe31413ee186a70bc6986d9d70e))
- `mount` supports loop devices. Image files mount through autoclear loop devices, as in util-linux. ([7d6b2b1](https://github.com/buglloc/axe/commit/7d6b2b1b9188bc36d5c67e77f9b1ce2420a39e0f))

### Changes

- New filesystems are mounted through `fsopen`/`fsconfig`/`fsmount`/`move_mount`. On older kernels without that API, `mount` uses `mount(2)`. Option errors now carry the kernel's message, for example `fsconfig() failed: tmpfs: Unknown parameter 'bogusopt'.` ([f9b342d](https://github.com/buglloc/axe/commit/f9b342d1faed4fe31413ee186a70bc6986d9d70e))
- A remount or `remount,bind` keeps every flag that was not specified. A bind mount given flags applies them with a follow-up `remount,bind`. ([f9b342d](https://github.com/buglloc/axe/commit/f9b342d1faed4fe31413ee186a70bc6986d9d70e))
- Loop-device behavior ([7d6b2b1](https://github.com/buglloc/axe/commit/7d6b2b1b9188bc36d5c67e77f9b1ce2420a39e0f)):
  - Supported options: `-o loop`, `loop=/dev/loopN`, `offset=` and `sizelimit=`. A regular-file source mounted as a block filesystem gets a loop device automatically.
  - An existing attachment of the same file and range is reused. An overlapping attachment is refused.
  - A write-protected backing file is attached read-only.
  - If the mount fails, the loop device created for it is detached.
- The `mount` synopsis in the command inventory now reads "List, mount, remount, bind, and move filesystems". ([f9b342d](https://github.com/buglloc/axe/commit/f9b342d1faed4fe31413ee186a70bc6986d9d70e))
- The mount documentation has been rewritten. ([5a9b1b8](https://github.com/buglloc/axe/commit/5a9b1b8c352a39b9947e36d416d269dcdce82cef))

### Fixes

- A lone `--help`, `-h`, `--version` or `--list` is now passed to the applet when AXE is invoked through an applet link or the PATH bridge. Before this fix, `mount --help` through a link printed AXE's own help. AXE answers these flags itself only when `argv[0]` is `axe` or names no applet. ([657b064](https://github.com/buglloc/axe/commit/657b0646892caeed5f5229cbb4c3b4ae2e04a33c))

## [v0.6.1](https://github.com/buglloc/axe/releases/tag/v0.6.1) — 2026-10-23

### Highlights

- AXE now uses Brush upstream revision `b562e0586b36`, up from `08db87a62345`. ([64c6c78](https://github.com/buglloc/axe/commit/64c6c78226231af6c131b8fe48f807c9d356e6a8))
- Updated AXE Store packages: `7zz` 26.04, `caddy` 2.11.7, `docker` 29.9.1, `helm` 4.3.1, `katana` 1.8.0, `kubectl` 1.37.1, `fzf` 0.74.5, `rclone` 1.75.2, `subfinder` 2.17.0, `tcpdump` 4.99.7, `tmux` 3.8, `yq` 4.54.1, `ss` 7.2.0, OpenSSH tools (`ssh`, `scp`, `sftp`, `ssh-add`, `ssh-agent`, `ssh-keygen`, `ssh-keyscan`) 10.6p1, and util-linux tools (`findmnt`, `lsns`, `nsenter`, `unshare`) 2.42.4. ([bbebe2c](https://github.com/buglloc/axe/commit/bbebe2c269e25a766d8c58e1b7e6bd6e2e4798c2))
- The Store `python` package moves to 3.14.9, built from python-build-standalone 20261021, and bundles `certifi` 2026.10.19. ([bbebe2c](https://github.com/buglloc/axe/commit/bbebe2c269e25a766d8c58e1b7e6bd6e2e4798c2))

### Fixes

- In a pipeline, `exit`, failed parameter expansions such as `${VAR?msg}`, failed builtins, and errors inside `eval` no longer terminate the parent shell. The shell sets `$?` and keeps running the script. A new smoke test covers this behavior. ([64c6c78](https://github.com/buglloc/axe/commit/64c6c78226231af6c131b8fe48f807c9d356e6a8))
- Fixed Store builds after the package updates. `age` and `rclone` now build with `GO_EXTLINK_ENABLED=0`, and `katana` now applies its patches after configuration so the upstream vendor hash is preserved. ([276c49b](https://github.com/buglloc/axe/commit/276c49b380afe74ba527c479d7297d8b769b2cc8))

## [v0.6.0](https://github.com/buglloc/axe/releases/tag/v0.6.0) — 2026-10-15

### Highlights

- Added `katana` 1.7.0 to AXE Store for crawling web applications and discovering URLs and endpoints. Available on Linux x86_64, Linux ARM64, and macOS ARM64. ([20f3a0d](https://github.com/buglloc/axe/commit/20f3a0d6fca9ff98554da0f6b7444687cb437573))
- Added `betterleaks` 1.9.0 to AXE Store for scanning files, stdin, and Git history for secrets, with optional validation. Available on Linux x86_64, Linux ARM64, and macOS ARM64. ([20f3a0d](https://github.com/buglloc/axe/commit/20f3a0d6fca9ff98554da0f6b7444687cb437573))
- Added `yara` and `yarac` 4.5.7 to AXE Store on Linux x86_64 and ARM64. `yara` matches user-supplied rules against files and process memory; `yarac` compiles rules for the scanner. ([20f3a0d](https://github.com/buglloc/axe/commit/20f3a0d6fca9ff98554da0f6b7444687cb437573))

### Changes

- The Store build of `katana` disables automatic update checks and browser downloads. Headless and hybrid crawling require an installed Chrome/Chromium, an explicit `-system-chrome-path`, or a browser connection through `-chrome-ws-url`. ([20f3a0d](https://github.com/buglloc/axe/commit/20f3a0d6fca9ff98554da0f6b7444687cb437573))
- `katana` and `betterleaks` include embedded fallback CA roots for TLS connections when system roots are unavailable. ([20f3a0d](https://github.com/buglloc/axe/commit/20f3a0d6fca9ff98554da0f6b7444687cb437573))
- The Store build of YARA enables crypto, Cuckoo, DEX, .NET, Mach-O, and magic modules. Its magic module uses an embedded file-identification database rather than loading one from the filesystem. ([20f3a0d](https://github.com/buglloc/axe/commit/20f3a0d6fca9ff98554da0f6b7444687cb437573))

## [v0.5.2](https://github.com/buglloc/axe/releases/tag/v0.5.2) — 2026-10-12

### Highlights

- Added `zdns` 2.0.5 to AXE Store for high-speed DNS lookups with optional local recursive resolution. ([68fd4e5](https://github.com/buglloc/axe/commit/68fd4e573af4877ac455fd4d963446ba30511ff4))
- Added `zgrab2` 1.0.0 to AXE Store for application-layer protocol scanning and service metadata collection. ([c1a29b3](https://github.com/buglloc/axe/commit/c1a29b335378f3ea5579aadbc834ddd51f1ab154))

### Changes

- `zgrab2 --blocklist-file -` reads `$HOME/.config/zgrab2/blocklist.conf` when present and uses an embedded default blocklist when that file is absent. Existing files, including empty ones, take precedence; other read errors and missing explicitly named files remain errors. ([c1a29b3](https://github.com/buglloc/axe/commit/c1a29b335378f3ea5579aadbc834ddd51f1ab154))

## [v0.5.1](https://github.com/buglloc/axe/releases/tag/v0.5.1) — 2026-10-11

### Highlights

- Unified bundled and AXE Store command categories by purpose. Website filters and command inventory rows now use the same category IDs and labels. ([eeb5208](https://github.com/buglloc/axe/commit/eeb520864bc3eab3abbf99e14c193d086e37b1c7))

### Changes

- Updated category values in `commands` output: for example, `cp` uses `files`, `chmod` uses `security`, `jq` uses `text`, and AXE controls such as `doctor` use `axe`. Aliases retain their canonical command’s category, including `dnsdomainname` → `system` and `gunzip` → `archives`. ([eeb5208](https://github.com/buglloc/axe/commit/eeb520864bc3eab3abbf99e14c193d086e37b1c7))
- Renamed 15 Store package IDs to match their categories:
  - `files/fd`, `files/fzf`, and `files/rg` replace the corresponding `search/` IDs.
  - `text/jq`, `text/sqlite3`, and `text/yq` replace the corresponding `data/` IDs.
  - `storage/findmnt`, `storage/fio`, `storage/rclone`, `storage/restic`, `storage/rsync`, and `storage/s5cmd` replace IDs under `containers/`, `debugging/`, `network/`, or `archives/`.
  - `terminal/tmux` and `terminal/zellij` replace the corresponding `runtime/` IDs.
  - `debugging/binwalk` replaces `security/binwalk`. ([eeb5208](https://github.com/buglloc/axe/commit/eeb520864bc3eab3abbf99e14c193d086e37b1c7))
- Website command search now matches the displayed category labels as well as category IDs, command names, synopses, and sources. ([eeb5208](https://github.com/buglloc/axe/commit/eeb520864bc3eab3abbf99e14c193d086e37b1c7))

## [v0.5.0](https://github.com/buglloc/axe/releases/tag/v0.5.0) — 2026-10-10

### Highlights

- Added bundled `zstd` compression and decompression, with multithreaded compression, stdout and explicit-file output, and compression levels 1–22. Input files are kept by default. ([72a9eeb](https://github.com/buglloc/axe/commit/72a9eebd7a43b82587e30492c154f88a71e48d6c))
- Added passive host overview and targeted process inspection to Vzik. `axe doctor` now reuses passive host observations. ([4f92a25](https://github.com/buglloc/axe/commit/4f92a25e49db379ccad5a963752e2771eff0184b))
- Expanded the signed Store bootstrap snapshot with 19 tools: `7zz`, `age`, `bwrap`, `caddy`, `cek`, `dig`, `ethtool`, `fio`, `helm`, `layerx`, `objdump`, `rclone`, `readelf`, `restic`, `s5cmd`, `stern`, `tmux`, `websocat`, and `zellij`. Existing snapshot entries are unchanged. ([0e86124](https://github.com/buglloc/axe/commit/0e86124ba06f975f6c1bd685ddf8d685a3221df4))

### Changes

- The default shell prompt now shows the current directory. ([340f090](https://github.com/buglloc/axe/commit/340f090767db08ca7ea3e16a085242d51bde3f7d))
- Vzik evidence moves to protocol v4, with explicit observation scope and outcomes, degraded interface-fallback reporting, and clearer bus selection. ([4f92a25](https://github.com/buglloc/axe/commit/4f92a25e49db379ccad5a963752e2771eff0184b))
- Vzik adds version output, pretty-printed metadata documents, and capture into a new private directory. Captures reject destination collisions, preserve JSONL framing, and retain non-UTF-8 artifact paths losslessly. ([4f92a25](https://github.com/buglloc/axe/commit/4f92a25e49db379ccad5a963752e2771eff0184b))
- Updated agent skills for Vzik protocol v4, passive overview, and one-process inspection. The guidance prefers `jq` selection from a saved snapshot while respecting explicit stdout-only constraints. ([aabb3da](https://github.com/buglloc/axe/commit/aabb3da9d7fcb0666b66933786c532de8cd55c25))

### Fixes

- Fixed Darwin Go binary output paths in AXE Store builds. ([82a67df](https://github.com/buglloc/axe/commit/82a67df718be1c5fbf4418635b8bccf9ec8583d6))
- Made relay tests independent of edition credentials. ([353d323](https://github.com/buglloc/axe/commit/353d323381732f5cbe3e0398a36ee73597bd4d4a))

## [v0.4.0](https://github.com/buglloc/axe/releases/tag/v0.4.0) — 2026-10-06

### Highlights

- Added bundled `killall` on Linux ([167cf49](https://github.com/buglloc/axe/commit/167cf49a7116ef915df9c0dc79ab934bb5309fef)).
- Added real thread listings and native typed sorting to `ps` ([108b1a7](https://github.com/buglloc/axe/commit/108b1a754f769329d8c8ce88a34925cb289d175f)).

### Changes

- Release preparation now allows Cargo registry index access when updating workspace dependencies, rather than requiring an offline update ([f9070f5](https://github.com/buglloc/axe/commit/f9070f5e0143cf91f72378a3eda081f3b71605a9)).

### Fixes

- Fixed BSD-style `ps ax` selection to include other users’ processes without a TTY ([33a81cd](https://github.com/buglloc/axe/commit/33a81cdf6a47adacf3b46158c0dcdf6cd4ec5b1a)).
- Improved `ps` numeric fields and output-width compatibility ([7bb8ebc](https://github.com/buglloc/axe/commit/7bb8ebc81a83fb7cc1d1516a424f81da0717c82c)).
- Fixed an executable-copy race in the Store/PATH fallback test ([97eea58](https://github.com/buglloc/axe/commit/97eea583602d62e84075283f33b30190380c621c)).

## [v0.3.0](https://github.com/buglloc/axe/releases/tag/v0.3.0) — 2026-10-04

### Highlights

- Added `gori` 0.7.1 to AXE Store for Linux x86_64 and ARM64. It provides a terminal HTTP intercepting proxy and web application testing tools ([a65eced](https://github.com/buglloc/axe/commit/a65ecedb33206b46c9687380086cb07f9f29b4cc)).
- Added `interactsh-client` 1.3.1 to AXE Store for Linux x86_64, Linux ARM64, and macOS ARM64. It generates out-of-band testing payloads and collects interactions ([1b65c0e](https://github.com/buglloc/axe/commit/1b65c0e8678b8dad1f487cd5647ef6486781110e)).

### Changes

- The Store build of `gori` disables automatic startup update checks by default; users can opt in through its settings ([a65eced](https://github.com/buglloc/axe/commit/a65ecedb33206b46c9687380086cb07f9f29b4cc)).
- The Store build of `interactsh-client` removes the startup version check ([1b65c0e](https://github.com/buglloc/axe/commit/1b65c0e8678b8dad1f487cd5647ef6486781110e)).

## [v0.2.2](https://github.com/buglloc/axe/releases/tag/v0.2.2) — 2026-10-01

### Highlights

SSH PTY sessions keep accepting input after the channel buffer fills ([58fba6b](https://github.com/buglloc/axe/commit/58fba6b0463058f816568559be41152be2a93988)). The release also switches the website download instructions to the S3 binary ([7c2065f](https://github.com/buglloc/axe/commit/7c2065fe9e16bddfb0ae3e82d4dced5b13dd4dbe)) and refreshes release metadata ([34ecc2c](https://github.com/buglloc/axe/commit/34ecc2c831583d1ac0dd277a0c90148b920039f7)).

### Fixes

- Fixed SSH PTY input stalls when the channel buffer fills ([58fba6b](https://github.com/buglloc/axe/commit/58fba6b0463058f816568559be41152be2a93988)).

## [v0.2.1](https://github.com/buglloc/axe/releases/tag/v0.2.1) — 2026-09-30

### Highlights

- Simplified the managed PATH directory for bundled commands ([81d8c80](https://github.com/buglloc/axe/commit/81d8c80e4cc2c1ba599c6628665822585c76c075)).

### Fixes

- Prevented concurrent PATH publishers from bypassing an active lock ([81d8c80](https://github.com/buglloc/axe/commit/81d8c80e4cc2c1ba599c6628665822585c76c075)).
- Updated executable links and removed obsolete command links during PATH publication ([81d8c80](https://github.com/buglloc/axe/commit/81d8c80e4cc2c1ba599c6628665822585c76c075)).

## [v0.2.0](https://github.com/buglloc/axe/releases/tag/v0.2.0) — 2026-09-29

### Highlights

- The SSH server accepts user certificates signed by ECDSA CAs using NIST P-256, P-384, or P-521 keys. Plain public keys remain rejected. ([1342f9b](https://github.com/buglloc/axe/commit/1342f9b194ce0fc7c7b578386f5f16ca79b17a9b))
- `axe-relay` server startup now requires `--config FILE`. Its listener addresses, ports, certificate paths, and token-file path come from that JSON configuration; `--token FILE` can override the configured token file. ([1844207](https://github.com/buglloc/axe/commit/18442077bb9627218ed7a963f6d6721a11d180aa))

### Changes

- New guides cover AXE architecture and custom editions; the relay guide documents file-backed server configuration and service setup. ([52b1fea](https://github.com/buglloc/axe/commit/52b1fea340bb6fb0f9adc8fb119fa7453726c01f))
- The bootstrap guide has been shortened and points to the component guides for build and publication details. ([9f8b3ee](https://github.com/buglloc/axe/commit/9f8b3eebd738530bfdfb77c3cfcd2ffb0052ab85))
- The AXE Store skill documentation distinguishes single-package checks from signed-snapshot verification and publication. ([6fbea0d](https://github.com/buglloc/axe/commit/6fbea0d14f566a946b9afafb40d4a62c1f146110))

## [v0.1.2](https://github.com/buglloc/axe/releases/tag/v0.1.2) — 2026-09-29

### Highlights

- AXE Store now includes Yandex Cloud CLI (`yc`) 1.25.0 for Linux x86_64 and aarch64, and macOS aarch64. ([6530237](https://github.com/buglloc/axe/commit/6530237d0cfddce1ce13d98c5ef5821e2bd60136))

### Changes

- The release process now builds and publishes `axe`, `axe-relay`, and `vzik` for all three supported release targets. Release metadata and download-table generation cover all nine artifacts. ([336a06e](https://github.com/buglloc/axe/commit/336a06e352e557c766b1b478dc4596c4e8db3381))

## [v0.1.1](https://github.com/buglloc/axe/releases/tag/v0.1.1) — 2026-09-27

### Highlights

- Added `dnsdomainname`, `rgrep`, and `gzcat` aliases ([cdccfd7](https://github.com/buglloc/axe/commit/cdccfd7f4f79678bff89c7c73f5ec7ffb4128aec)).
- Consolidated applet dispatch and compression drivers ([f34c26d](https://github.com/buglloc/axe/commit/f34c26d8a7295be0cf70d1141c8ecf1875cb26d8)).

### Changes

- Removed the Cargo applet feature matrix and switched three Brush crates from vendored copies to upstream dependencies. ([3eb50c1](https://github.com/buglloc/axe/commit/3eb50c10435eee4aa0dbe519eb827e03fa8bbe1c), [e0e74cc](https://github.com/buglloc/axe/commit/e0e74cc8270bf2ef37d9f34d89919034dac0757f))

### Fixes

- Consolidated Store metadata and revalidated stale indexes ([e62c318](https://github.com/buglloc/axe/commit/e62c318276bb28ad4012046cd3ccb45530a363d6)).

## [v0.1.0](https://github.com/buglloc/axe/releases/tag/v0.1.0) — 2026-09-26

### Highlights

- Initial AXE release, Store components, relay, and `vzik`.
