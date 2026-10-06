# Vzik

`vzik` collects bounded host and container evidence. Inside AXE, run:

```bash
vzik collect
vzik network listeners --max-items 4096
vzik security posture
vzik capabilities
vzik capabilities porto.list
vzik capture collect --output baseline.jsonl --receipt baseline.receipt.json --stderr baseline.stderr.jsonl
vzik validate baseline.jsonl
vzik summarize baseline.jsonl
```

`vzik capabilities` lists probes and global limits. Pass a probe ID, as in `vzik capabilities porto.list`, for its request schema and access requirements. A probe reports unavailable when it cannot access a host service; unsupported probes are reported rather than silently omitted.

Collection writes protocol-v3 JSONL. A final `stream_end` closes the stream, but does not guarantee complete coverage: inspect its outcome, coverage, and capabilities that were not started. A closed degraded collection exits with status `3`; complete collection exits with status `0`. A stream without `stream_end` is incomplete. Use `validate` to check saved streams and `summarize` to inspect coverage.

`vzik capture` refuses to overwrite files. It writes capture and stderr files directly, then publishes and verifies a receipt after validating the stream. Interruption can leave files without a receipt. A receipt can describe degraded evidence; it does not mean every probe succeeded.

Evidence may contain sensitive host data. Keep capture, receipt, and stderr files together when sharing or archiving a collection.
