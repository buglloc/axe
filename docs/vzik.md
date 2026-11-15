# Vzik

`vzik` collects bounded host and container evidence. It runs as a standalone binary or as an AXE applet. Start with passive host context, then pick a targeted command:

```bash
vzik --version
vzik overview
vzik monitor process --duration 30s
vzik monitor files /var/log --duration 30s
vzik monitor sockets --duration 30s
vzik monitor all --files /var/log --duration 30s
vzik collect
vzik network listeners --max-items 4096
vzik security posture
vzik capabilities
vzik capabilities porto.list
vzik capture collect --output-dir baseline-evidence
vzik validate baseline-evidence/capture.jsonl
vzik summarize baseline-evidence/capture.jsonl
```

Field types, units, missing values, and encodings are specified in the [collection protocol reference](../skills/vzik/references/protocol-v5.md) and the [monitor stream reference](../skills/vzik/references/monitor-v2.md).

## Discovery and output

`vzik capabilities` lists probes and global limits. Pass a probe ID, as in `vzik capabilities porto.list`, to see its request schema and access requirements. A probe that cannot reach a host service reports itself as unavailable; unsupported probes are reported, not silently omitted.

`vzik --version` (or `-V`) prints the version without collecting evidence. Discovery, validation, summaries, capture stdout, and receipts are pretty-printed JSON. Collection and process diagnostics are compact JSONL, one complete record per line.

## Host overview and process inspection

`overview` emits one compact `host_overview` record: system identity, collector credentials and restrictions, visible memory, CPU, load and pressure, current cgroup limits, isolation evidence, and subsystem markers. `overview --details` adds namespaces, capability sets, swap, the kernel command line, and cgroup counters and pressure. Neither mode runs programs or connects to services. Values describe what the collector can see, which is not necessarily the physical host, and a subsystem marker does not prove that the service is reachable. `axe doctor` includes the detailed overview and adds AXE runtime, execution, and storage checks.

`process inspect PID` reads one visible Linux process: memory, CPU time, threads, resource limits, I/O counters, open descriptors, executable metadata, LSM label, ancestry, session, and TTY, alongside its security context. If optional sources are missing or denied, the result is partial. If the process stat file is unreadable or the PID has been reused, there is no result. It does not read environment variables, process memory, or memory maps, but command lines, executable paths, and descriptor targets can still contain secrets.

Neither command is part of the `collect` profile.

## Monitoring

`monitor` is Linux-only and runs until it is stopped or reaches a limit such as `--duration`. Bare `vzik monitor` prints help. Choose one subcommand:

```text
vzik monitor process [COMMON_OPTIONS]
vzik monitor files PATH... [--glob PATTERN] [COMMON_OPTIONS]
vzik monitor sockets [COMMON_OPTIONS]
vzik monitor all --files PATH... [--glob PATTERN] [COMMON_OPTIONS]
```

File paths are watched recursively and symlinks are not followed. Process and socket monitoring covers everything visible in the collector's namespaces.

Process events are exact when the kernel process connector is available and sampled from `/proc` otherwise. File events name the responsible process only when vzik has the privileges for fanotify; without them, inotify reports changes without attribution. Socket changes are always sampled.

Process records include the executable and arguments, so treat the output as sensitive; `--full` also adds environment data. Monitoring exits with status `3` when a selected class is only sampled, a provider is unavailable, events were dropped, or tracked state was evicted, and with `141` when stdout closes.

## Collection and capture

`collect` and other collection commands write protocol-v5 JSONL. A final `stream_end` record closes the stream but does not guarantee full coverage: check its `collection_outcome`, `truncated`, and per-capability coverage. A complete collection exits with status `0`; a degraded one exits with `3`. A stream without `stream_end` is incomplete. Use `validate` to check a saved stream and `summarize` to see its coverage.

`vzik capture COMMAND --output-dir DIR` creates a new private directory with `capture.jsonl`, `receipt.json`, and `stderr.jsonl`. The parent must exist and `DIR` must not. For custom file names, use `--output FILE --receipt FILE [--stderr FILE]` instead; stderr then defaults to the output path plus `.stderr`. Capture never overwrites existing files. It writes the receipt only after validating the stream, so an interrupted capture leaves files without a receipt. A receipt can describe degraded evidence; it does not mean every probe succeeded.

## Reading saved evidence

Check the summary first, then select records with `jq`:

```bash
vzik capture overview --output-dir overview-evidence
vzik summarize overview-evidence/capture.jsonl
jq 'select(.type == "data" and .data_kind == "host_overview") | .data | {scope, detail, host: .environment.host, uid_map: .environment.restrictions.uid_map}' overview-evidence/capture.jsonl
```

When projecting process records, keep `scope` and `pid` alongside the selected fields.

Evidence can contain sensitive host data. Keep the capture, receipt, and stderr files together when sharing or archiving a collection.
