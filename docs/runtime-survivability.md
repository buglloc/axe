# Runtime survivability

AXE can keep running after its executable is removed. Whether it can start another AXE process depends on the platform and execution policy. Shell-launched applets, daemon workers, SSH sessions, and nested commands such as `xargs` need a child process.

## What remains available

Shell builtins, aliases, functions, and `doctor` run inside the current process and keep working without child execution. `doctor` still tries to start children for its probes. Ordinary bundled applets invoked from the shell need a child process.

On Linux, a running AXE process can still launch bundled applets after its executable is deleted or atomically replaced. A running session keeps using the original executable, not the replacement. If procfs is unavailable and the executable is deleted while AXE is still starting, recovery can fail.

On Darwin, child launches need the executable's path. Once it is removed or replaced, only in-process operations remain.

Child launches keep the applet name, argument and path bytes (including non-UTF-8 paths), environment changes, and redirections.

## Failure limits

| Condition | Result |
| --- | --- |
| Linux executable removed | Bundled commands launched by AXE can keep working. |
| Execution from a file descriptor is blocked | AXE launches through the original path or a private copy of its executable. |
| Original path removed; no private copy or procfs | Commands launched from the AXE shell may still work; programs that look up applets by path cannot start AXE. |
| No form of execution is allowed | New AXE processes fail; the running shell keeps in-process operations. |
| Process creation blocked or exhausted | Bundled commands fail; builtins and `doctor` remain callable. |
| PATH bridge cannot be published | The AXE shell still resolves bundled commands; external programs cannot find them on `PATH`. |

AXE does not substitute a host executable from `PATH` when a bundled applet cannot start. The shell reports such failures with status 126. Store commands follow separate fallback rules; see [Commands](commands.md).

The private copy needs a readable executable image and a writable filesystem that permits execution. A read-only root or a `noexec` mount can rule it out.

The copy does not keep the original file's capabilities, security labels, or setuid/setgid behavior, and its identity is weaker than that of the original file. Do not rely on those properties after the original is gone.

Bundled pipeline stages do not guarantee full pipeline concurrency or pipeline-wide job control. Do not assume they behave like external commands for signal delivery or large streaming pipelines.

`sshd` starts even when it cannot publish the PATH bridge, but refuses to start if it cannot launch sessions. A detached daemon reports success only after its worker is listening.

## Runtime storage and PATH

AXE stores command links and private executable copies in the first available runtime root:

1. `AXE_WORK_DIR`.
2. A runtime root inherited from a parent AXE process.
3. `$XDG_CACHE_HOME/axe`, or `$HOME/.cache/axe` on Linux and `$HOME/Library/Caches/axe` on Darwin when `XDG_CACHE_HOME` is not an absolute path.
4. `$XDG_RUNTIME_DIR/axe`.
5. `$TMPDIR/axe-<uid>`, or the platform temporary directory if `TMPDIR` is unset or relative.

Automatically selected roots are private and owned by the user. On Linux, if the selected root cannot hold an executable copy, AXE also tries the runtime and temporary directories. An explicit `AXE_WORK_DIR` turns that fallback off, so point it at a writable location that permits execution.

The PATH bridge lives in `<runtime-root>/.axe-bridge/bin`. Sessions that share a root share the bridge, and the most recent session to publish it decides which executable later path-based launches use. Give sessions separate `AXE_WORK_DIR` roots when they need separate bridges.

AXE sets `SHELL` and `AXE_SHELL` to a usable executable path when it has one. Otherwise it leaves the inherited `SHELL` unchanged and unsets `AXE_SHELL`. AXE shells and SSH exec sessions export `AXE=true` with or without a bridge.

Remove old runtime storage only after the AXE processes using it have exited. Store cache cleanup is separate; see [AXE Store](store.md).

## Diagnose a running session

Run these inside the affected AXE shell:

```bash
doctor --json
doctor --path /chosen/runtime/root --verbose
commands jq
jq -n '{ok: true}'
```

`doctor` reports shell state, launch candidates, relay and bridge availability, self-exec and process-creation probes, filesystem restrictions, and degradations. Its exit status tells you whether it wrote the report. Read the report to see which capabilities work. `--path` selects the directory for filesystem probes, not the runtime root. The probes create temporary files and child processes.

`commands NAME` shows whether an applet has a published `local_path`; the shell can launch the applet without one. The `jq -n` example starts a bundled applet as a child. If that works but an external program cannot find the applet, inspect `PATH`, `AXE_APPLET_DIR`, and the bridge. If child launch fails, check the self-exec and process-creation results, seccomp policy, runtime-root permissions, free space, and mount options.
