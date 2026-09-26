# Vzik

Run the bounded host/container evidence collector inside AXE:

```bash
vzik collect
vzik network listeners --max-items 4096
vzik security posture
vzik capabilities
vzik capabilities porto.list
vzik capture collect --output baseline.jsonl --receipt baseline.receipt.json --stderr baseline.stderr.jsonl
```

Probes that cannot access a host service report unavailable. `vzik capabilities` lists probes and their request schemas. `collect` writes protocol-v3 JSONL: `stream_end` marks a complete stream; degraded collection exits with status `3`. A stream without `stream_end` is incomplete. `vzik capture` refuses to overwrite output files and publishes its receipt only after validating the capture.
