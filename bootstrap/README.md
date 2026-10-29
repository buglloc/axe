# AXE bootstrap

Small programs that download the static `axe` release onto a broken or minimal
Linux host, verify it, and run it without installing anything.

| Store key | Program | Verification |
| --- | --- | --- |
| `b/sh` | POSIX shell | size + SHA-256; runs unverified only if no SHA-256 tool exists |
| `b/py` | Python 3.6+ | size + SHA-256 |
| `b/js` | Node.js | size + SHA-256 |
| `b/pl` | Perl 5 | size + SHA-256 |
| `b/amd64-linux`, `b/aarch64-linux` | Native ELF | size + Ascon-Hash256 |

The scripts are universal: they select `x86_64-linux` or `aarch64-linux` from
`uname -m` (or `AXE_BOOTSTRAP_TARGET`). Each Native loader serves one target.
Releases publish every program under `releases/v<VERSION>/b/` and update the
stable `b/` aliases.

## Transport

Every program embeds the payload size and digest, the release host and path,
and the pinned Store addresses from `config/store.json`. It connects to those
addresses directly, IPv6 first, with no DNS. Because the digest is pinned,
plain HTTP is safe; HTTPS adds nothing to integrity and fails on hosts without
CA certificates or with a wrong clock.

- Python and Node.js try HTTPS on every address, then HTTP.
- Perl tries HTTPS only when `IO::Socket::SSL` is installed.
- Shell tries, in order: curl (HTTPS, then HTTP), wget or BusyBox wget, Python,
  Perl, Node.js, and bash `/dev/tcp`. Every fetcher uses plain HTTP except
  curl's first pass.
- Native uses HTTP only.

A size or digest mismatch rejects the download and the next address or tool is
tried. A rejected download is never executed.

## Execution

| Program | Without `--output` |
| --- | --- |
| Shell | private directory in `$AXE_BOOTSTRAP_DIR`, `$TMPDIR`, `/dev/shm`, `/tmp`, `/var/tmp`, `$HOME`, or the working directory; a probe script skips `noexec` mounts; the file stays after `exec` |
| Python | `memfd_create`, then the same directory list |
| Perl | the same directory list; a failed `exec` moves to the next directory |
| Node.js | the same directory list; Node.js runs `axe` as a child and removes the directory after it exits |
| Native | `memfd_create`, then `/tmp` |

Script options: `--output PATH` saves the verified file to `PATH` and runs it
from there; `--no-exec` (with `--output`) only saves it. The remaining
arguments go to `axe`; use `--` before an argument that looks like an option.
With no `axe` arguments, every program uses `default_args` from the edition's
`config/bootstrap.json`.

## Generation

`tools/release.py` builds the bundle from the release payloads and calls this
module. To render by hand:

```sh
python3 bootstrap/generate.py BUNDLE.json OUTPUT_DIR
bootstrap/native/build.sh x86_64 OUTPUT_DIR/x86_64-linux.h axe-bootstrap
```

`BUNDLE.json` holds `version`, `default_args`, and per-target `host`, `path`,
`ports` (`https`, `http`), `addresses`, `size`, `sha256`, and `ascon_hash256`.
`generate.py` validates every value before embedding it in shell, Python,
JavaScript, Perl, and C, and writes `sh`, `py`, `js`, `pl`, and one
`<target>.h` per target. Output is deterministic.

## Tests

`just bootstrap-test` (part of `just smoke`) builds a small static payload,
serves it from a local dual-stack server, and runs every program, including each
shell fetcher on its own. The tests cover argument and `default_args` handling,
a response head split across TCP segments, a corrupted payload (never
executed), and `--output --no-exec`.
