# Vzik protocol v3 reference

Use this reference for discovery, stream validation, capture receipts, and JSON
queries. Use hierarchical `vzik ... --help` output for current commands,
arguments, defaults, and bounds.

## Contents

- [Discovery and process status](#discovery-and-process-status)
- [Stream state machine](#stream-state-machine)
- [Outcomes and coverage](#outcomes-and-coverage)
- [Baseline profile scope](#baseline-profile-scope)
- [Data records](#data-records)
- [Capture receipt](#capture-receipt)
- [Linux byte strings](#linux-byte-strings)
- [Useful streaming queries](#useful-streaming-queries)

## Discovery and process status

`vzik capabilities` returns a compact index with protocol semantics, exit
statuses, global limits, and stable capability IDs. `vzik capabilities
CAPABILITY_ID` returns request schema, data kinds, access class, baseline
position, and possible outcomes for exactly one capability.

Collection process status and terminal state are one contract:

- `0`: `stream_end.outcome` is `complete`;
- `3`: `stream_end.outcome` is `degraded`, but the stream remains valid and its
  findings remain usable within reported coverage;
- `2`: invalid CLI request or unreadable validation input;
- `4`: internal invariant failure, invalid stream, or capture artifact I/O
  failure (including an existing output path);
- `5`: stdout write or flush failure;
- `124`: cooperative deadline with terminal `stream_abort`;
- `128+signal`: interruption with terminal `stream_abort`.

Process errors are protocol-v3 stderr JSONL with stable `code`, `operation`,
`retryable`, `message`, and structured `details`. Branch on fields such as
`details.kind`, never human message text.

`validate` and `summarize` return `0` for any valid completed stream, including
one whose collection exited `3`; inspect the summary's `stream.outcome` and
capability coverage rather than treating validation success as complete scope.

## Stream state machine

A successful invocation emits exactly:

```text
stream_start
  capability_start
    data | diagnostic ...
  capability_end
  ...
stream_end
EOF
```

`schema_version` is `3` on every record. `seq` starts at zero and increases by
one for every record. A targeted command has one capability section. A profile
has its fixed collector-defined sections in order. Every record is one JSON
object followed by LF.

Important fields:

- `stream_start`: collector identity, invocation kind, command ID, effective
  global limits, and the complete unique ordered `planned_capabilities`;
- `capability_start`: capability ID and normalized request;
- `data`: capability ID, data kind, provider, and typed `data` object;
- `diagnostic`: capability ID and stable diagnostic object;
- `capability_end`: capability outcome, pre-terminal counters, coverage,
  truncation, and limits hit;
- `stream_end`: stream outcome, aggregate coverage, pre-terminal counters, and
  ordered `not_started_capabilities`.

The counters in a terminal record describe records and JSONL bytes before that
terminal record. Do not compare them to the final file size as if they included
the terminal line itself.

`validate` additionally verifies that started capability IDs followed by
`not_started_capabilities` exactly equal the declared plan, without duplicates.
`summarize` returns `capabilities.planned`, `capabilities.started`,
`capabilities.not_started`, symmetric counts, and IDs grouped by capability
outcome.

## Outcomes and coverage

Capability outcomes:

- `complete`: declared scope was observed without known coverage loss;
- `partial`: usable facts exist, but a limit, race, permission failure, malformed
  source, or I/O failure left unknown coverage;
- `unavailable`: no usable data was obtained from the source;
- `unsupported`: the platform or source semantics are not implemented.

Stream outcomes:

- `complete`: every planned capability started and ended `complete`;
- `degraded`: at least one capability ended `partial`, `unavailable`, or
  `unsupported`, or a planned capability was not started.

`degraded` is a completed, structurally valid stream, not an aborted prefix.
Deadline and signal interruption use `stream_abort`, never `stream_end`.

Coverage fields:

- `observed`: emitted entities or chunks;
- `scanned`: source-dependent work count, often bytes or directory entries;
- `skipped`, `denied`, `vanished`: explicit coverage loss;
- `truncated`: acquisition stopped before declared scope was exhausted.

`scanned` units are capability-specific. Use the hard-limit names shown by the
selected command's help and the terminal `limits_hit`; do not compare it across
unrelated capabilities.

`complete` is relative to the capability's declared providers, sources, and
current namespaces. It is not a claim about host state hidden from the process.

Stable general diagnostic codes are `source_not_found`,
`source_permission_denied`, `source_malformed`, `source_io_error`,
`source_raced`, `coverage_incomplete`, and `limit_reached`.

## Baseline profile scope

`collect` identifies its stream as `baseline-v3`. Its normalized
`network.sockets` request declares
`socket_selection: inet_all_unix_listeners`: all TCP/UDP sockets and only
listening Unix sockets. Its `systemctl.list` request declares
`unit_selection: security_baseline` and `excluded_unit_types: ["device"]`.
Targeted `network sockets`, `network listeners`, and `systemctl list`
collections retain their complete command-specific scope.

## Data records

Current data kinds and key fields:

| Capability | Data kind | Key fields |
|---|---|---|
| `host.info` | `host` | OS, architecture, hostname, distribution, uptime |
| `kernel.info` | `kernel` | name, release, version, command line |
| `kernel.modules` | `kernel_module` | name, size, use count, dependencies, state |
| `kernel.sysctls` | `kernel_sysctl` | key, value, source |
| `security.posture` | `security_control` | category, control name, runtime value, source |
| `process.list` | `process` | PID/PPID, credentials, capabilities, seccomp, namespaces, executable and command line |
| `network.interfaces` | `network_interface` | name, index/link metadata or traffic counters |
| `network.addresses` | `network_address` | interface, family, address/prefix, broadcast or destination |
| `network.resolvers` | `resolver_directive` | resolver, hosts, NSS, and systemd-resolved directives with source and line |
| `network.routes` | `network_route` | family, interface, destination, gateway, metric |
| `network.neighbors` | `network_neighbor` | family, address, link-layer address, state flags, interface |
| `network.sockets` | `network_socket` | protocol/family, endpoints, state, UID/inode, owners |
| `network.listeners` | `network_socket` | listening socket fields and bounded owners |
| `network.firewall` | `firewall_evidence` | runtime legacy table names or static nftables/iptables/UFW rules with source |
| `mount.list` | `mount` | IDs, device, root, target, filesystem, source, options, namespace |
| `cgroup.inspect` | `cgroup_membership` | hierarchy, controllers, membership path, self-scope v2 properties |
| `user.list` | `user` | name, UID/GID, home, shell, GECOS |
| `group.list` | `group` | name, GID, members |
| `auth.posture` | `auth_control` | shadow account state/algorithm or PAM/login/polkit/doas directive |
| `sudo.rules` | `sudo_directive` | source, line, static directive including followed include files |
| `service.list` | `service` | system/global-user/per-user manager scope, all systemd unit types or SysV name, static enablement/drop-ins/security directives |
| `schedule.list` | `schedule` | cron/anacron/periodic/at/timer source, entry or timer directives |
| `systemctl.list` | `systemd_unit_runtime` | manager scope/user/socket, unit type, load/active/sub state, object path, jobs, unit-file state, timestamps and type-specific runtime fields |
| `systemctl.inspect` | `systemd_unit_runtime` | exact unit runtime record from one selected system or user manager |
| `dbus.list` | `dbus_record` | bus ID/features/interfaces and owned/activatable names with unique owners and available PID/UID/GIDs/security label |
| `dbus.inspect` | `dbus_record` | one bus name's ownership, activation availability, unique owner and available credentials |
| `container.list` | `container` | runtime-agnostic running container ID, runtime, representative PID, cgroup and procfs source |
| `container.inspect` | `container_context` | matching native runtime ID or self cgroup, markers and namespaces |
| `porto.list` | `porto_container`, `porto_property_catalog`, `porto_property` | Porto property catalog, normalized containers and chunked property values |
| `porto.inspect` | `porto_container_context`, `porto_property_catalog`, `porto_property` | named Porto snapshot and property values |
| `ssh.server_config` | `ssh_server_directive` | source, line, key, value, static `Match` context; include files followed |
| `ssh.authorized_keys` | `ssh_authorized_key` | user, resolved configured source, line, key type, SHA-256 fingerprint, options |
| `filesystem.privilege_surfaces` | `filesystem_privilege_surface` | explicit-root path, type, mode, owner, set-ID/writability/file-capability reasons |
| `filesystem.unix_sockets` | `filesystem_unix_socket` | explicit-root socket path, ownership, mode, device/inode, mount metadata |
| `file.stat` | `file_metadata` | path, type, size, mode, UID/GID, device/inode, timestamps, link target |
| `file.read` | `file_chunk` | path, offset, byte count, encoding, content, EOF |

`file.stat` reports final symlinks as symlinks without following them;
`file.read` rejects a final symlink or non-regular target. Parent path
components can still resolve through symlinks. Read only authorized paths,
and remember that stat and read can race. `auth.posture` emits shadow password
state/algorithm, not hashes, but other raw directives, command lines, Porto
properties, and explicitly read file content can carry secrets.

Runtime systemd capabilities use `vzik systemctl` and direct read-only D-Bus
calls. Generic D-Bus capabilities use `vzik dbus` and never activate services.
Both default to `/run/dbus/system_bus_socket`; `--user USER|UID` selects
`/run/user/UID/bus`, `--socket PATH` selects an exact Unix socket, and list-only
`--all-users` adds discoverable user buses. An unavailable bus has an explicit
capability outcome and does not fail the rest of a baseline collection.

Runtime-agnostic container capabilities read procfs and cgroups only. Porto
capabilities use `vzik portoctl` and the direct read-only Porto RPC Unix socket;
the socket defaults to `/run/portod.socket` and can be selected with `--socket`.
Environment properties are redacted by default; stdout and stderr require
explicit bounded stream collection. Property-level Porto failures remain
attached to their property records.

Use the data-kind summary and representative bounded output when generating
filters or validating types.

## Capture receipt

`vzik capture` creates new mode-0600 capture and stderr files, validates the
complete stream, then creates and verifies a protocol-v3 receipt. Its stdout
JSON has `artifact_status: "sealed"`, `outcome`, three artifact paths, and a
`coverage_summary` whose planned, started, and not-started capability sets
and counts match the receipt. A sealed degraded capture returns status `3`
and remains usable. If stdout summary writing fails after sealing, status is
`5`; inspect the receipt and validate the capture rather than assuming the
files were rolled back.

The three files are not a filesystem transaction. Interruption or write
failure can leave capture or stderr without a receipt; existing paths cause
an error and are not overwritten. Receipt presence means the capture passed
validation and hash sealing at that time, not that later modification is
impossible or that prior writes were atomic. Protect all artifacts, including
stderr, as sensitive evidence.

## Linux byte strings

A Linux byte-originated value has this form:

```json
{"display":"readable or escaped text","raw_base64":"only for invalid UTF-8"}
```

If `raw_base64` is absent, `display` is exact valid UTF-8. If it is present,
`display` may contain `\\xNN` escapes and is presentation-only. Keep the base64
field in machine evidence. Decode to a byte-safe consumer, never through a shell
variable that cannot preserve NUL.

For `file_chunk`, `encoding: "utf-8"` means `content` is text for that chunk;
`encoding: "base64"` means `content` encodes its raw bytes. Encoding can differ
between chunks. `bytes` always counts raw bytes, not JSON or base64 length.

## Useful streaming queries

List facts without materializing the whole capture:

```sh
jq -c 'select(.type == "data") |
       [.capability, .data_kind, .data]' capture.jsonl
```

Find only the current process:

```sh
jq -c 'select(.type == "data" and
              .capability == "process.list" and
              .data.is_self == true) | .data' processes.jsonl
```

Select listening-related mount or resolver evidence by semantic fields rather
than raw line text:

```sh
jq -c 'select(.type == "data" and .capability == "mount.list") |
       select(.data.read_only == false) |
       {target: .data.target, filesystem: .data.filesystem, options: .data.options}' mounts.jsonl

jq -c 'select(.type == "data" and
              .capability == "network.resolvers" and
              .data.name.display == "nameserver") | .data' resolvers.jsonl
```

Summarize all known coverage loss:

```sh
jq -c 'select(
         (.type == "capability_end" and .outcome != "complete") or
         .type == "diagnostic"
       )' capture.jsonl
```

Inspect file chunks without corrupting binary content:

```sh
jq -c 'select(.type == "data" and .capability == "file.read") |
       {offset: .data.offset, bytes: .data.bytes,
        encoding: .data.encoding, eof: .data.eof,
        content: .data.content}' file.jsonl
```

Before claiming a complete file read, require a `complete` capability outcome,
no truncation or limits hit, contiguous offsets, and an EOF marker. The zero-byte
EOF record is a terminator, not additional file content.
