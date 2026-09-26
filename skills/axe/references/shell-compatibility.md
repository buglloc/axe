# AXE shell compatibility

Read this reference before writing unattended shell automation that uses
background jobs, traps, process substitution, Bash-specific expansion, or a
strictly isolated `PATH`. These boundaries describe the current AXE/Brush
runtime, not GNU Bash.

- [Background references](#background-references)
- [Validated automation baseline](#validated-automation-baseline)
- [Current compatibility boundaries](#current-compatibility-boundaries)

## Background references

Treat `$!` as an opaque reference:

- a decimal value is the PID of a real direct child;
- `%N` is a Brush job spec used when an asynchronous list has no safe OS PID;
- `wait "$ref"` accepts both forms;
- the Brush `kill` builtin accepts a decimal PID or `%N`, including numeric
  signal options such as `-0` and `-9`;
- never pass `%N` to `ps`, a directly dispatched or host `kill`, or another
  PID-only interface;
- an in-process `%N` job ends with its AXE shell, while an external child may
  outlive the shell.

Capture the first wait result. Completed jobs can remain visible as `Done`, and
a repeated `wait` on `%N` may not preserve the original failure status.

When `errexit` is active, disable it before launching the background job and
leave it disabled through the first wait. An in-process job inherits `errexit`;
if it fails, the parent AXE shell can exit before reaching a guarded `wait`.
Wrapping only `wait` in `if` is therefore insufficient:

```sh
set +e
COMMAND &
ref=$!
wait "$ref"
status=$?
set -e
```

Perform explicit cleanup after capturing `status`. Use plain `jobs` and
explicit `wait REF`. `jobs -l`, `wait -n`, and `wait -p` are currently
unimplemented and return status 99.

## Validated automation baseline

The real `axe` smoke path covers these behaviors:

- non-terminal stdin accepts complete multiline compound commands;
- `read` preserves UTF-8, and splitting on a non-whitespace `IFS` preserves
  interior empty fields;
- a quoted heredoc inside `$()` keeps its body literal;
- a numeric brace range preserves explicit width (`{01..03}` becomes
  `01 02 03`);
- `printf` with no consuming conversion emits its format once and terminates;
- a resolved but non-executable `PATH` candidate returns 126, while an
  unresolved command returns 127.

## Current compatibility boundaries

### Process substitution

Do not use `< <(...)` or `>(...)` in unattended automation. Input process
substitution can accept the syntax but fail to deliver producer output, causing
an unbounded reader to hang without a diagnostic.

For stream-only work, use a pipeline. When loop state must remain in the current
shell, use a non-guessable private file and explicit status precedence. For a
standalone route:

```sh
base=${AXE_WORK_DIR:-${TMPDIR:-/tmp}}
umask 077
tmp=$(mktemp "$base/axe-input.XXXXXX") || exit $?

set +e
producer >"$tmp"
status=$?
if [ "$status" -eq 0 ]; then
    while IFS= read -r line; do
        # Update current-shell state here.
        :
    done <"$tmp"
    status=$?
fi

rm -f -- "$tmp"
cleanup_status=$?
[ "$status" -ne 0 ] && exit "$status"
exit "$cleanup_status"
```

Bound `producer` explicitly when it can block. In a larger shell, preserve the
caller's `errexit` and `umask` state and return rather than exit. Never derive
the filename from `$$` alone.

### EXIT traps in nested shell environments

A top-level EXIT trap works, but a trap installed inside `( ... )` or `$()` can
be skipped. Do not depend on such a trap for lock release, temporary-file
removal, or final output. Perform the cleanup explicitly before leaving the
nested environment.

### Interactive suspension of bundled commands

Do not use `Ctrl-Z` followed by `jobs`/`fg` as a lifecycle mechanism for a
foreground bundled applet. The prompt can return without the stopped process
being registered as a Brush job; `jobs` is then empty and `fg` reports no
current job.

Start interruptible long-running work explicitly with `COMMAND &`, capture the
opaque `$!`, and use the Brush `kill` builtin plus one explicit `wait`. This
also works in non-interactive sessions and preserves a status the caller can
handle.

### Command provenance

Use both discovery surfaces:

```sh
commands NAME | jq '.command | {source, availability, local_path}'
type NAME
```

`commands` identifies AXE ownership. `type` reports current aliases, functions,
and resolution, but labels a bundled applet as a shell builtin. `command -v`
may print only an alias name rather than its definition, so do not use it for
provenance or alias recovery.

### Empty PATH

After AXE publishes its applet bridge, an initially empty `PATH` can contain a
trailing `:`. Under POSIX path search that empty component means the current
directory. Do not execute unresolved names from an untrusted working directory.

When a task explicitly requires no host or current-directory lookup, normalize
`PATH` after shell startup:

```sh
if [ -n "${AXE_APPLET_DIR:-}" ]; then
    PATH=$AXE_APPLET_DIR
else
    PATH=/nonexistent
fi
export PATH
```

This intentionally disables host-PATH fallback. Bundled shell resolution still
precedes `PATH`.