# Goblin output schema

## Contents

- [Common envelope](#common-envelope)
- [Pagination](#pagination)
- [ELF](#elf)
- [PE](#pe)
- [Mach-O and archives](#mach-o-and-archives)
- [Strings](#strings)
- [Hex](#hex)
- [Errors](#errors)

## Common envelope

Every successful document is a JSON object containing:

| Field | Type | Meaning |
|---|---|---|
| `schema` | string | Currently `goblin/v1` |
| `file` | string | Input path as supplied by the shell |
| `format` | string | `elf`, `pe`, `mach-o`, `mach-o-fat`, `archive`, `unknown`, or `unsupported` |
| `size` | integer | File size where exposed |

Collection fields appear only when requested. Summary fields remain present alongside requested collections.

## Pagination

Sections, segments, symbols, relocations, exports, strings, archive members, and
PE imports use this shape at their named top-level field. ELF uses the same page
shape at `.imports.symbols`; `.imports.libraries` is an unpaged array:

```json
{
  "items": [],
  "total": 0,
  "skip": 0,
  "limit": 256,
  "truncated": false
}
```

`total` is the number of records before paging. `truncated` means records exist after `skip + limit`; it does not indicate records skipped before the current page. To prove complete coverage, begin at zero and advance `skip` by the number of returned items until `skip + items.length >= total`.

For ELF imports:

```json
{
  "imports": {
    "libraries": ["libc.so.6"],
    "symbols": {
      "items": [],
      "total": 0,
      "skip": 0,
      "limit": 256,
      "truncated": false
    }
  }
}
```

## ELF

Summary fields:

| Field | Meaning |
|---|---|
| `bits` | ELF class, 32 or 64 |
| `endian` | `little` or `big` |
| `architecture` | Normalized machine family |
| `object_type` | `relocatable`, `executable`, `position-independent-executable`, `shared`, `core`, or fallback |
| `entry` | Hexadecimal virtual entry address |
| `interpreter` | Dynamic loader path or null |
| `libraries` | DT_NEEDED libraries |
| `rpath`, `runpath` | Runtime library paths |
| `counts` | Dataset sizes before paging |
| `security` | Static hardening indicators |

Security fields:

```json
{
  "pie": true,
  "nx": true,
  "relro": "full",
  "stack_canary": false,
  "stripped": true
}
```

`relro` is `none`, `partial`, or `full`. Full means GNU_RELRO plus immediate binding. `stack_canary` is inferred from dynamic imports of `__stack_chk_fail` or `__stack_chk_guard`; statically linked or renamed mechanisms may not be detected.

Section records expose `index`, `name`, numeric `type`, hexadecimal `flags` and `address`, and integer `offset`, `size`, and `alignment`.

Segment records expose numeric `index`, `type`, and `flags`; integer `offset`,
`file_size`, `memory_size`, and `alignment`; and hexadecimal
`virtual_address` and `physical_address`. ELF `PT_LOAD` has `type: 1`.

To translate a virtual address `address` through a file-backed `PT_LOAD`:

```text
file_offset = offset + (address - virtual_address)
```

Require `virtual_address <= address` and
`address + length <= virtual_address + file_size` before reading `length`
bytes. Containment only within `memory_size` is insufficient because the
remaining range can be zero-filled memory with no corresponding file bytes.
If no segment or multiple segments satisfy the range, report the mapping as
absent or ambiguous instead of guessing.

Symbol records expose `table`, `name`, hexadecimal `value`, integer `size`, numeric binding/type/visibility, and `section_index`.

Relocation records expose `table`, hexadecimal `offset`, `symbol_index`, numeric `type`, and optional signed `addend`.

## PE

Summary fields include `bits`, `architecture`, hexadecimal `entry` and `image_base`, linked `libraries`, and collection `counts`.

PE section records expose the name, RVA, virtual size, raw-file offset/size, and
hexadecimal characteristics. `.imports` is a page whose items expose `library`,
`name`, `ordinal`, and RVA. `.exports` is a page whose items expose `name`, RVA,
and a file offset when resolvable.

## Mach-O and archives

Mach-O summary exposes class, entry, linked libraries, and counts. Fat Mach-O exposes its architecture count. Detailed Mach-O collection flags are not currently emitted.

Archive output exposes a paginated `members` collection. Inspect an extracted member separately for format-specific metadata.

## Strings

`--strings` adds:

```json
{
  "strings": {
    "items": [
      {
        "offset": 4096,
        "offset_hex": "0x1000",
        "value": "printable bytes"
      }
    ],
    "total": 1,
    "skip": 0,
    "limit": 256,
    "truncated": false
  }
}
```

Offsets are raw file offsets. The extractor accepts horizontal tab and printable ASCII bytes and stops at other bytes.

## Hex

`--hex OFFSET --length LENGTH` adds:

```json
{
  "hex": {
    "offset": 4096,
    "requested_length": 32,
    "returned_length": 32,
    "file_size": 8192,
    "eof": false,
    "lines": [
      {
        "offset": 4096,
        "offset_hex": "0x1000",
        "bytes": "7f 45 4c 46",
        "ascii": ".ELF"
      }
    ]
  }
}
```

Each line contains at most 16 bytes. An offset beyond EOF is clamped to EOF and returns no lines.

## Errors

Runtime errors are one compact JSON document on stderr:

```json
{
  "schema": "goblin/v1",
  "error": {
    "kind": "io",
    "file": "sample",
    "message": "No such file or directory (os error 2)"
  }
}
```

`kind` is currently `io`, `parse`, or `output`. CLI parser diagnostics use the standard command help/error format and exit status 2.
