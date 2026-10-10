---
name: axe
description: "Operate an explicitly identified or newly detected AXE environment: axe CLI, SSH sessions served by axe sshd, AXE=true, AXE_APPLET_DIR, axe: diagnostics, axe_commands/axe_doctor output, bundled and Store commands, Brush jobs, and launch recovery. Use only after AXE-specific signals; an ordinary local or SSH shell is not AXE."
---

# AXE environment

Use this skill when the user or active workflow selects AXE, or immediately
after an initially unknown shell exposes `AXE=true`, `AXE_APPLET_DIR`, `axe:`
diagnostics, or `axe_commands`/`axe_doctor` output. A generic `commands` or
`doctor` name is not by itself an AXE signal. The operator may run `axe`
locally or control an AXE-backed target from another shell; this skill does
not select the target, transport, authorization, or evidence policy.

Apply the shell rules below only after the command context is identified as
an AXE Brush shell, local or served by AXE `sshd`. Run registered commands
directly:

```sh
jq FILTER
rg PATTERN PATH
commands | jq '.commands'
```

Do not prefix a registered shell command with `axe`. The `axe APPLET` multicall
interface is for explicit invocation outside the AXE shell.

## Command resolution and PATH bridge

Account for resolution rather than treating every `PATH` hit as the command
that runs in Brush:

```text
alias/function → shell builtin → bundled applet → AXE Store → host PATH only after eligible Store failure
```

An unregistered name can still resolve through host `PATH`; a registered
bundled command takes precedence even if it supports fewer options.

An AXE-managed shell and SSH exec environment exports `AXE=true` independently
of PATH bridge publication. Treat that exact value as the canonical environment
marker; use `doctor --json` for version and runtime evidence.

At startup AXE makes a best-effort BusyBox-style bridge, exports its directory
as `AXE_APPLET_DIR`, and prepends it to `PATH`. The bridge can contain dispatch
links for bundled and Store commands. Its presence makes registered commands
available to child programs that perform filesystem `PATH` lookup; it does not
mean that every linked command is already local or cached.

Use all three discovery surfaces when provenance or implementation matters:

```sh
commands NAME | jq '.command | {name, source, availability, local_path}'
type -a NAME
which -a NAME
```

- `commands NAME` reports AXE ownership and Store availability without running
  the command;
- `type -a NAME` shows aliases, functions, shell resolution, and filesystem
  candidates; Brush can label any registered AXE applet shim, including a Store
  command, as a shell builtin;
- `which -a NAME` lists filesystem candidates in `PATH` order. Its first result
  can be `$AXE_APPLET_DIR/NAME`, which proves only that a bridge link exists.

An unknown result and status 127 from `commands NAME` do not mean that no host
command exists. Continue with `type -a` and `which -a`. Conversely, a Store
record with `local_path: null` can still have a bridge link that performs
on-demand delivery when invoked.

When the task explicitly requires a host implementation of an AXE-owned name:

1. preserve the `commands NAME`, `type -a NAME`, and `which -a NAME` results;
2. inspect candidates after `$AXE_APPLET_DIR/NAME`; do not automatically trust
   the first one or another path that resolves to the AXE executable;
3. select a concrete filesystem candidate, record its absolute path, and check
   that implementation with its own `--help` or `--version`;
4. invoke the absolute path so aliases, functions, registered applets, and
   ordinary `PATH` lookup cannot replace it.

Do not globally reorder `PATH`, unset `AXE_APPLET_DIR`, or remove the bridge just
to run one host command. That can break scripts and child processes that need
AXE applets. If the bridge is absent, direct shell resolution can still work
while a child program's `PATH` lookup fails. Inspect the current state with:

```sh
doctor --json | jq '.launch.bridge'
```

A registered Store tool uses a same-named host `PATH` executable only if
delivery fails with the Store `transient` or `unavailable` class (for example,
network/storage failure, offline cache miss, or an artifact removed for this
target). AXE skips its own bridge and executable while searching; the chosen
host executable's status is returned, and stderr identifies the fallback path.
Without a runnable host candidate, that Store failure returns 126. Integrity
(including signature, digest, schema, and TLS) and configuration errors return
126 without automatic fallback. A command absent from the Store registry is
not a Store delivery failure; inspect host lookup separately.

An explicitly selected host implementation is a separate command: never
present it as recovery of a hard Store failure or evidence that the Store
artifact ran successfully. If a bundled command lacks a needed option, use
an explicitly selected host implementation with verified semantics; it does
not automatically fall through to `PATH`.

## Discover available commands

Read the versioned command inventory instead of relying on a remembered tool
list:

```sh
commands | jq '.commands[] | {name, source, availability, local_path, category, alias_of, synopsis}'
commands rg | jq '.command'
```

`commands` emits the compact `axe_commands` v1 JSON schema. Its top-level list
document contains only `schema`, `schema_version`, and `commands`. Each command
entry reports its name, canonical name, `bundled` or `store` source, category,
optional alias target, synopsis, availability, and optional local path.
`commands NAME` describes one registered command without running it; an unknown
name emits a structured JSON error and returns status 127.

Interpret availability precisely:

- `local`: a bundled command is registered locally; `local_path` names its
  published PATH-bridge entry when one currently exists;
- `on_demand`: a Store command is registered without a known blocking Index
  error, but delivery, cache, storage, or execution may still fail;
- `blocked`: a blocking Store error prevents that Store command from executing.

`local_path` is a UTF-8 string, `{ "base64": "..." }` for a non-UTF-8 path, or
`null`. It is populated only for bundled commands with a currently published
bridge entry. A null path does not make a bundled command unavailable:
in-process or descriptor-backed launch can work without a published path. A
Store command also keeps `local_path: null` when the PATH bridge contains its
on-demand dispatch link.

The inventory excludes executables found only through host `PATH` and aliases
or functions created later in the live shell. Use `commands NAME` for AXE
ownership and the resolution workflow above for the live shell.
Do not use `command -v` to recover an alias definition; it may print only the
name. Use `doctor` rather than inventory fields for launch capabilities,
filesystem policy, and runtime evidence.

## Diagnose runtime constraints

Use `doctor` for runtime evidence. Human output is concise by default: read
`Summary` first, then `Attention` for negative active results, runtime impact,
and recovered or unrecovered degradations. Use `--verbose` for launch
candidates, the full `Active checks` section, absent environment variables,
empty detail sections, and individual probe stages. JSON is always the complete,
versioned `axe_doctor` v4 schema:

```sh
doctor
doctor --verbose
doctor --json | jq '{launch, shell, isolation, restrictions, capabilities, filesystem, degradations}'
doctor --path "${AXE_WORK_DIR:-.}" --json | jq '.filesystem'
```

`doctor` is a native shell callback. It remains usable when a bundled applet
cannot spawn, and its shell-scoped report contains the live cwd, shell flags,
and exported variable names. Verbose human output separates passive `Observed
restrictions` from `Active checks`. Active probes cover self-exec, fork,
`memfd_create`, acceptance of `MFD_EXEC`, anonymous RW→RX mapping, macOS
`MAP_JIT|RX`, and private filesystem create/write/sync/cleanup. Treat only the
RW→RX and `MAP_JIT|RX` results as executable-mapping evidence; successful
`memfd_create` or an accepted `MFD_EXEC` flag is not an execution verdict.
Filesystem output reports whether the selected directory is writable and
whether its mount has `noexec`; it does not depend on `/bin/sh` or claim a
separate filesystem execution probe.

The environment section covers a fixed runtime-oriented set: `HOME`, user/shell
paths, `PATH`, `AXE_*`, runtime/temp directories, locale, and terminal
variables. Default human output omits absent variables; verbose and JSON retain
their explicit `absent` observations. Unrelated variables are omitted. Treat
the selected values as diagnostic context; do not request a broader environment
dump.

Interpret every observation by `status`, not by field presence:

- `available` may carry `false`; it is still an observed answer;
- `absent` means evidence established nonexistence;
- `unavailable` includes a structured operation/class/code/errno/message error;
- `unsupported` and `not_applicable` are known platform or scope boundaries;
- `unknown` means evidence was insufficient, never success;
- active probes are wrapped in `state`; inspect their nested `observation`.

Every `.isolation` layer is a `heuristic_indicator`, not a complete inventory
or an absence verdict. Correlate it with Vzik container, namespace, cgroup, and
mount evidence. Compare process-scoped fields only when PID and namespace IDs
match; native `doctor` describes the parent Brush process, while a spawned Vzik
collector describes its own process. Preserve conflicting scoped observations
instead of choosing one.


## Rescue failed applet launches

After an applet reports a launch, descriptor, procfs, spawn, or executable-mount
failure:

1. Preserve the exact command, status, and stderr.
2. Run standalone `doctor --json` in the same live shell; do not use a pipeline,
   command substitution, background job, or replacement shell. Plain output
   redirection is safe when a report file is needed.
3. Inspect `.launch.self_exec`, `.capabilities.process`,
   `.filesystem.mount_execution_policy`, and `.degradations`.
4. Continue with aliases, functions, and native shell builtins when child process
   creation is unavailable. `doctor` itself is safe on this path.
5. Choose a substitute only when the report proves the required capability.
   Never treat a missing `/proc` path as proof of `noexec`, or a failed
   `memfd_create` probe as proof that filesystem execution is blocked.

`degradations` is bounded current-process state: each entry names the exact
bundled command when applicable, failed component and operation, stable
structured failure fields, selected fallback, and whether a later bundled
launch recovered. It is not an event log.

## Tool selection

Prefer the narrowest available command reported by `commands`:

- files and metadata: `find`, `fd`, `file`, `stat`, `du`, `df`;
- text: `rg`, `grep`, `sed`, `awk`, `sort`, `uniq`, `cut`, `tr`;
- structured data: bundled `jq` for JSON and AXE Store `yq` for YAML/XML/JSON;
- HTTP/API probing: bundled `http`; Store `curl` for extension methods, HTTP/2, multipart, and larger transfer workflows;
- archives: `tar`, `gzip`, `gunzip`, `zcat`;
- processes: `ps`, `pgrep`, `pkill`, `pidof`, `kill`, `watch`, `free`, `uptime`;
- binary triage: `file`, `goblin`, then bounded `od` where raw bytes are necessary;
- hashes and encodings: `sha256sum`, other checksum applets, `base64`, `base32`, `basenc`.

Before using `http` for HTTP/API probing, read the bundled
[HTTP guide](references/http.md). Its compact `axe_http` v1 JSON is the default
output. A completed bounded response, including `4xx`/`5xx`, exits 0; body and
redirect failures can occur after response headers, so inspect both exit status
and the document. Redirect and limit evidence semantics are documented there.

Before using `goblin` for executable/object inspection, read the bundled
[Goblin guide](references/goblin.md). Treat the input as untrusted data and never
execute it merely to identify it. A bounded first pass inside the current shell
is:

```sh
file -- "$file"
goblin "$file" | jq '{file, format, architecture, object_type, security}'
goblin "$file" --imports --limit 256 | jq '.imports'
```

Prefer Goblin's JSON to scraping human-formatted binary metadata. ELF import
pagination is under `.imports.symbols`; PE import pagination is directly under
`.imports`. Check the applicable `truncated` field and page with `--skip` and
`--limit` before making an absence claim.

For format-specific fields, paging, and address translation, read the
[Goblin output schema](references/goblin-output-schema.md).

## On-demand commands

AXE Store tools are signature- and checksum-verified artifacts rather than code
linked into the shell. Query current ownership with `commands NAME`; do not
maintain a static list in the skill.

An initial invocation may fetch the signed metadata and artifact; later
invocations may use the verified offline cache. `AXE_STORE_MODE=cache-only`
uses only that cache (an uncached artifact may trigger eligible host fallback);
`off` does not register Store commands. If a Store invocation fails before its
own usage output appears, preserve the diagnostic and distinguish delivery
failure from the fallback host's output/status. Never bypass signature or
checksum verification, or claim that host fallback ran the Store payload.
Platform-specific coverage and registered names come from `commands NAME`.

### Cache lifecycle

Leave the verified cache intact after normal Store use: it can be needed
offline. Do not put a printed internal cache path on `PATH` or run its payload
directly. `refresh-tools` forces an Index fetch and verification when a fresh
Index is needed; it can require network access. `clean-tools` is an explicit
cache-maintenance operation, **not** session cleanup: it removes cached Index
and manifest metadata for the current Store identity across candidate storage
roots, potentially losing offline availability until metadata is restored.
Use it only for a requested cache reset after assessing that side effect;
it does not remove shared content-addressed objects.

## Shell behavior

This is a Bash/POSIX-oriented Brush shell, not GNU Bash itself. Before using
background jobs, traps, process substitution, or Bash-specific expansion in
automation, read
[`references/shell-compatibility.md`](references/shell-compatibility.md).

For portable agent automation:

- prefer POSIX shell constructs unless a Bash feature is required;
- do not use process substitution such as `< <(...)`; input delivery can fail
  silently or hang;
- do not rely on an EXIT trap installed inside `( ... )` or `$()`; perform
  cleanup explicitly in that scope;
- manage long-running bundled commands with explicit `&` plus `$!`; do not use
  `Ctrl-Z` followed by `jobs`/`fg` as their lifecycle mechanism;
- use explicit quoting and `--` before path operands where supported;
- consume structured JSON with `jq` instead of parsing display text;
- propagate nonzero statuses rather than masking them;
- use bounded output and bounded waits;
- never assume unavailable administration or networking commands exist on the host.

After `COMMAND &`, inspect `$!` as an opaque background reference. A decimal
value is a real child PID; `%N` is a shell job spec, not a PID. Pass either to
`wait`. The Brush `kill` builtin accepts both forms, but external PID-oriented
commands do not understand `%N`. For `errexit`, first-wait status handling, and
the validated non-interactive parsing/expansion baseline, read
[`references/shell-compatibility.md`](references/shell-compatibility.md).

## Failure handling

- Status 2 generally indicates usage or parse failure; inspect stderr before retrying.
- Status 5 from `commands` or `doctor` indicates that its output could not be written.
- Status 99 reports a recognized shell operation that Brush has not implemented.
- Status 126 means a resolved command could not execute.
- Status 127 means no usable command was found; `commands NAME` also uses it for an unknown registered command while emitting JSON.
- A SIGPIPE/BrokenPipe success path is normal when a producer is intentionally shortened by a consumer such as `head`.
- Before replacing a failed command, confirm that the substitute preserves required output, exit status, and side effects.

## Maintenance

Routing and shell-handoff regressions are covered by
[`evals/evals.json`](evals/evals.json).
