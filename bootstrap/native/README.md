# Native bootstrap loader

A freestanding Linux loader for x86_64 and AArch64. The output is a static
`ET_EXEC` ELF of about 5 KiB with no libc, dynamic loader, `INTERP`, or
`DT_NEEDED`. It uses raw syscalls. The linker script maps the ELF headers into
the code segment, so the file has no page-alignment padding.

## Build

```sh
bootstrap/native/build.sh x86_64|aarch64 CONSTANTS_H OUTPUT
```

`CONSTANTS_H` is the `<target>.h` file written by `bootstrap/generate.py`. Both
architectures build with `zig cc` from the development shell; set
`AXE_BOOTSTRAP_CC` to use another clang-compatible driver. The script rejects
an output that is not `ET_EXEC` or has `INTERP` or `DT_NEEDED`.

| File | Purpose |
| --- | --- |
| `loader.c` | network, Ascon-Hash256, and exec logic |
| `types.h` | fixed-width types |
| `linker.ld` | two `PT_LOAD` segments and a non-executable stack |
| `<arch>/arch.h` | syscall numbers and the syscall wrapper |
| `<arch>/start.S` | `_start`: passes `argc`, `argv`, `envp` to `loader_main` |

## Runtime behavior

1. Map an anonymous buffer of the payload size.
2. For each pinned address in order (IPv6 first), connect to the HTTP port with
   a 5-second deadline and send `GET` with the release `Host`.
3. Read the response head (up to 4 KiB, across any number of reads), then read
   exactly the payload size. Status and headers are not parsed.
4. Hash the payload with Ascon-Hash256 (NIST SP 800-232) and compare it with
   the pinned digest. A mismatch moves on to the next address.
5. Run the payload with `memfd_create` and `execveat(AT_EMPTY_PATH)`. If that
   fails, write a mode-0700 file under `/tmp` and `execve` it.

The original `argv` and environment are passed through. With no arguments, the
edition's `default_args` are added after `argv[0]`. On failure the loader prints
one line to stderr (`download failed`, `payload digest mismatch`, or `cannot
execute payload`) and exits with status 1.
