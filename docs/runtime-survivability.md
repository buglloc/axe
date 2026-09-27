# AXE self-exec after unlink, without procfs, and under exec restrictions

AXE has two execution paths:

- **In-process entry dispatch:** the current process enters an applet through BusyBox-style `argv[0]`, `axe --applet`, a positional command, or hidden dispatch. Shell aliases, functions, and builtins also run in-process.
- **Self-exec:** a running AXE process starts another AXE process for a bundled command from Brush, a nested `xargs` invocation, a daemon worker, an SSH shell/exec session, or a BusyBox-style PATH bridge.

Entry dispatch runs without access to the executable. Most bundled applets launched from Brush use the applet shim to start a child AXE process. The native `doctor` callback runs in the shell process and reads a snapshot of live shell state. Builtins, aliases, and functions remain available when self-exec is unavailable.

When starting a new process, AXE preserves:

- the applet's original `argv[0]`;
- byte-oriented `OsString` arguments and non-UTF-8 paths;
- the caller's modified environment;
- stdin, stdout, stderr, and shell redirections;
- session and process-group setup at launch boundaries that specify it;
- the order of caller `pre_exec` hooks;
- Tokio `kill_on_drop` behavior and child-process lifetime.

## Capabilities retained at startup

`crates/axe/src/executable.rs` creates the sole process-wide owner of self-exec capabilities and the runtime root before building the registry, starting the Tokio runtime, or creating other threads.

On Linux, AXE checks these sources in order:

1. An inherited descriptor handed off by a previous AXE process.
2. `/proc/self/exe`, if procfs works.
3. A filesystem candidate from `current_exe()`, then from the bytes of `AT_EXECFN`, resolved relative to the startup working directory.
4. A private filesystem relay, if needed.

The descriptor and path are tied to the current image's device and inode. AXE accepts a filesystem candidate only after canonicalizing it and checking that it is an executable regular file. If a descriptor is already known, AXE rejects a path to a different inode.

AXE validates an inherited descriptor and runtime root rather than trusting the handoff: it checks the live fd and its identity, and rechecks the root before use. It immediately sets `FD_CLOEXEC` on the descriptor and removes the internal handoff environment variables before starting any threads.

There is an unavoidable startup window. If Linux starts AXE only by pathname, procfs is unavailable, and the pathname is removed before `initialize()` opens the original inode, AXE cannot recover that inode. Only an executable fd inherited from the launcher closes this window completely.

AXE's retained descriptor is readable. This is stronger than the minimum exec-only capability and lets AXE create a relay. AXE does not treat an `O_PATH` or exec-only descriptor as an independent capability: being able to execute an inode does not prove that its bytes can be read and copied.

Darwin has no descriptor backend. Its capability is a verified canonical filesystem path.

## Linux self-exec order

Bundled child backends run in a fixed order:

1. **`execveat(fd, "", ..., AT_EMPTY_PATH)`** executes the retained descriptor directly.
2. **`fexecve(fd, ...)`** provides a libc/POSIX fallback.
3. **One checked filesystem path** points to the original inode if mount policy confirms execution, or to a private relay if the original is lost or on a `noexec` mount. If policy is `unknown` or unsupported, AXE tries the relay first, then the original.

AXE does not pass `/proc/self/fd/<fd>` to Brush as a plain path. Brush's child sanitation closes unrelated descriptors before `exec`, leaving that pathname dangling. A published bridge uses the selected original or relay path; live `/proc/<pid>/exe` is used only when there is no conventional path.

A failure in one descriptor backend does not stop the sequence. On some libc implementations, for example, `fexecve` can depend on `execveat` or procfs; that can fail on an older kernel or under a restrictive seccomp profile.

Before a descriptor launch, AXE duplicates the fd with `F_DUPFD_CLOEXEC`. It first tries numbers starting at 256 to avoid ordinary application descriptors, then falls back to numbers starting at 3 under a low `RLIMIT_NOFILE`. If the kernel does not support `F_DUPFD_CLOEXEC`, AXE uses `F_DUPFD` and sets `FD_CLOEXEC` separately.

The parent builds the complete C `argv` and `envp` arrays. The final `pre_exec` hook only clears `CLOEXEC` and calls syscalls: no allocation or Rust synchronization occurs after `fork`. Before falling back to a path, AXE opens it, compares the fd against the retained device and inode, and passes that checked fd to the next process.

Internal call sites register the descriptor hook last. A new `pre_exec` hook added after it must not call `close_range`, `closefrom`, or otherwise close the retained fd. If such sanitation is added, the descriptor must be explicitly allowlisted.

## Why AXE keeps a private filesystem relay

A retained descriptor is not enough when:

- the kernel, libc, or seccomp policy blocks descriptor execution;
- a consumer accepts only a path.

AXE creates a relay only on Linux and only from a readable descriptor. It selects one process-wide runtime root in this order: explicit `AXE_WORK_DIR`, a verified descriptor-backed parent root, `$XDG_CACHE_HOME/axe`, Linux `$HOME/.cache/axe` or Darwin `$HOME/Library/Caches/axe`, `$XDG_RUNTIME_DIR/axe`, then `$TMPDIR/axe-<uid>`. An automatically selected root is a private directory with mode `0700`; runtime selection does not search arbitrary mounts. If the selected root is on a `noexec` mount, the relay tries only the fixed `$XDG_RUNTIME_DIR/axe` and `$TMPDIR/axe-<uid>` locations. Explicit `AXE_WORK_DIR` disables this fallback.

Publication follows these steps:

1. AXE creates a user-owned `.axe-self` directory with mode `0700`, rejecting symlinks and other owners.
2. It opens a unique temporary regular file with mode `0700` using `create_new`.
3. It copies the image with bounded `read_at` calls without changing the source descriptor's offset.
4. It rechecks the source device, inode, size, and mtime.
5. It calls `fsync` and changes the mode to `0500`.
6. It reads the linker-generated SHA-1 ELF build ID and publishes a hard link at `.axe-self/<40 hex>`.
7. Where `flock` is available, it holds a shared lease on the accepted relay until the process exits.
8. It runs a hidden probe from the published file.
9. It accepts the path only if the probe succeeds.

The same ELF build ID produces one relay regardless of the source file's inode. Under an exclusive directory lock, AXE keeps the current relay and one previous relay and removes additional unlocked files. It does not remove a relay with a live process's shared lease; it collects that relay after the lease is released. If the filesystem or sandbox blocks `flock` (`ENOSYS`, `EOPNOTSUPP`, `ENOLCK`, `EPERM`, or `EACCES`), AXE uses `nolock-<40 hex>`. Normal garbage collection does not touch these relays, so publication works but they are not removed automatically.

A `noexec` mount, an unavailable or read-only root, a corrupt file, or a failed probe rules out that root. Once the roots are exhausted, AXE selects the checked original path if its execution policy is unknown.

The relay sits in a private directory, but path execution still leaves a window between the last inode check and `execve`. A retained descriptor is stronger; the relay is a controlled fallback for execution policy and path-only consumers, not a replacement.

A relay preserves AXE's executable bytes, not the original filesystem security object. Its new inode has none of the original file capabilities, LSM labels, security xattrs, or setuid/setgid semantics. AXE deliberately sets its mode to `0500`.

The bundled executable provider selects a fallback again before each launch. If the original path disappears between launches, AXE prepares the relay in the parent before `fork`: copying and filesystem mutations are unsafe inside `pre_exec`. A relay may be materialized even when descriptor execution later succeeds, because the path fallback must be ready before entering the child.

## Availability by launch state

<!-- markdownlint-disable MD013 -->

| State | Bundled child / daemon / SSH session | PATH bridge | `SHELL` | `AXE_SHELL` |
| --- | --- | --- | --- | --- |
| Linux, original path executable | Descriptor, then original path | Original path | Original path | Original path |
| Linux, original path `noexec` or lost, relay available | Descriptor, then relay | Relay | Relay | Relay |
| Linux, original path policy unknown, relay unavailable | Descriptor, then checked original path | Original path | Original path | Original path |
| Linux, original path removed, no relay, descriptor exec allowed | Descriptor-aware launch works | Absent, or an earlier bridge is dangling | Inherited value | Unset |
| Linux, descriptor exec blocked, relay and original path unavailable | Cannot start a new AXE process; Brush builtins still work | Absent | Inherited value | Unset |
| Darwin, canonical path exists | Path launch | Canonical path | Canonical path | Canonical path |
| Darwin, path removed or replaced | In-process dispatch only | Absent or dangling | Inherited value | Unset |

<!-- markdownlint-enable MD013 -->

`SHELL` and `AXE_SHELL` contain only real, publishable paths. AXE does not put `/dev/fd`, an internal fd number, or a fabricated path in either variable.

## Failure cases

### No procfs

A missing `/proc`, a masked proc mount, or an unusable `/proc/self/fd` disables only the proc backend. AXE obtains a retained descriptor whenever it can read the filesystem image. After the first descriptor launch, AXE hands that capability to later processes without procfs.

### Image removed or replaced

An open descriptor continues to refer to the original inode after unlink or atomic replacement. Descriptor-aware launch executes that inode's image; AXE rejects a filesystem path to a new inode.

An existing PATH bridge to the original path cannot survive unlink automatically. If AXE started without an original path, a new bridge requires a private relay.

### `execveat` unavailable or blocked by seccomp

`ENOSYS`, `EPERM`, and other `execveat` errors move execution to `fexecve`, then to one preselected checked path. That path is the original on a confirmed executable mount, or the relay if the original is lost or on `noexec`. When policy blocks descriptor syscalls but permits ordinary `execve`, bundled children, workers, and SSH sessions use the checked path.

If neither descriptor nor filesystem execution is allowed, AXE does not disguise the failure by running an external command from `PATH`. Starting a new process fails with status 126 or `NotFound`, depending on the caller.

### MDWE, W^X, and blocked `memfd_create`

Self-exec does not call `memfd_create`, use executable `mmap`, or call `mprotect(PROT_EXEC)`. AXE executes an already executable descriptor or an ordinary relay file. `PR_SET_MDWE` and W^X therefore need no separate workaround; blocking `memfd_create` does not affect self-exec.

AXE Store uses a sealed `memfd` to run single-file package payloads. It is not part of AXE self-exec.

### No writable executable filesystem

If AXE cannot create, fill, and execute a relay in any runtime root, the relay is unavailable. Descriptor-aware Brush, supervisor, and SSH launchers use the retained descriptor through `execveat` or `fexecve`.

After the original path is removed, a path-only consumer cannot launch. AXE returns a controlled error rather than substituting an external executable for a bundled command.

### Broken environment

An empty `PATH`, stale `AXE_APPLET_DIR`, invalid `AXE_WORK_DIR`, or inaccessible `HOME` leaves the in-process registry available. AXE removes a stale bridge marker and its exact PATH components. Failure to publish a new bridge is a warning; direct entry dispatch and Brush builtins run without it.

Descriptor handoff takes priority only after fd and identity checks. AXE ignores and removes an invalid environment value.

### Unable to create a process

Non-interactive/minimal Brush uses a current-thread Tokio runtime and starts without worker threads. Reedline needs a blocking pool, so the interactive shell uses a multi-thread runtime. If `clone` or `fork` returns `EAGAIN` or `ENOMEM`, or policy blocks it, that spawn fails; native builtins remain available in the running shell.

Ordinary bundled applets in Brush require a new process: the shim starts a child AXE process. `doctor` runs without spawn, including output through shell redirection. Other bundled applets cannot run in-process when spawning is blocked.

## Consumers

### Brush

The bundled shim receives either a Linux `BundledExecutable::Descriptor` or a checked filesystem path, then builds a `SimpleCommand`. It preserves `argv[0]`, the environment, and redirections, and adds descriptor execution last before spawn.

The builtin adapter waits for the child inside the builtin and does not receive the pipeline PGID from the dispatcher. A bundled pipeline stage therefore does not guarantee full pipeline concurrency or job-control semantics.

When self-exec is unavailable, Brush retains builtins, aliases, and functions; a bundled applet reports its own spawn failure.

### Supervisor

Daemon re-exec uses the same `ExecutableCommand`. Internal supervisor/worker flags, detached stdio, session setup, and log files are configured before descriptor execution. The launcher reports success only after the worker binds and signals through an inherited readiness descriptor. The supervisor owns the worker process group, forwards TERM/INT/QUIT/HUP, and waits for graceful shutdown. On Linux, the worker receives a parent-death signal, so it does not outlive an unexpected supervisor exit.

### SSHD

One `Arc<Executable>` is created before bind and shared by all shell/exec sessions. The Tokio wrapper takes the standard command only after the descriptor backend is fully prepared and preserves `kill_on_drop`.

The SSHD bridge is best-effort: failure to publish it does not prevent descriptor-aware session launch. If neither a descriptor nor an executable path is available at startup, however, `sshd` exits before bind because it cannot start sessions.

### Path-only consumers

Ordinary `execvp`, a BusyBox-style symlink, and an external launcher library accept only a path. They need a live proc bridge, a checked original path, or a relay. A retained descriptor alone cannot replace a PATH bridge.

Separately, `capsh --` opens the path from `AXE_SHELL` before changing capabilities and executes it through `fexecve`. This works while `AXE_SHELL` points to the original filesystem path.
