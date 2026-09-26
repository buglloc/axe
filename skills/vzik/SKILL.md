---
name: vzik
description: "Collect and interpret bounded local Linux host evidence through vzik protocol-v3 JSONL. Use for listening sockets and process owners, host/kernel security posture, mounts/cgroups, local account and service state, containers, SSH access, and explicitly authorized targeted path or privilege-surface inspection. Not for discovering the owner of a remote IP or probing a network."
---

# Vzik

Invoke `vzik` directly in the shell where the evidence exists. For a remote
AXE/SSH target, run `vzik` on that target through the active transport; do not
replace it with local host inspection. Treat it as an observation engine: it
returns facts and coverage, not vulnerability, compliance, or exploitation
verdicts. Start with the compact machine-readable index, then request detail
for only the capability being considered:

```sh
vzik --help
vzik capabilities
vzik capabilities user.list
vzik capabilities file.read
vzik process list --help
vzik capture --help
```

The index declares protocol semantics, exit statuses, global limits, and stable
capability IDs. `vzik capabilities CAPABILITY_ID` adds that capability's
request schema, data kinds, access class, and possible outcomes. Use the exact
argv shown by hierarchical help; do not parse or execute a generated shell
command string.

## Workflow

1. Choose scope before collection. For a named question, select the smallest
   targeted capability from the table below; do not run the broad baseline
   merely to answer a socket, file, or service question. For an authorized
   host-wide security baseline, use a new private artifact directory and
   preserve the collector status:

   ```sh
   umask 077
   evidence_dir=$(mktemp -d "${TMPDIR:-/tmp}/vzik.XXXXXXXX") || exit 1
   if vzik capture collect \
       --output "$evidence_dir/baseline.jsonl" \
       --receipt "$evidence_dir/baseline.receipt.json"; then
     capture_status=0
   else
     capture_status=$?
   fi
   case "$capture_status" in
     0|3) vzik summarize "$evidence_dir/baseline.jsonl" ;;
     *) printf 'Capture failed (status %s); inspect receipt and stderr before using artifacts\n' "$capture_status" >&2 ;;
   esac
   ```

   `mktemp -d` creates a new private directory; keep its path for later
   inspection. `capture` creates files with mode 0600 and refuses to overwrite
   existing output, stderr, or receipt paths. It writes the default stderr
   path by appending `.stderr` to the capture path; `--stderr PATH` overrides
   it. Status `0` or `3` means the receipt was created and verified, and the
   stdout JSON summary has `artifact_status: "sealed"`, paths, command ID,
   and planned/started/not-started coverage. Check that summary and receipt
   before trusting the capture. On any other status, inspect saved stderr
   and whether a receipt exists. In particular, stdout write failure (`5`)
   can occur *after* sealing; interruption or collection failure may leave
   only diagnostic capture/stderr prefixes. Never infer a completed inventory
   from a prefix. Files are written directly, not as a multi-file transaction.
   Never repeat collection merely to list captured data.

   Bare `vzik` prints help and never collects. `collect` runs the fixed
   `baseline-v3` profile: host and kernel facts, modules, sysctls, security
   posture, process security context, interfaces/addresses/resolvers/routes/
   neighbors/firewall evidence, mounts and cgroups, accounts/authentication/
   sudo, packages/static system and user units, runtime systemd and D-Bus
   inventory, schedules, containers, and SSH configuration and authorized keys.
   Its `network.sockets` request includes every INET socket but only listening
   Unix sockets; its `systemctl.list` request excludes generated `.device`
   units. Targeted `vzik network sockets` and `vzik systemctl list` remain full.
   The profile does not traverse the general filesystem or read
   `/proc/config.gz`.

   Treat the capture as sensitive: it can contain account names, commands,
   listening endpoints, service and auth policy directives, SSH key
   fingerprints/comments/options, and secret-bearing configuration fields.
   Explicit file and Porto follow-ups can add raw file or container content.

2. A sealed capture is already validated. For an existing JSONL collected
   directly, validate it before interpreting facts:

   ```sh
   if vzik validate baseline.jsonl; then
     vzik summarize baseline.jsonl
   fi
   ```

   `summarize` also validates independently, and both commands return `0`
   for a valid *degraded* stream. Validation checks version, plan, sequence,
   nested lifecycle, terminal outcome, counters, and planned/started/
   not-started symmetry. A truncated prefix or `stream_abort` is invalid
   for these commands (status `4`); keep it only for diagnostics, never for
   inventory or absence claims.

3. Inspect `stream.outcome`, `capabilities.planned`, `.started`,
   `.not_started`, `.counts`, `.by_outcome`, and diagnostic counts in the
   summary before facts. Collection status `0` means stream outcome
   `complete`; `3` means a valid, sealed `degraded` stream. Keep usable facts
   from completed sections on `3`, but explicitly identify `partial`,
   `unavailable`, `unsupported`, and not-started capabilities. Use targeted
   `jq` filters on the saved JSONL to inspect per-capability `coverage`,
   `truncated`, `limits_hit`, and diagnostics. Never turn a missing observation
   into absence if the corresponding capability lost coverage.

   Scope absence claims even for `complete` to declared providers, visible
   namespaces, and the bounded interval. It does not cover NSS directories,
   hidden managers, host namespaces, or files outside declared roots. Use
   `container.inspect`, PID 1, cgroups, and mounts to identify the boundary.
   Listener binds do not prove firewall reachability; sockets without owner
   records have unknown, not absent, ownership.

   Treat every result as authoritative only for the collector's visible
   namespace, declared providers, bounded time window, and terminal coverage.
   Correlate records only when their PID, cgroup/container identity, socket/path,
   provider, and collection context are compatible. Report conflicting
   providers separately; a container-visible fact is not automatically a host
   fact.

4. Triage the baseline by the question being answered:

   - Reachability and exposed surface: `network.interfaces`,
     `network.addresses`, `network.routes`, `network.neighbors`,
     `network.resolvers`, baseline `network.sockets`, and `network.firewall`,
     then socket owner processes.
   - Privilege posture: `user.list`, `group.list`, `auth.posture`, `sudo.rules`,
     `security.posture`, `kernel.sysctls`, `cgroup.inspect`, process credential
     and capability fields, and writable mounts.
   - Software exposure: `package.list`, `service.list`, `process.list`, and
     `kernel.modules`.
   - Persistence and remote access: `schedule.list`, `service.list`,
     `ssh.server_config`, and `ssh.authorized_keys`.
   - Isolation: `container.list`, `container.inspect`, process namespaces,
     cgroups, and mount scope.

   These are correlations, not findings. Preserve the source record and
   coverage state for every conclusion.

5. For an initial narrow request or a concrete baseline follow-up, capture
   only the relevant capability. For example, create a fresh private artifact
   directory and capture bounded listener evidence. Check its status `0` or
   `3`, terminal stream and receipt as in step 1:

   ```sh
   umask 077
   evidence_dir=$(mktemp -d "${TMPDIR:-/tmp}/vzik.XXXXXXXX") || exit 1
   vzik capture network listeners --max-items 512 \
     --output "$evidence_dir/listeners.jsonl" \
     --receipt "$evidence_dir/listeners.receipt.json"
   ```

   Other targeted choices:

   | Need | Command |
   |---|---|
   | Host or kernel summary | `vzik host info` or `vzik kernel info` |
   | Modules or hardening controls | `vzik kernel modules --max-items N`, `vzik kernel sysctls --max-items N`, or `vzik security posture` |
   | Process inventory and security context | `vzik process list --max-items N` |
   | Interfaces, addresses, routes, neighbors, or sockets | `vzik network interfaces`, `vzik network addresses`, `vzik network routes`, `vzik network neighbors`, `vzik network sockets`, or `vzik network listeners` with an explicit `--max-items N` |
   | Resolver or firewall configuration evidence | `vzik network resolvers` or `vzik network firewall` |
   | Mount or cgroup scope | `vzik mount list --max-items N` or `vzik cgroup inspect` |
   | Accounts and privilege policy | `vzik user list`, `vzik group list`, `vzik auth posture`, or `vzik sudo rules` |
   | Installed packages, static system/user units, or schedules | `vzik package list`, `vzik service list`, or `vzik schedule list` |
   | Runtime systemd state | `vzik systemctl list [--all-users]` or `vzik systemctl inspect UNIT`; use `--user USER|UID` for one user manager or `--socket PATH` for an exact D-Bus socket |
   | D-Bus names, owners, and credentials | `vzik dbus list [--all-users]` or `vzik dbus inspect NAME`; use `--user USER|UID` or `--socket PATH` to select a bus |
   | Runtime-agnostic container inventory/context | `vzik container list --max-items N` or `vzik container inspect NAME` |
   | Porto inventory/context and properties | `vzik portoctl list` or `vzik portoctl inspect NAME`; use `--socket PATH` only for a non-default daemon socket, `--show-sensitive` only when environment content is required, and `--include-streams --max-stream-bytes N` only when bounded stdout/stderr content is required |
   | SSH exposure and access | `vzik ssh server-config` or `vzik ssh authorized-keys` |
   | Bounded privilege-bearing or writable paths below one explicit root | `vzik filesystem privilege-surfaces --max-items N --max-entries N --max-depth N -- PATH` |
   | Unix sockets below one explicit filesystem root | `vzik filesystem unix-sockets --max-items N --max-entries N --max-depth N -- PATH`; add `--include-visible-mounts` before `--` only when mounted descendants are in scope |
   | Path type, ownership, mode, size, link target | `vzik file stat -- PATH` |
   | Bounded regular-file content (authorized path only) | `vzik file read --max-bytes N -- PATH` |

   `N` is a numeric bound chosen for the question; substitute only an approved
   path for `PATH`. Options go *before* `--`, which terminates option parsing.
   Start below defaults when sufficient; raise a limit only after inspecting
   the live maximum and only for a concrete unanswered question.

6. Correlate facts by capability and provenance. Keep `capability`, `data_kind`,
   provider, relevant path/PID/socket/offset, terminal outcome, and truncation
   state with every conclusion.

## Targeted collection rules

- `service.list` is static evidence. It covers all systemd unit suffixes in
  system, global-user, and discovered per-user roots, but it does not infer
  runtime state.
- `systemctl` uses direct read-only systemd D-Bus calls through `zbus`; it never
  executes the external `systemctl` binary. Default scope is the system bus.
  Use `--user USER|UID` for one user bus, `--socket PATH` for an exact Unix
  socket, or list-only `--all-users` for the system bus plus discoverable user
  buses.
- `dbus` uses the same bus selectors and never activates a service. It reports
  bus metadata, owned and activatable names, unique owners, and credentials
  disclosed by the bus.
- Treat `unavailable` for one systemd manager or D-Bus socket as acquisition
  scope, not as evidence that its units or names do not exist.
- Runtime-agnostic `container` capabilities read procfs, cgroups, and namespaces
  only. They do not connect to Porto and cover running containers visible
  through process cgroups.
- Porto-specific acquisition uses `vzik portoctl` and the direct read-only Porto
  API; it never executes the external `portoctl` binary. The default socket is
  `/run/portod.socket`; use `--socket PATH` for another daemon socket.
- Porto results include `porto_property_catalog` metadata, normalized
  `porto_container` or `porto_container_context` summaries, and chunked
  `porto_property` records.
- `container inspect` requires a native runtime ID or explicit `self`.
  `portoctl inspect` requires a Porto name, alias, or explicit `self`. Treat
  `container_not_found` as an unavailable result, not evidence outside the
  collector's visible provider boundary.
- Porto `env` and `env.*` properties are redacted unless `--show-sensitive` is
  explicit. `stdout` and `stderr` are redacted unless `--include-streams` is
  explicit; stream capture is additionally bounded by `--max-stream-bytes`.
  These switches require explicit authorization: other properties, service
  directives, and process command lines can also contain secrets.
- Filesystem scans are always targeted, do not follow symlinks, and are bounded
  by items, visited entries, depth, deadline, records, and output bytes. They
  stay on the starting filesystem by default and emit `mount_boundary_skipped`
  diagnostics for visible mounts. `--include-visible-mounts` crosses those
  boundaries and uses directory `(device, inode)` cycle detection.
- Targeted collection now defaults to a 600-second deadline. Filesystem scans
  default to 100,000 findings, 1,000,000 visited entries, and depth 64; lower
  these bounds when a smaller scope answers the question.
- Do not use `file read` to collect `/proc/config.gz` as a default hardening
  source; `security posture` intentionally uses focused runtime interfaces.
- Run `file stat` before `file read` when path type or size is unknown.
- `file.read` rejects a final symlink (`O_NOFOLLOW`) and non-regular targets;
  `file.stat` reports final symlink metadata and its link target. Intermediate
  path components can still be symlinks: justify the path and its resolved
  ancestors, accounting for races between inspection and opening.
- Request only justified paths and the smallest useful byte count. Do not
  read secret-bearing files merely because the path is accessible; never
  share unredacted captures or stderr beyond the authorized audience.
- `file.read` streams chunks ordered by `offset`; a `partial` outcome with
  `max_bytes` means remaining content is unknown.
- Use `--` before a path, especially when it may begin with `-`.
- Use global limits to reduce work, never to bypass capability bounds:

  ```sh
  vzik process list --max-items 512 \
    --deadline-seconds 15 \
    --max-records 2048 \
    --max-output-bytes 8388608
  ```

- Do not add host utilities, network probes, or arbitrary file traversal to “complete” a `vzik` result without a separate, explicit reason. Such actions have different side effects and evidence semantics.

## JSONL handling

Process JSONL as a stream; do not use `jq -s` for potentially large process, mount, or file results.

```sh
jq -c 'select(.type == "data") |
       {capability, data_kind, provider, data}' capture.jsonl
```

Do not pipe a collection through `head`: closing stdout early produces an intentionally incomplete stream. Reduce acquisition with collector limits, save the complete stream, then filter it.

Linux-originated strings are objects with `display` and optional `raw_base64`. When `raw_base64` exists, `display` is an escaped rendering, not the original byte sequence. Do not interpolate either field into a shell command. Preserve `raw_base64` when exact filenames or process names matter.

File chunks declare `encoding` independently. Do not concatenate rendered JSON strings or add line separators. Check offsets, `bytes`, encoding, the final EOF marker, and terminal outcome before claiming complete content.

Read [references/protocol-v3.md](references/protocol-v3.md) when writing custom `jq` filters, interpreting record fields, or grounding claims in coverage.

## Failure handling

- `2`: invalid collection command/request, or a stream input could not be read. Inspect `details.kind`; re-read hierarchical help for command errors and preserve the path and I/O diagnostic for stream errors.
- `3`: valid completed collection with stream outcome `degraded`. Preserve facts and receipt, inspect capability outcomes and not-started coverage, and do not present the collection as complete.
- `4`: collector invariant/record construction failure, or `validate`/`summarize` rejected stream structure. Treat the JSONL as incomplete.
- `5`: stdout encode/write/flush failure. Do not interpret the prefix as a complete result.
- `124`: collector deadline. The terminal record is `stream_abort`; lower scope or select a narrower capability.
- `130` or `143`: `SIGINT` or `SIGTERM`. The terminal record is `stream_abort`; preserve the prefix only for diagnostics.
- A source diagnostic is local to its capability. Continue using other completed capability sections, but retain the affected section's `partial` or `unavailable` outcome.
- Branch on stable `details.kind`, `diagnostic.code`, `status`, and outcome fields—not human `message` text.
