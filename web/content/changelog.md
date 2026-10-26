+++
title = "Changelog"
+++

## [v0.8.1](https://github.com/buglloc/axe/releases/tag/v0.8.1) — 2026-10-26

### Highlights

- Added `less` to the AXE Store for `aarch64-linux` and `x86_64-linux`. It pages through files and command output. `dc312c4675db6dad8c85fc528af02980ac72ed26`

### Changes

- Built the Store `less` package with static ncurses and compiled-in descriptions for common terminals, including Kitty and Ghostty, so it does not depend solely on a host terminfo database. `dc312c4675db6dad8c85fc528af02980ac72ed26`
- Refreshed the signed Store bootstrap index and inventory to include `less` version `710`. `59ec18fd95773eaf107529cf12d81224ef3f8658`
- The supplied evidence does not support additional user-facing changes beyond these Store and package updates.

## [v0.8.0](https://github.com/buglloc/axe/releases/tag/v0.8.0) — 2026-10-25

### Highlights

- Added bundled `sdiff` with side-by-side comparison, merge mode, interactive conflict selection, and atomic output publication. `41ccfcc009c4b45c3f34ab57b4c346ddacdefd8`
- Improved `diff` compatibility with GNU context horizons, `--horizon-lines`, shifted hunk boundaries, missing-final-newline handling, and byte- or display-width-aware side-by-side output. `d500cdef41302380584ccbfbeef3c1e499e9eac1`
- Added the portable `difft` Store package based on difftastic. `22ba6b29cd7adfaef7c8f79064b68cc4ce5786f6`
- Updated all vendored Brush crates to upstream revision `e37b3c91`, including fixes for builtin help output, empty-array expansion, and globbing after empty quotes. `741c1f482956058f50bce2817686ac5a49ad4b40`
- Added portable Store packages for Vim, asciinema, micro, ox, broot, and spf. `a72afa76188f832dd0d47382a865169c99cabe83` `ce45980e8edaf6eba7aef9568b33384267ad079b` `9a17438f663f76fc6cfff1dd74c66d1c157e6752` `89229eabd540f4f46392aab2938be255f59a0d21`

### Changes

- Removed the bundled `diff3` applet because its generic merge implementation misclassified conflicts and could panic on valid input. `4bed2f40d8092427bbdbcccc354fc142686d27ca`
- `diff` now supports `-E`, `-Z`, `--strip-trailing-cr`, `-d`/`--minimal`, `--unidirectional-new-file`, `-T`, `--left-column`, and `--suppress-common-lines`; its edit scripts use gix-imara-diff with raw-byte line tokenization. `10e3b08e14f9ca141ece7ccce88d344334bda6fb`
- `tar` now accepts byte-oriented `--files-from` lists, including `-T -`, `--null`, non-UTF-8 names, and in-list `-C`/`--directory` directives. `548cf28f6f5a422f9b0ffb3fdada442ce57f56c6`
- `gzip` and XZ compression now preserve source ownership where permitted, clean up staging files after termination signals, support `-S`/`--suffix`, and recursively process directories with `-r`. `cf7214d368bfac119f51377f42fa26b9876eea7b`
- `awk` now skips missing `ARGV` entries and bounds dynamically sized fields and field indices, returning a runtime error instead of hanging or panicking on extreme values. `b16904f92ffc16cfffae4df3136f873a37c56c63`
- `jq` input handling now preserves a lazy byte stream across files and stdin, validates JSON boundaries, canonicalizes numbers, and aligns `halt_error` status and payload behavior with jq. `3258b2be38e6fb1d32d394f0f0b9f3cba894c264`
- `sed` now keeps scripts, operands, patterns, replacements, transliteration tables, labels, and filenames byte-oriented; resolves transliteration behavior from the active locale; and opens `w`/`W` and substitution output files at startup. `debc6ee942bfac119f51377f42fa26b9876eea7`
- `xargs` now supports GNU logical EOF options `-E`, legacy `-e`, and `--eof`, including raw markers and the documented record-boundary behavior. `c89784d302904d9bf4c0faebb918306850b535d8`
- The release adds a vendored provenance audit and a `vendor-test` target covering vendored packages against the locked workspace dependency graph. `2aa9fbb5a11aab9c851bfbb68786eeeec1530ff3`

### Fixes

- AXE now preserves inherited SIGPIPE behavior across standalone applets, shell builtins, external children, and bundled self-exec children; in-process broken-pipe failures map to shell status 141 without an extra diagnostic. `5036f0478e64d6bd6f6affaac60239e10a5dd7b1`
- `cmp` now reports stdout write failures with status 2 and flushes its `-l` output before returning. `ffa7e9e8727da6b0bb2aa11ce5a23c4365e686af`
- `mkdir` restores all special mode bits through a pinned parent-relative descriptor, avoiding pathname substitution races and preserving the sticky bit. `914aba98440ac18c3a3005420c61ca4edc21ab5f`
- `blkid` now rejects truncated LUKS1 probes and reports swap, LUKS, exFAT, GPT, MBR, FAT12, and partition-table identifiers according to the documented compatibility behavior. `678fa26cab0434c43da27d5b61c4e91a192e1dd9` `7d785f1690afb738939574620920bb2f0ce2616`
- `stty` now accepts arbitrary Linux `termios2` speeds through BOTHER while retaining standard names and aliases. `a995af5a70aae630c604470bbb37d2ac73cf3d2f`
- `find` preserves raw Unix pathname bytes in formatters and regular-expression operands, and propagates output write failures instead of panicking. `899ad633700a7217e5955f91e88a1f69a0cfa979`
- `tree` now supports raw-name and question-mark modes, locale-specific escaping, and `-N`; its `-h` behavior is also aligned with GNU tree. `30fd83be49feb62259f98d70a482e1ae59d7fafd` `237f3bd7cde2dabdeeb3cb5a809975b9082ab7ba`
- `ps`, `ip`, `arp`, and `ifconfig` received compatibility fixes for field widths, route and address rendering, option bundling, missing-device errors, and broken-pipe handling. `c404ed3ff25cc3531c1b3277607f7a588033b9ae` `d82cb0683f37c57cddbcae7cc27731b6ae3edcdb` `2228565a05765577a107c0944de4c9083324c0f0`
- `umount` now supports recursive and filtered unmount selection, lazy and forced modes, read-only remount on busy targets, and verbose reporting. `b1492ead8cca02676ebd7d1cf7cc3262942b5e4b`
- `ping` now includes mean deviation in its `rtt min/avg/max/mdev` summary. `de48a9d3290fa6c21c2a93d8366675d915b14865`
- `gzip -f` and `zcat -f` now pass non-gzip input through unchanged with status 0; without `-f`, invalid gzip input remains an error. `a4bfcc50fd19d738b827951e47a1ed490d614e4a`
- `blockdev --report` now lists available block devices when no device is specified, and `--setro`/`--setrw` pass the ioctl argument in the expected form. `ff0a8987371bb7c07afdcb31b6487da335dff281`

## [v0.7.0](https://github.com/buglloc/axe/releases/tag/v0.7.0) — 2026-10-23

### Highlights

- The bundled `mount` now mounts filesystems as well as listing them. (`f9b342d1faed`)
- `mount` supports loop devices. Image files mount through autoclear loop devices, as in util-linux. (`7d6b2b1b9188`)

### Changes

- New filesystems are mounted through `fsopen`/`fsconfig`/`fsmount`/`move_mount`. On older kernels without that API, `mount` uses `mount(2)`. Option errors now carry the kernel's message, for example `fsconfig() failed: tmpfs: Unknown parameter 'bogusopt'.` (`f9b342d1faed`)
- A remount or `remount,bind` keeps every flag that was not specified. A bind mount given flags applies them with a follow-up `remount,bind`. (`f9b342d1faed`)
- Loop-device behavior (`7d6b2b1b9188`):
  - Supported options: `-o loop`, `loop=/dev/loopN`, `offset=` and `sizelimit=`. A regular-file source mounted as a block filesystem gets a loop device automatically.
  - An existing attachment of the same file and range is reused. An overlapping attachment is refused.
  - A write-protected backing file is attached read-only.
  - If the mount fails, the loop device created for it is detached.
- The `mount` synopsis in the command inventory now reads "List, mount, remount, bind, and move filesystems". (`f9b342d1faed`)
- The mount documentation has been rewritten. (`5a9b1b8c352a`)

### Fixes

- A lone `--help`, `-h`, `--version` or `--list` is now passed to the applet when AXE is invoked through an applet link or the PATH bridge. Before this fix, `mount --help` through a link printed AXE's own help. AXE answers these flags itself only when `argv[0]` is `axe` or names no applet. (`657b0646892c`)

## [v0.6.1](https://github.com/buglloc/axe/releases/tag/v0.6.1) — 2026-10-23

### Highlights

- AXE now uses Brush upstream revision `b562e0586b36`, up from `08db87a62345`. (`64c6c78226231`)
- Updated AXE Store packages: `7zz` 26.04, `caddy` 2.11.7, `docker` 29.9.1, `helm` 4.3.1, `katana` 1.8.0, `kubectl` 1.37.1, `fzf` 0.74.5, `rclone` 1.75.2, `subfinder` 2.17.0, `tcpdump` 4.99.7, `tmux` 3.8, `yq` 4.54.1, `ss` 7.2.0, OpenSSH tools (`ssh`, `scp`, `sftp`, `ssh-add`, `ssh-agent`, `ssh-keygen`, `ssh-keyscan`) 10.6p1, and util-linux tools (`findmnt`, `lsns`, `nsenter`, `unshare`) 2.42.4. (`bbebe2c269e2`)
- The Store `python` package moves to 3.14.9, built from python-build-standalone 20261021, and bundles `certifi` 2026.10.19. (`bbebe2c269e2`)

### Fixes

- In a pipeline, `exit`, failed parameter expansions such as `${VAR?msg}`, failed builtins, and errors inside `eval` no longer terminate the parent shell. The shell sets `$?` and keeps running the script. A new smoke test covers this behavior. (`64c6c78226231`)
- Fixed Store builds after the package updates. `age` and `rclone` now build with `GO_EXTLINK_ENABLED=0`, and `katana` now applies its patches after configuration so the upstream vendor hash is preserved. (`276c49b380af`)

## [v0.6.0](https://github.com/buglloc/axe/releases/tag/v0.6.0) — 2026-10-15

### Highlights

- Added `katana` 1.7.0 to AXE Store for crawling web applications and discovering URLs and endpoints. Available on Linux x86_64, Linux ARM64, and macOS ARM64. (`20f3a0d6fca9`)
- Added `betterleaks` 1.9.0 to AXE Store for scanning files, stdin, and Git history for secrets, with optional validation. Available on Linux x86_64, Linux ARM64, and macOS ARM64. (`20f3a0d6fca9`)
- Added `yara` and `yarac` 4.5.7 to AXE Store on Linux x86_64 and ARM64. `yara` matches user-supplied rules against files and process memory; `yarac` compiles rules for the scanner. (`20f3a0d6fca9`)

### Changes

- The Store build of `katana` disables automatic update checks and browser downloads. Headless and hybrid crawling require an installed Chrome/Chromium, an explicit `-system-chrome-path`, or a browser connection through `-chrome-ws-url`. (`20f3a0d6fca9`)
- `katana` and `betterleaks` include embedded fallback CA roots for TLS connections when system roots are unavailable. (`20f3a0d6fca9`)
- The Store build of YARA enables crypto, Cuckoo, DEX, .NET, Mach-O, and magic modules. Its magic module uses an embedded file-identification database rather than loading one from the filesystem. (`20f3a0d6fca9`)

## [v0.5.2](https://github.com/buglloc/axe/releases/tag/v0.5.2) — 2026-10-12

### Highlights

- Added `zdns` 2.0.5 to AXE Store for high-speed DNS lookups with optional local recursive resolution. (`68fd4e573af4`)
- Added `zgrab2` 1.0.0 to AXE Store for application-layer protocol scanning and service metadata collection. (`c1a29b335378`)

### Changes

- `zgrab2 --blocklist-file -` reads `$HOME/.config/zgrab2/blocklist.conf` when present and uses an embedded default blocklist when that file is absent. Existing files, including empty ones, take precedence; other read errors and missing explicitly named files remain errors. (`c1a29b335378`)

## [v0.5.1](https://github.com/buglloc/axe/releases/tag/v0.5.1) — 2026-10-11

### Highlights

- Unified bundled and AXE Store command categories by purpose. Website filters and command inventory rows now use the same category IDs and labels. (`eeb520864bc3`)

### Changes

- Updated category values in `commands` output: for example, `cp` uses `files`, `chmod` uses `security`, `jq` uses `text`, and AXE controls such as `doctor` use `axe`. Aliases retain their canonical command’s category, including `dnsdomainname` → `system` and `gunzip` → `archives`. (`eeb520864bc3`)
- Renamed 15 Store package IDs to match their categories:
  - `files/fd`, `files/fzf`, and `files/rg` replace the corresponding `search/` IDs.
  - `text/jq`, `text/sqlite3`, and `text/yq` replace the corresponding `data/` IDs.
  - `storage/findmnt`, `storage/fio`, `storage/rclone`, `storage/restic`, `storage/rsync`, and `storage/s5cmd` replace IDs under `containers/`, `debugging/`, `network/`, or `archives/`.
  - `terminal/tmux` and `terminal/zellij` replace the corresponding `runtime/` IDs.
  - `debugging/binwalk` replaces `security/binwalk`. (`eeb520864bc3`)
- Website command search now matches the displayed category labels as well as category IDs, command names, synopses, and sources. (`eeb520864bc3`)

## Unreleased

### Changes

- Unified bundled and Store command categories by purpose. Website filters and inventory rows use the same category IDs and labels.
- Renamed 15 Store package IDs to match their categories, including `files/rg`, `text/jq`, `storage/restic`, and `terminal/tmux`. Regenerate the signed Store bootstrap snapshot before an AXE release.

## [v0.5.0](https://github.com/buglloc/axe/releases/tag/v0.5.0) — 2026-10-10

### Highlights

- Added bundled `zstd` compression and decompression, with multithreaded compression, stdout and explicit-file output, and compression levels 1–22. Input files are kept by default. (`72a9eebd7a43`)
- Added passive host overview and targeted process inspection to Vzik. `axe doctor` now reuses passive host observations. (`4f92a25e49db`)
- Expanded the signed Store bootstrap snapshot with 19 tools: `7zz`, `age`, `bwrap`, `caddy`, `cek`, `dig`, `ethtool`, `fio`, `helm`, `layerx`, `objdump`, `rclone`, `readelf`, `restic`, `s5cmd`, `stern`, `tmux`, `websocat`, and `zellij`. Existing snapshot entries are unchanged. (`0e86124ba06f`)

### Changes

- The default shell prompt now shows the current directory. (`340f090767db`)
- Vzik evidence moves to protocol v4, with explicit observation scope and outcomes, degraded interface-fallback reporting, and clearer bus selection. (`4f92a25e49db`)
- Vzik adds version output, pretty-printed metadata documents, and capture into a new private directory. Captures reject destination collisions, preserve JSONL framing, and retain non-UTF-8 artifact paths losslessly. (`4f92a25e49db`)
- Updated agent skills for Vzik protocol v4, passive overview, and one-process inspection. The guidance prefers `jq` selection from a saved snapshot while respecting explicit stdout-only constraints. (`aabb3da9d7fc`)

### Fixes

- Fixed Darwin Go binary output paths in AXE Store builds. (`82a67df718be`)
- Made relay tests independent of edition credentials. This is a test-isolation fix; the supplied evidence does not establish a relay runtime behavior change. (`353d32338173`)

## [v0.4.0](https://github.com/buglloc/axe/releases/tag/v0.4.0) — 2026-10-06

### Highlights

- Added bundled `killall` on Linux (`167cf49a7116ef915df9c0dc79ab934bb5309fef`).
- Added real thread listings and native typed sorting to `ps` (`108b1a754f769329d8c8ce88a34925cb289d175f`).

### Changes

- Release preparation now allows Cargo registry index access when updating workspace dependencies, rather than requiring an offline update (`f9070f5e0143cf91f72378a3eda081f3b71605a9`).

### Fixes

- Fixed BSD-style `ps ax` selection to include other users’ processes without a TTY (`33a81cdf6a47adacf3b46158c0dcdf6cd4ec5b1a`).
- Improved `ps` numeric fields and output-width compatibility (`7bb8ebc81a83fb7cc1d1516a424f81da0717c82c`).
- Fixed an executable-copy race in the Store/PATH fallback test (`97eea583602d62e84075283f33b30190380c621c`).

## [v0.3.0](https://github.com/buglloc/axe/releases/tag/v0.3.0) — 2026-10-04

### Highlights

- Added `gori` 0.7.1 to AXE Store for Linux x86_64 and ARM64. It provides a terminal HTTP intercepting proxy and web application testing tools (`a65ecedb33206b46c9687380086cb07f9f29b4cc`).
- Added `interactsh-client` 1.3.1 to AXE Store for Linux x86_64, Linux ARM64, and macOS ARM64. It generates out-of-band testing payloads and collects interactions (`1b65c0e8678b8dad1f487cd5647ef6486781110e`).

### Changes

- The Store build of `gori` disables automatic startup update checks by default; users can opt in through its settings (`a65ecedb33206b46c9687380086cb07f9f29b4cc`).
- The Store build of `interactsh-client` removes the startup version check (`1b65c0e8678b8dad1f487cd5647ef6486781110e`).

## [v0.2.2](https://github.com/buglloc/axe/releases/tag/v0.2.2) — 2026-10-01

### Highlights

SSH PTY sessions keep accepting input after the channel buffer fills (`58fba6b0463058f816568559be41152be2a93988`). The release also switches the website download instructions to the S3 binary (`7c2065fe9e16bddfb0ae3e82d4dced5b13dd4dbe`) and refreshes release metadata (`34ecc2c831583d1ac0dd277a0c90148b920039f7`).

### Fixes

- Fixed SSH PTY input stalls when the channel buffer fills (`58fba6b0463058f816568559be41152be2a93988`).

## [v0.2.1](https://github.com/buglloc/axe/releases/tag/v0.2.1) — 2026-09-30

### Highlights

- Simplified the managed PATH directory for bundled commands (`81d8c80e4cc2c1ba599c6628665822585c76c075`).

### Fixes

- Prevented concurrent PATH publishers from bypassing an active lock (`81d8c80e4cc2c1ba599c6628665822585c76c075`).
- Updated executable links and removed obsolete command links during PATH publication (`81d8c80e4cc2c1ba599c6628665822585c76c075`).

## [v0.2.0](https://github.com/buglloc/axe/releases/tag/v0.2.0) — 2026-09-29

### Highlights

- The SSH server accepts user certificates signed by ECDSA CAs using NIST P-256, P-384, or P-521 keys. Plain public keys remain rejected. (`1342f9b194ce0fc7c7b578386f5f16ca79b17a9b`)
- `axe-relay` server startup now requires `--config FILE`. Its listener addresses, ports, certificate paths, and token-file path come from that JSON configuration; `--token FILE` can override the configured token file. (`18442077bb9627218ed7a963f6d6721a11d180aa`)

### Changes

- New guides cover AXE architecture and custom editions; the relay guide documents file-backed server configuration and service setup. (`52b1fea340bb6fb0f9adc8fb119fa7453726c01f`)
- The bootstrap guide has been shortened and points to the component guides for build and publication details. (`9f8b3eebd738530bfdfb77c3cfcd2ffb0052ab85`)
- The AXE Store skill documentation distinguishes single-package checks from signed-snapshot verification and publication. (`6fbea0d14f566a946b9afafb40d4a62c1f146110`)

## [v0.1.2](https://github.com/buglloc/axe/releases/tag/v0.1.2) — 2026-09-29

### Highlights

- AXE Store now includes Yandex Cloud CLI (`yc`) 1.25.0 for Linux x86_64 and aarch64, and macOS aarch64. (`6530237d0cfddce1ce13d98c5ef5821e2bd60136`)

### Changes

- The release process now builds and publishes `axe`, `axe-relay`, and `vzik` for all three supported release targets. Release metadata and download-table generation cover all nine artifacts. (`336a06e352e557c766b1b478dc4596c4e8db3381`)

## [v0.1.1](https://github.com/buglloc/axe/releases/tag/v0.1.1) — 2026-09-27

### Highlights

- Added `dnsdomainname`, `rgrep`, and `gzcat` aliases (`cdccfd7f4f79678bff89c7c73f5ec7ffb4128aec`).
- Consolidated applet dispatch and compression drivers (`f34c26d8a7295be0cf70d1141c8ecf1875cb26d8`).

### Changes

- Removed the Cargo applet feature matrix and switched three Brush crates from vendored copies to upstream dependencies. (`3eb50c10435eee4aa0dbe519eb827e03fa8bbe1c`, `e0e74cc8270bf2ef37d9f34d89919034dac0757f`)

### Fixes

- Consolidated Store metadata and revalidated stale indexes (`e62c318276bb28ad4012046cd3ccb45530a363d6`).

## [v0.1.0](https://github.com/buglloc/axe/releases/tag/v0.1.0) — 2026-09-26

### Highlights

- Initial AXE release, Store components, relay, and `vzik`.
