# Vzik

`vzik` collects bounded host and container evidence as a standalone binary or an AXE applet. Start with passive host context, then select a targeted command:

```bash
vzik --version
vzik overview
vzik process inspect 1234
vzik collect
vzik network listeners --max-items 4096
vzik security posture
vzik capabilities
vzik capabilities porto.list
vzik capture collect --output-dir baseline-evidence
vzik validate baseline-evidence/capture.jsonl
vzik summarize baseline-evidence/capture.jsonl
```

`vzik capabilities` lists probes and global limits. Pass a probe ID, as in `vzik capabilities porto.list`, for its request schema and access requirements. A probe reports unavailable when it cannot access a host service; unsupported probes are reported rather than silently omitted.

`vzik --version` (or `-V`) prints the binary version without collecting evidence. Discovery, validation, summaries, capture stdout, and receipts are pretty-printed JSON documents. Collection and process diagnostics remain compact JSONL: one complete record per line.

`overview` emits one compact `host_overview` record: system identity, collector credentials and restrictions, visible memory/CPU/load/pressure, current cgroup limits, isolation evidence, and fixed subsystem markers. `overview --details` adds the full passive observations, including namespaces, capability sets, swap, kernel command line, and cgroup counters/pressure. The record's `detail` distinguishes `summary` from `full`; missing values remain explicit observation statuses. Neither mode executes programs or connects to services. Values describe the collector's view, not necessarily the physical host; a marker does not prove service reachability. `axe doctor` reuses the full passive observations and adds AXE-specific runtime and active execution/storage checks.

`process inspect PID` reads one visible Linux process without enumerating processes. It adds memory, cumulative CPU ticks, threads, resource limits, I/O counters, and per-source availability to the existing security context. Missing or denied optional sources produce partial evidence; an unreadable mandatory stat or detected PID reuse produces no process detail. It does not read environment variables, memory maps, smaps, or file-descriptor inventories. Command lines can still contain secrets.

Both commands are targeted capabilities, not additions to the security `collect` profile. Detailed field types, units, missing-value semantics, and scope are maintained in the [Vzik skill reference](../skills/vzik/references/protocol-v4.md); discovery remains compact and does not embed full result schemas.

Collection writes protocol-v4 JSONL. A final `stream_end` closes the stream, but does not guarantee complete coverage: inspect its `collection_outcome`, `truncated`, capability coverage, and capabilities that were not started. A closed degraded collection exits with status `3`; complete collection exits with status `0`. A stream without `stream_end` is incomplete. Use `validate` to check saved streams and `summarize` to inspect coverage.

`network interfaces` reads sysfs link metadata. If `/sys/class/net` cannot be enumerated, it reports the source failure and retains available `/proc/net/dev` traffic counters with `partial` coverage (exit `3`). Those counters do not establish missing link metadata. If the fallback source is also unreadable, the capability is `unavailable`.

`vzik capture COMMAND --output-dir DIR` creates a new mode-0700 directory containing `capture.jsonl`, `receipt.json`, and `stderr.jsonl`, each mode 0600. Its parent must already exist; an existing directory, file, or symlink is rejected. This destination cannot be combined with `--output`, `--receipt`, or `--stderr`. For custom filenames, use `--output FILE --receipt FILE [--stderr FILE]`; the default stderr path appends `.stderr` to the output path. Both modes refuse to overwrite artifacts. Capture writes evidence and stderr directly, then publishes and verifies a receipt after validating the stream. Interruption can leave a directory and file prefixes without a receipt. A receipt can describe degraded evidence; it does not mean every probe succeeded.

Protocol v4 and `axe_doctor` schema v4 use sparse observations: missing values, reasons, errors, and empty evidence are omitted; observed zero, false, and empty values remain. Each isolation layer reports `indicator_present`, provider, confidence, and evidence, with a shared heuristic interpretation. UID/GID maps name the parent namespace; cgroup membership names namespace-relative paths. Pressure groups carry `visible_system` or `visible_current_cgroup` scope. Process credentials use `uids`; RSS is reported in bytes and I/O counter names distinguish read/write interfaces from accounted storage activity.

Capability coverage retains loss counts and `truncated`. The stream-level flag is `stream_end.truncated`, `summarize` exposes `stream.truncated`, and capture summaries and receipts use `stream_truncated`. There is no numeric stream-wide coverage aggregate. Record positions come from `seq`, while `counters.jsonl_bytes` counts bytes before the terminal line. Consumers must migrate to these fields; `validate` rejects protocol-v3 captures. The `baseline-v3` profile membership and order are unchanged.

Saved JSONL can be projected without changing its framing or recollecting data. Check the summary first, then use the JSONL reader's query selector or pretty `jq` output:

```bash
vzik capture overview --output-dir overview-evidence
vzik summarize overview-evidence/capture.jsonl
jq 'select(.type == "data" and .data_kind == "host_overview") | .data | {scope, detail, host: .environment.host, uid_map: .environment.restrictions.uid_map}' overview-evidence/capture.jsonl
```

For a process projection, retain `scope` and `pid` alongside the selected resource fields.

Evidence may contain sensitive host data. Keep capture, receipt, and stderr files together when sharing or archiving a collection.
