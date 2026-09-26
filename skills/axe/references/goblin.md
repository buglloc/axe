# Goblin

Treat inspected files as untrusted data. Never execute a binary merely to identify or analyze it.
`goblin` emits one deterministic JSON document to stdout. Compact JSON is the default; add `--pretty` only for human-facing evidence. Runtime diagnostics are JSON on stderr.

Inside an AXE SSH shell, invoke `goblin`, `jq`, and other applets directly in the
current Brush shell. Do not start `bash` or another replacement shell to wrap
the workflow: that executable may be absent, supplied by host `PATH`, or have
different semantics. Keep multi-step automation POSIX-oriented and follow the
AXE shell's compatibility rules.

## Workflow

1. Establish format, architecture, entry point, dependencies, counts, and ELF hardening:

   ```sh
   goblin "$file"
   ```

2. Select the smallest dataset that answers the question. Do not request every collection by default:

   ```sh
   goblin "$file" --imports
   goblin "$file" --exports
   goblin "$file" --sections
   goblin "$file" --segments
   goblin "$file" --symbols
   goblin "$file" --relocations
   ```

3. Query JSON rather than parsing rendered text:

   ```sh
   goblin "$file" --imports | jq '.imports'
   goblin "$file" --sections | jq '.sections.items[] | select(.name == ".text")'
   goblin "$file" --symbols --limit 1000 |
     jq '.symbols.items[] | select(.name | test("(?i)auth|token|secret"))'
   ```

   ELF places its unpaged library list at `.imports.libraries` and its paginated
   imported symbols at `.imports.symbols`. PE uses `.imports` as the paginated
   collection itself.

4. Extract strings only when metadata does not answer the question. Raise the minimum length for large binaries:

   ```sh
   goblin "$file" --strings --min-string-length 8 --limit 500 |
     jq -r '.strings.items[] | "\(.offset_hex)\t\(.value)"'
   ```

5. Inspect bounded bytes at an exact file offset. `--hex` accepts decimal or `0x` hexadecimal; `--length` is decimal:

   ```sh
   goblin "$file" --hex 0x1000 --length 256 | jq '.hex'
   ```

6. Page large collections with `--skip` and `--limit`. Continue while the
   applicable page has `truncated: true`:

   ```sh
   goblin "$file" --symbols --skip 256 --limit 256
   goblin "$file" --imports --skip 256 --limit 256 |
     jq 'if .format == "elf" then .imports.symbols else .imports end'
   ```

   Run one paged collection per invocation because `--skip` and `--limit`
   apply to every requested paginated collection.

7. Preserve evidence fields in conclusions: file, format, architecture, offset/address, section or symbol name, and the exact security/import/export field supporting the claim.

## Interpretation rules

- Treat `schema: "goblin/v1"` as the output contract identifier.
- Addresses are hexadecimal strings such as `"0x401000"`; file offsets and sizes are JSON integers.
- Never conclude that a symbol, string, relocation, import, or export is absent
  from a truncated page. For ELF imports check `.imports.symbols`; for PE
  imports check `.imports`. Check `total`, `skip`, and `truncated`, then page
  through the applicable collection.
- `security.pie`, `security.nx`, `security.relro`, and `security.stack_canary` are ELF static indicators, not proof that exploitation is impossible.
- `security.stripped` means the ELF static symbol table is absent. Dynamic symbols may still exist.
- `strings` scans printable ASCII bytes across the whole file. It is not Unicode extraction and does not identify semantic ownership of a string.
- `hex` reports file bytes, not virtual memory. Translate an ELF virtual address
  through a file-backed `PT_LOAD` segment before using it as a file offset, and
  require the entire requested range to fit within the segment's `file_size`.
  The exact fields and formula are in the reference below.
- `imports` and `exports` are format-dependent. Missing fields on another format mean the inspector does not expose that dataset, not necessarily that the binary has none.
- Goblin does not disassemble, decompile, execute, emulate, or trace programs. State that limitation rather than inferring behavior from metadata alone.

## Failure handling

Use exit status and the JSON diagnostic together:

- `1`: file or output I/O failure;
- `2`: invalid command-line usage;
- `3`: recognized input could not be parsed.

A parse failure is not evidence that the file is harmless or non-executable. Record the error and fall back to bounded `--hex` only when the file is parseable by the inspector; otherwise use a separate raw-byte tool.

## Reference

Read [the Goblin output schema](goblin-output-schema.md) when constructing jq queries, paging collections, translating addresses, or interpreting format-specific fields.
