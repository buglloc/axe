# Runtime survivability

AXE can keep running after its executable is removed, but starting another AXE process depends on the platform and execution policy. This applies to bundled applets launched from Brush, daemon workers, SSH sessions, and nested commands such as `xargs`.

## What remains available

Direct applet dispatch in an already running process does not need the executable path. Shell builtins, aliases, and functions also remain available. Ordinary bundled applets invoked from Brush require a child process; `doctor` runs in the shell process, although its diagnostic probes attempt to create children.

On Linux, AXE retains access to its executable at startup and can use it after deletion or atomic replacement. A running session keeps using its original executable, not the replacement. Without procfs or an inherited executable image, deletion before startup finishes can prevent recovery.

Darwin relies on the executable's filesystem path. Removing or replacing it leaves only in-process operations available.

Child launches preserve the applet name, argument and path bytes (including non-UTF-8 paths), the caller's environment changes, and stdin/stdout/stderr redirections.

## Failure limits

| Condition | Result |
| --- | --- |
| Linux executable removed; descriptor execution allowed | Descriptor-aware child launches can still work. |
| Descriptor execution blocked; original path or private executable copy available | AXE tries ordinary path execution. |
| Original path lost; no usable copy or procfs path | Bundled child launches may work, but path-only consumers cannot launch AXE. |
| Neither descriptor nor path execution allowed | New AXE processes fail; the running shell retains in-process operations. |
| Process creation blocked or exhausted | Bundled child commands fail; builtins and `doctor` remain callable. |
| PATH bridge cannot be published | Brush still resolves bundled commands; external programs cannot rely on the bridge. |

AXE does not substitute a host executable from `PATH` when a bundled applet cannot start. Shell execution failures use status 126; other launchers report their own errors. Store delivery has separate fallback rules; see [Commands](commands.md).

Creating a private executable copy requires a readable image and a writable filesystem that permits execution. Read-only roots and `noexec` mounts can prevent this fallback. Store tool delivery has separate storage and execution requirements.

Bundled pipeline stages do not currently guarantee full pipeline concurrency or pipeline-wide job control. Do not assume they behave like external commands for signal delivery or large streaming pipelines.

`sshd` treats PATH publication as best-effort, but refuses startup if it cannot launch sessions. Detached daemon startup reports success only after the worker binds and signals readiness.

## Runtime storage and PATH

AXE uses a runtime root for command links and private executable copies. It selects:

1. Explicit `AXE_WORK_DIR`.
2. A verified runtime root inherited from a parent AXE process.
3. `$XDG_CACHE_HOME/axe`, or the platform's home cache when `XDG_CACHE_HOME` is not an absolute path: `$HOME/.cache/axe` on Linux, `$HOME/Library/Caches/axe` on Darwin.
4. `$XDG_RUNTIME_DIR/axe`.
5. `$TMPDIR/axe-<uid>` (the platform temporary directory if `TMPDIR` is unset or relative).

Automatic roots are private, user-owned directories. On Linux, executable-copy creation can also try the runtime and temporary locations if the selected root fails. Explicit `AXE_WORK_DIR` disables that fallback; choose a writable, executable location when path-based launches must survive removal of the original.

The bridge is at `<runtime-root>/.axe-bridge/bin`. Sessions sharing a root share its command links and executable target; the last publisher controls later path-based launches. Use separate `AXE_WORK_DIR` roots when sessions need separate bridges. Bridge updates are not atomic across the whole command inventory. Do not remove `.axe-bridge/.lock` while AXE is running.

Command links to the original executable can stop working after deletion. New links can use a private copy or, on Linux, a live `/proc/<pid>/exe` path. A procfs path lasts only while that process and procfs remain available.

AXE sets `SHELL` and `AXE_SHELL` to a usable executable path when one is available. Otherwise, it leaves inherited `SHELL` unchanged and unsets `AXE_SHELL`. AXE-managed shells and SSH exec sessions export `AXE=true` even without a bridge.

Remove old runtime storage only after the AXE processes using it have exited. Store cache cleanup is separate; see [AXE Store](store.md).

### Executable-copy security

A private executable copy does not preserve the original file's capabilities, security labels, or setuid/setgid behavior. Do not rely on those properties after path-based recovery. Private storage reduces tampering risk, but path-based execution has weaker identity guarantees than descriptor execution.

## Diagnose a running session

Run these inside the affected AXE shell:

```bash
doctor --json
doctor --path /chosen/runtime/root --verbose
commands jq
jq -n '{ok: true}'
```

`doctor` reports live shell state, launch candidates, relay and bridge availability, a self-exec probe, process-creation results, filesystem restrictions, and degradations. Its exit status reports whether the diagnostic output was written, not whether every capability works; inspect the report. `--path` changes the filesystem probe directory, not the selected runtime root. Probes can create temporary files and child processes, so the report is not a passive inspection.

Use `commands NAME` to check whether an applet has a published `local_path`; that is separate from Brush's ability to launch it. The final example exercises a bundled child directly. If direct launch works but an external program cannot find the applet, inspect `PATH`, `AXE_APPLET_DIR`, and the bridge target. If child launch fails, check the self-exec and process-creation results, seccomp policy, runtime-root permissions, free space, and mount execution policy.
