# Vzik monitor schema v2

`vzik monitor` writes JSONL with `schema: "raskop/vzik-monitor"` and
`schema_version: 2`. This stream is separate from collection and must not be
passed to `vzik validate` or `vzik summarize`.

## Records

- Every record carries `seq`, starting at zero and increasing by one, including
  `stream_start`, `diagnostic`, and `stream_end`.
- `stream_start` declares `classes`, filters, bounds, and whether `--full`
  makes output sensitive. Each selected class has `{provider, detection}`.
- `snapshot_start` and `snapshot_end` delimit a bounded state checkpoint and
  carry `snapshot_id`; `snapshot_state.class` is `process`, `files`, or
  `sockets`.
- `event` is `{type: "event", class, event, provider, detection, data}`.
  `detection: "exact"` means a kernel event; `detection: "sampled"` means a
  scan or diff observation.
- `gap` is `{type: "gap", class, provider, reason, dropped, source?}`.
  `source` is the byte-string directory path whose watch setup failed. Reasons
  are `output_queue`, `queue_overflow`, `provider_stopped`, `watch_limit`,
  `watch_unavailable`, and `directory_unreadable`.
- `diagnostic` is `{type: "diagnostic", class, diagnostic: {provider, code,
  message, source?}}`. Codes are `provider_unavailable`,
  `provider_stopped`, and, with `--verbose`, `watch_unavailable` or
  `directory_unreadable`.
- `stream_end` is `{type: "stream_end", stop_reason, collection_outcome,
  classes}`. `stop_reason` is `signal`, `duration`, `max_events`, or
  `provider_stopped`; terminal status is represented by `collection_outcome`.

A snapshot restores state but cannot reconstruct events lost before a `gap`.

## Sources

Process events use `proc_connector` when available and otherwise
`procfs_scan`. File events use `fanotify` with process attribution when
available, and `inotify` without attribution as fallback. Socket events use
`procfs` and are sampled by diffing
`/proc/net/{tcp,tcp6,udp,udp6,unix}`. File providers never follow symlinks.

The monitor does not change audit policy, sysctls, tracefs/bpffs mounts,
firewall state, or filesystem state. Process records always include bounded
`executable` and `argv`; `--full` adds bounded environment values. Treat
stdout as sensitive for process-selected runs and whenever `--full` is used.

`source` paths use the Linux byte-string form: a valid UTF-8 JSON string, or
`{display, base64}` for invalid UTF-8.

Provider values are `proc_connector` or `procfs_scan` for processes,
`fanotify`, `inotify`, or `unavailable` for files, and `procfs` for sockets.

On non-Linux, the command emits one `stream_end` record with `seq: 0`,
`collection_outcome: "unsupported"`, and `stop_reason: "unsupported"`, then
exits with status `4`.

## Class selection

Use one explicit monitor subcommand. `process` selects process events;
`files PATH...` selects recursive file events for one or more roots;
`sockets` selects socket events; and `all --files PATH...` selects all three
classes with one or more repeatable file roots. `--glob PATTERN` is valid only
for `files` and `all`. File paths are recursive and symlinks are not followed.
Selected classes are the only entries in `stream_start.classes`, snapshot
states, and `stream_end.classes`. Terminal status considers only selected
classes.

## Outcomes

Exit status `0` means all selected classes were exact and no gaps or evictions
occurred. Exit status `3` means a selected class was sampled or unavailable,
or the stream recorded a gap or eviction. A closed stdout returns `141` without
a `stream_end`.
