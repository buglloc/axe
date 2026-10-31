# Vzik monitor schema v1

`vzik monitor` writes JSONL with `schema: "raskop/vzik-monitor"` and
`schema_version: 1`. This stream is separate from protocol-v4 collection and
must not be passed to `vzik validate` or `vzik summarize`.

## Records

- `stream_start` declares `event_classes`, filters, bounds, and whether
  `--full` makes output sensitive.
- `snapshot_start`, `snapshot_state`, and `snapshot_end` delimit a bounded
  state checkpoint. `snapshot_state.class` is `process`, `files`, or `sockets`.
- `event` contains `class`, `event`, `source`, `detection`, and `data`.
  `detection: "exact"` means a kernel event; `detection: "sampled"` means a
  scan or diff observation.
- `gap` contains `class`, `source`, `reason`, and `dropped`. It is explicit
  loss, not an empty observation.
- `diagnostic` reports source or resource conditions.
- `stream_end` contains `stop_reason`, `collection_outcome`, `classes`, and
  `completion`.

Sequenced records contain monotonic `sequence`. A snapshot restores state but
cannot reconstruct events lost before a `gap`.

## Sources

Process events use proc connector when available and otherwise a bounded `/proc`
scan. File events use fanotify filesystem marks with process attribution when
available, and recursive inotify watches without attribution as fallback. Both
backends never follow symlinks. Socket events are sampled
by diffing `/proc/net/{tcp,tcp6,udp,udp6,unix}` and do not identify an actor
process.

The monitor does not change audit policy, sysctls, tracefs/bpffs mounts,
firewall state, or filesystem state. Process records always include bounded
`executable` and `argv`; `--full` adds bounded environment values. Treat
stdout as sensitive for process-selected runs and whenever `--full` is used.

## Class selection

Use one explicit monitor subcommand. `process` selects process events;
`files PATH...` selects recursive file events for one or more roots;
`sockets` selects socket events; and `all --files PATH...` selects all three
classes with one or more repeatable file roots. `--glob PATTERN` is valid only
for `files` and `all`. File paths are recursive and symlinks are not followed.
Selected classes are the only entries in `stream_start.event_classes`, snapshot
states, and `stream_end.classes`. Completion status considers only selected
classes.

## Completion

Exit status `0` means all selected classes were exact and no gaps or evictions
occurred. Exit status `3` means a selected class was sampled or unavailable,
or the stream recorded a gap or eviction. A closed stdout returns `141` without
a `stream_end`.
