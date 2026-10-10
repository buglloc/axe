# Vzik protocol v4 reference

Use this reference for discovery, stream validation, capture receipts, and JSON
queries. Use hierarchical `vzik ... --help` output for current commands,
arguments, defaults, and bounds.

## Contents

- [Discovery and process status](#discovery-and-process-status)
- [Stream state machine](#stream-state-machine)
- [Outcomes and coverage](#outcomes-and-coverage)
- [Baseline profile scope](#baseline-profile-scope)
- [Data records](#data-records)
- [Host overview contract](#host-overview-contract)
- [Process inspection contract](#process-inspection-contract)
- [Capture receipt](#capture-receipt)
- [Linux byte strings](#linux-byte-strings)
- [Useful streaming queries](#useful-streaming-queries)

## Discovery and process status

`vzik capabilities` returns a compact index with protocol semantics, exit
statuses, global limits, and stable capability IDs. `vzik capabilities
CAPABILITY_ID` returns request schema, data kinds, access class, baseline
position, and possible outcomes for exactly one capability.

Collection process status and terminal state are one contract:

- `0`: `stream_end.collection_outcome` is `complete`;
- `3`: `stream_end.collection_outcome` is `degraded`, but the stream remains valid and its
  findings remain usable within reported coverage;
- `2`: invalid CLI request or unreadable validation input;
- `4`: internal invariant failure, invalid stream, or capture artifact I/O
  failure (including an existing output path);
- `5`: stdout write or flush failure;
- `124`: cooperative deadline with terminal `stream_abort`;
- `128+signal`: interruption with terminal `stream_abort`.

Process errors are protocol-v4 stderr JSONL with stable `code`, `operation`,
`retryable`, `message`, and structured `details`. Branch on fields such as
`details.kind`, never human message text.

`validate` and `summarize` return `0` for any valid completed stream, including
one whose collection exited `3`; inspect the summary's `stream.collection_outcome` and
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

`schema_version` is `4` on every record. `seq` starts at zero and increases by
one for every record. A targeted command has one capability section. A profile
has its fixed collector-defined sections in order. Every record is one JSON
object followed by LF.

Important fields:

- `stream_start`: collector identity, invocation kind, command ID, effective
  global limits, and the complete unique ordered `planned_capabilities`;
- `capability_start`: capability ID and normalized request;
- `data`: capability ID, data kind, provider, and typed `data` object;
- `diagnostic`: capability ID and stable diagnostic object;
- `capability_end`: capability outcome, pre-terminal byte counter, coverage
  (including `coverage.truncated`), and limits hit;
- `stream_end`: `collection_outcome`, `truncated`, pre-terminal byte counter, and
  ordered `not_started_capabilities`.

`counters.jsonl_bytes` counts JSONL bytes before the terminal record; it does
not include that terminal line. `seq` is its zero-based record position.
Stream truncation is true if any capability was truncated or any planned
capability was not started. A source failure can degrade a stream without
truncating it.

`validate` additionally verifies that started capability IDs followed by
`not_started_capabilities` exactly equal the declared plan, without duplicates.
`summarize` returns `capabilities.planned`, `capabilities.started`,
`capabilities.not_started`, symmetric counts, and IDs grouped by capability
outcome.

`validate` accepts schema version 4 and verifies that terminal truncation agrees
with capability truncation and the unstarted suffix of the plan.

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

- `observed`: acquired observations, entities, or chunks according to capability;
- `skipped`: omitted, unsupported, or inconclusive observations/candidates;
- `denied`, `vanished`: permission failures and missing sources/candidates;
- `truncated`: acquisition stopped before declared scope was exhausted.

Counts are capability-specific, not a percentage or a count of all host objects.
Use source availability, diagnostics, and `limits_hit` to explain gaps.

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

`host.overview` and `process.inspect` are targeted-only; neither changes the
contents or order of `baseline-v3`.

## Data records

Current data kinds and key fields:

| Capability | Data kind | Key fields |
|---|---|---|
| `host.overview` | `host_overview` | collector-visible environment, observation timestamp and elapsed time |
| `host.info` | `host` | OS, architecture, hostname, distribution, uptime |
| `kernel.info` | `kernel` | name, release, version, command line |
| `kernel.modules` | `kernel_module` | name, size, use count, dependencies, state |
| `kernel.sysctls` | `kernel_sysctl` | key, value, source |
| `security.posture` | `security_control` | category, control name, runtime value, source |
| `process.list` | `process` | PID/PPID, credentials, capabilities, seccomp, namespaces, executable and command line |
| `process.inspect` | `process_detail` | one PID's security context, resource counters, rlimits, I/O and source availability |
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

`network.interfaces` uses sysfs link metadata. If `/sys/class/net` cannot be
enumerated, it emits a source diagnostic and falls back to `/proc/net/dev`
traffic counters. A successful fallback is still `partial`: index, MTU,
link type, hardware address, flags, and operstate remain unknown.
Coverage retains the sysfs failure as denied/vanished/skipped according to
the error; source failure alone does not set `truncated` or imply a limit hit.
If procfs also fails, the capability is `unavailable` and both source failures
remain visible. Branch on the diagnostic code and terminal coverage.

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
Records expose `selection_scope`: `system` for the default system endpoint,
`user` for a selected or discovered user endpoint, and `explicit_socket`
whenever `--socket` is supplied, including together with `--user`. This field
describes endpoint selection, not a discovered bus type. `socket` preserves
the exact endpoint; `manager_user` and `manager_uid` retain user selection
metadata when applicable.

Runtime-agnostic container capabilities read procfs and cgroups only. Porto
capabilities use `vzik portoctl` and the direct read-only Porto RPC Unix socket;
the socket defaults to `/run/portod.socket` and can be selected with `--socket`.
Environment properties are redacted by default; stdout and stderr require
explicit bounded stream collection. Property-level Porto failures remain
attached to their property records.

Use the data-kind summary and representative bounded output when generating
filters or validating types.

## Host overview contract

`host_overview` has `detail: "summary" | "full"`,
`scope: "collector_visible"`, `observed_at_unix_ms: u64` (Unix milliseconds at
collection start), `elapsed_ms: u64`, and `environment`. `vzik overview`
selects summary; `vzik overview --details` selects full. The reads are
sequential, not an atomic snapshot. There is no persistent entity key; the
record describes this invocation's observer and visible system.

Environment leaves use `Observation<T>` with mandatory `status`. Both modes
omit missing `value`, `reason`, and `error`, and empty `evidence`. Evidence
contains source/detail pairs. Only `available` supplies a value;
`available(false)`, numeric zero, and an available empty value are
actual observations. `absent` means the selected optional source or marker
was not present; `unknown` means evidence does not establish a value;
`unavailable` carries acquisition failure; `unsupported` means unimplemented
platform semantics; `not_applicable` and `redacted` are explicit exclusions.
Never substitute zero, false, or an empty collection for an unknown value.
Errors expose operation/class/code/errno/message; branch on status and code.

Coverage counts the selected mode's observations. Fields excluded from summary
by design do not imply acquisition loss or truncation.

| Environment path | Value type and interpretation | In summary |
|---|---|---|
| `host.os`, `.architecture`, `.hostname` | string | yes |
| `host.distro` | object with optional string `id`, `name`, `version_id`, `pretty_name` | yes |
| `kernel.name`, `.release`, `.version`, `.command_line` | string; command line is the visible kernel source | release only |
| `memory.physical_bytes`, `.available_bytes`, `.swap_total_bytes`, `.swap_free_bytes` | u64 bytes; visible memory figures, not an effective container allocation | physical and available only |
| `resources.cpu_online` | u32 count from the visible online CPU list | yes |
| `resources.cpu_allowed_list` | string Linux CPU-list syntax for the collector's allowed CPUs | yes |
| `resources.uptime_seconds` | finite nonnegative f64 seconds | yes |
| `resources.load_average` | three finite nonnegative f64 values, 1/5/15-minute load averages | yes |
| `resources.pressure.{cpu,memory,io}` | PSI object with `some` and optional `full` | yes |
| `resources.pressure.scope` | `"visible_system"` for PSI from the visible `/proc/pressure` sources | yes |
| `process.{pid,ppid,uid,effective_uid,gid,effective_gid}` | u32 observer IDs in the visible namespaces | PID and effective IDs only |
| `restrictions.capability_sets.{inheritable,permitted,effective,bounding,ambient}` | array of Linux capability names | no |
| `restrictions.no_new_privileges` | bool | yes |
| `restrictions.seccomp_mode`, `.seccomp_filter_count` | u32 raw kernel values | mode only |
| `restrictions.lsm_profile` | string | no |
| `restrictions.{uid_map,gid_map}` | array of `{namespace_start, parent_namespace_start, length}` with u32 ranges in the collector and its parent namespace | yes |
| `restrictions.namespaces.{cgroup,ipc,mnt,net,pid,time,user,uts}` | string namespace link target | no |
| `restrictions.cgroups.membership` | array of `{hierarchy_kind: "unified" \| "legacy", namespace_relative_path: byte string}`; legacy entries also carry `hierarchy_id: u32` and `controllers: string[]` | yes |
| `restrictions.cgroups.limits` | observations of raw string `cpu.max`, `memory.current`, `memory.max`, `pids.current`, `pids.max` | yes |
| `restrictions.cgroups.accounting` | `cpu_stat`, `memory_events`, `pids_events`: maps of string keys to u64 counters | no |
| `restrictions.cgroups.pressure.{cpu,memory,io}` | current visible cgroup PSI | no |
| `restrictions.cgroups.pressure.scope` | `"visible_current_cgroup"` for safely resolved current cgroup PSI | no |
| `subsystems.{procfs,sysfs,cgroup_v2,systemd,dbus,porto}` | bool for the selected marker's expected file type | yes |
| `isolation.interpretation` | `"heuristic_indicator"` for the three isolation layers | yes |
| `isolation.{virtual_machine,container,sandbox}` | one observation per layer; value is `{verdict: "indicator_present", provider: string, confidence: "low" \| "medium" \| "high"}` with evidence on the observation | yes |

PSI `some`/`full` contain `avg10`, `avg60`, `avg300` (f64 percentages)
and `total_us` (u64 accumulated stall microseconds). Missing `full` is not zero.
Accounting counter units follow the kernel key: CPU `*_usec` are microseconds,
`nr_*` are counts; memory/pids events are counts. Cgroup memory limits are
bytes, pids limits counts, and `cpu.max` is quota/period microseconds or `max`.
These are local values, not minima across ancestor cgroups.

Self UID/GID maps describe the immediate parent namespace, not necessarily the
initial host namespace. An empty available map is an observed unmapped namespace.
Cgroup membership paths are relative to the collector's cgroup namespace;
`/` does not establish host-root membership or absence of resource budgets.
Unified membership has no legacy hierarchy ID or controller list; it does not
report the active or delegated v2 controllers.
Pressure scope identifies the observed source domain, not a physical-host
identity or a verdict about collection quality.

Subsystem sources are `/proc`, `/sys`, `/sys/fs/cgroup/cgroup.controllers`,
`/run/systemd/system`, `/run/dbus/system_bus_socket`, and `/run/portod.socket`.
The last two require a Unix socket file type. No socket is connected;
absence does not exclude another location or a hidden host subsystem.
Linux-only observations are `unsupported` on other platforms; portable
host/process facts remain available where implemented.

Overview coverage counts selected observations, with one observation per isolation layer.
Available, normal absent, and not-applicable fields count as observed;
unsupported/redacted and inconclusive isolation heuristics count as skipped
without acquisition degradation. Failed or malformed factual measurements
make the capability partial. New summary source reads are bounded to 16 KiB;
counter maps allow at most 64 keys of at most 64 bytes each.

## Process inspection contract

`process_detail` has `scope: "collector_visible"` and one `pid: u32`.
PID/PPID refer to the collector's visible PID namespace and are not persistent
object identity. Required summary fields are `ppid: u32`, `name` and `state`
as Linux byte strings, `is_self: bool`, `namespaces: object`,
`security_context_complete: bool`, `resources: object`, and `sources: object`.

When acquired, security fields match `process.list`:
`uids`/`gids` objects with u32 real/effective/saved/filesystem IDs, `groups: u32[]`,
byte-string hexadecimal `cap_inheritable`, `cap_permitted`, `cap_effective`,
`cap_bounding`, `cap_ambient`, `no_new_privs: bool`, `seccomp: u32`,
`seccomp_filters: u32`, and byte-string octal `umask`.
`cmdline` is an array of byte strings; a successfully read empty array is valid.
`exe`, `cwd`, and `root` are optional byte strings. Optional
`cgroup_membership` uses the same structured membership entries as overview.
Namespace keys are cgroup/ipc/mnt/net/pid/user/uts with available link targets
as byte strings. Missing optional fields mean unavailable evidence, not zero.

| Resource field | Type and unit |
|---|---|
| `rss_bytes`, `virtual_size_bytes` | u64 bytes; RSS uses checked pages × page size |
| `threads` | u64 thread count |
| `cpu_user_ticks`, `cpu_system_ticks` | u64 accumulated CPU ticks, not utilization |
| `clock_ticks_per_second` | u64 ticks per second used to interpret CPU counters |

Optional `rlimits` is an array of `{name, soft, hard, units?}`.
`name`/`units` are byte strings; soft/hard are u64 or the string `"unlimited"`.
Zero is a real limit; unitless priorities omit units. Optional `io` has u64
`read_interface_bytes`, `write_interface_bytes` (kernel rchar/wchar accounting
through read/write interfaces, including cached I/O), `read_calls`, `write_calls`
(syscr/syscw), and `accounted_storage_read_bytes`, `accounted_storage_write_bytes`,
`accounted_cancelled_storage_write_bytes` (kernel storage-layer accounting).
Zero storage accounting does not establish that all reads were served from cache.

`sources` maps stat/status/cmdline/cgroup/exe/cwd/root/limits/io and
`ns.{cgroup,ipc,mnt,net,pid,user,uts}` to `{source, availability}`.
`source` is a byte-string `/proc/PID/...` path. Availability is `available`,
`truncated`, `not_found`, `permission_denied`, `malformed`, or `io_error`.
Reads are bounded to 64 KiB per file. Variable output fields have a 4 KiB
serialized budget; shortening is explicit, with `limit_reached`, partial
coverage, and `max_source_bytes` or `max_line_bytes` in `limits_hit`.
Source-read truncation omits fields; output shortening can retain prefixes.

Stat is mandatory and re-read before emission. If unreadable or malformed,
the capability is unavailable and emits no detail. Detected PID reuse emits
`source_raced` and discards the snapshot; the private race guard is not
serialized identity. Optional source failures retain usable facts with
partial coverage. Complete means all declared sources were available; it
does not imply an atomic or permanently valid process snapshot.
No environ, smaps, memory-map, or FD inventory is acquired.

## Capture receipt

`vzik capture COMMAND --output-dir DIR` requires an existing parent and creates
one new mode-0700 directory with `capture.jsonl`, `receipt.json`, and
`stderr.jsonl`. Any existing destination, including a symlink, is an error.
Alternatively, specify `--output FILE --receipt FILE [--stderr FILE]`;
the default stderr path appends `.stderr` to the output path. These file options
cannot be combined with `--output-dir`.

`vzik capture` creates new mode-0600 capture and stderr files, validates the
complete stream, then creates and verifies a protocol-v4 receipt. Its stdout
JSON has `artifact_status: "sealed"`, `collection_outcome`, three artifact paths, and a
`coverage_summary` whose planned, started, and not-started capability sets
and counts match the receipt. Its `stream_truncated` flag also appears under
the receipt's `capture`; per-capability coverage retains `truncated`.
A sealed degraded capture returns status `3`
and remains usable. If stdout summary writing fails after sealing, status is
`5`; inspect the receipt and validate the capture rather than assuming the
files were rolled back.

The stdout summary and receipt are pretty JSON documents; collection and
stderr are compact JSONL. Artifact paths in these metadata documents are
absolute paths with canonical parents: valid UTF-8 is a JSON string; otherwise
the path uses the [Linux byte-string object](#linux-byte-strings) with
`display` and `raw_base64`. Preserve the latter for byte-safe reopening.

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
