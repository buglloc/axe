use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use goblin::Object;
use memmap2::Mmap;
use serde_json::{Value, json};

const SCHEMA: &str = "goblin/v1";

#[derive(Parser)]
#[command(
    name = "goblin",
    version,
    about = "Inspect executable and object files as deterministic JSON"
)]
struct Args {
    /// Binary, object, or archive to inspect.
    file: PathBuf,

    /// Include section records.
    #[arg(long)]
    sections: bool,

    /// Include program segment records.
    #[arg(long)]
    segments: bool,

    /// Include static and dynamic symbols.
    #[arg(long)]
    symbols: bool,

    /// Include imported libraries and symbols.
    #[arg(long)]
    imports: bool,

    /// Include exported symbols.
    #[arg(long)]
    exports: bool,

    /// Include dynamic and procedure-linkage relocations.
    #[arg(long)]
    relocations: bool,

    /// Extract printable ASCII strings.
    #[arg(long)]
    strings: bool,

    /// Include a canonical hexadecimal byte dump from OFFSET.
    #[arg(long, value_name = "OFFSET", value_parser = parse_number)]
    hex: Option<usize>,

    /// Maximum collection records or strings returned.
    #[arg(long, default_value_t = 256, value_parser = parse_positive)]
    limit: usize,

    /// Collection records to skip before applying --limit.
    #[arg(long, default_value_t = 0)]
    skip: usize,

    /// Minimum string length used by --strings.
    #[arg(long, default_value_t = 4, value_parser = parse_positive)]
    min_string_length: usize,

    /// Bytes returned by --hex.
    #[arg(long, default_value_t = 256, value_parser = parse_positive)]
    length: usize,

    /// Pretty-print JSON instead of emitting one compact record.
    #[arg(long)]
    pretty: bool,
}

pub fn goblin(args: Vec<OsString>) -> i32 {
    let args = match Args::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            let _ = error.print();
            return code;
        }
    };

    match inspect(&args) {
        Ok(document) => {
            let stdout = io::stdout();
            let mut output = stdout.lock();
            let result = if args.pretty {
                serde_json::to_writer_pretty(&mut output, &document)
            } else {
                serde_json::to_writer(&mut output, &document)
            };
            if let Err(error) =
                result.and_then(|()| output.write_all(b"\n").map_err(serde_json::Error::io))
            {
                if error.io_error_kind() == Some(io::ErrorKind::BrokenPipe) {
                    return 0;
                }
                write_error("output", &args.file, &error.to_string());
                return 1;
            }
            0
        }
        Err(error) => {
            write_error(error.kind, &args.file, &error.message);
            error.code
        }
    }
}

struct InspectError {
    kind: &'static str,
    message: String,
    code: i32,
}

fn inspect(args: &Args) -> Result<Value, InspectError> {
    let file = File::open(&args.file).map_err(|error| InspectError {
        kind: "io",
        message: error.to_string(),
        code: 1,
    })?;

    // SAFETY: the mapping is read-only and remains bounded by the opened file for this invocation.
    // Concurrent file truncation can still make any mmap reader fail at the OS level; callers must not
    // mutate the inspected file while this command is running.
    let bytes = unsafe { Mmap::map(&file) }.map_err(|error| InspectError {
        kind: "io",
        message: error.to_string(),
        code: 1,
    })?;

    let object = Object::parse(&bytes).map_err(|error| InspectError {
        kind: "parse",
        message: error.to_string(),
        code: 3,
    })?;

    let mut document = match object {
        Object::Elf(elf) => inspect_elf(args, &bytes, &elf),
        Object::PE(pe) => inspect_pe(args, &bytes, &pe),
        Object::Mach(mach) => inspect_mach(args, &mach),
        Object::Archive(archive) => inspect_archive(args, &archive),
        Object::Unknown(magic) => json!({
            "schema": SCHEMA,
            "file": path_json(&args.file),
            "size": bytes.len(),
            "format": "unknown",
            "magic": hex_u64(magic),
        }),
        _ => json!({
            "schema": SCHEMA,
            "file": path_json(&args.file),
            "size": bytes.len(),
            "format": "unsupported",
        }),
    };

    let root = document
        .as_object_mut()
        .expect("inspectors always return a JSON object");
    if args.strings {
        root.insert(
            "strings".into(),
            string_page(&bytes, args.min_string_length, args.skip, args.limit),
        );
    }

    if let Some(offset) = args.hex {
        root.insert("hex".into(), hex_page(&bytes, offset, args.length));
    }

    Ok(document)
}

fn inspect_elf(args: &Args, bytes: &[u8], elf: &goblin::elf::Elf<'_>) -> Value {
    let mut root = serde_json::Map::new();
    root.insert("schema".into(), json!(SCHEMA));
    root.insert("file".into(), path_json(&args.file));
    root.insert("size".into(), json!(bytes.len()));
    root.insert("format".into(), json!("elf"));
    root.insert("bits".into(), json!(if elf.is_64 { 64 } else { 32 }));
    root.insert(
        "endian".into(),
        json!(if elf.little_endian { "little" } else { "big" }),
    );
    root.insert(
        "architecture".into(),
        json!(elf_machine(elf.header.e_machine)),
    );
    let pie = elf_is_pie(elf);
    root.insert(
        "object_type".into(),
        json!(if pie {
            "position-independent-executable"
        } else {
            elf_type(elf.header.e_type)
        }),
    );
    root.insert("entry".into(), json!(hex_u64(elf.entry)));
    root.insert("interpreter".into(), json!(elf.interpreter));
    root.insert("libraries".into(), json!(elf.libraries));
    root.insert("rpath".into(), json!(elf.rpaths));
    root.insert("runpath".into(), json!(elf.runpaths));
    root.insert("security".into(), elf_security(elf));
    root.insert(
        "counts".into(),
        json!({
            "sections": elf.section_headers.len(),
            "segments": elf.program_headers.len(),
            "symbols": elf.syms.len() + elf.dynsyms.len(),
            "dynamic_symbols": elf.dynsyms.len(),
            "relocations": elf.dynrels.len() + elf.dynrelas.len() + elf.pltrelocs.len(),
        }),
    );

    if args.sections {
        let items = elf
            .section_headers
            .iter()
            .enumerate()
            .map(|(index, section)| {
                json!({
                    "index": index,
                    "name": elf.shdr_strtab.get_at(section.sh_name),
                    "type": section.sh_type,
                    "flags": hex_u64(section.sh_flags),
                    "address": hex_u64(section.sh_addr),
                    "offset": section.sh_offset,
                    "size": section.sh_size,
                    "alignment": section.sh_addralign,
                })
            });
        root.insert(
            "sections".into(),
            page(items, elf.section_headers.len(), args),
        );
    }

    if args.segments {
        let items = elf
            .program_headers
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                json!({
                    "index": index,
                    "type": segment.p_type,
                    "flags": segment.p_flags,
                    "offset": segment.p_offset,
                    "virtual_address": hex_u64(segment.p_vaddr),
                    "physical_address": hex_u64(segment.p_paddr),
                    "file_size": segment.p_filesz,
                    "memory_size": segment.p_memsz,
                    "alignment": segment.p_align,
                })
            });
        root.insert(
            "segments".into(),
            page(items, elf.program_headers.len(), args),
        );
    }

    if args.symbols {
        let static_symbols = elf.syms.iter().map(|symbol| {
            json!({
                "table": "static",
                "name": elf.strtab.get_at(symbol.st_name),
                "value": hex_u64(symbol.st_value),
                "size": symbol.st_size,
                "binding": symbol.st_bind(),
                "type": symbol.st_type(),
                "visibility": symbol.st_visibility(),
                "section_index": symbol.st_shndx,
            })
        });
        let dynamic_symbols = elf.dynsyms.iter().map(|symbol| {
            json!({
                "table": "dynamic",
                "name": elf.dynstrtab.get_at(symbol.st_name),
                "value": hex_u64(symbol.st_value),
                "size": symbol.st_size,
                "binding": symbol.st_bind(),
                "type": symbol.st_type(),
                "visibility": symbol.st_visibility(),
                "section_index": symbol.st_shndx,
            })
        });
        let total = elf.syms.len() + elf.dynsyms.len();
        root.insert(
            "symbols".into(),
            page(static_symbols.chain(dynamic_symbols), total, args),
        );
    }

    if args.imports {
        let symbols = elf
            .dynsyms
            .iter()
            .filter(|symbol| symbol.is_import())
            .map(|symbol| {
                json!({
                    "name": elf.dynstrtab.get_at(symbol.st_name),
                    "type": symbol.st_type(),
                    "binding": symbol.st_bind(),
                })
            });
        let total = elf
            .dynsyms
            .iter()
            .filter(|symbol| symbol.is_import())
            .count();
        root.insert(
            "imports".into(),
            json!({
                "libraries": elf.libraries,
                "symbols": page(symbols, total, args),
            }),
        );
    }

    if args.exports {
        let symbols = elf
            .dynsyms
            .iter()
            .filter(|symbol| !symbol.is_import() && symbol.st_value != 0)
            .map(|symbol| {
                json!({
                    "name": elf.dynstrtab.get_at(symbol.st_name),
                    "value": hex_u64(symbol.st_value),
                    "size": symbol.st_size,
                    "type": symbol.st_type(),
                    "binding": symbol.st_bind(),
                })
            });
        let total = elf
            .dynsyms
            .iter()
            .filter(|symbol| !symbol.is_import() && symbol.st_value != 0)
            .count();
        root.insert("exports".into(), page(symbols, total, args));
    }

    if args.relocations {
        let dynamic_rel = elf
            .dynrels
            .iter()
            .map(|relocation| ("dynamic-rel", relocation));
        let dynamic_rela = elf
            .dynrelas
            .iter()
            .map(|relocation| ("dynamic-rela", relocation));
        let plt = elf
            .pltrelocs
            .iter()
            .map(|relocation| ("procedure-linkage", relocation));
        let total = elf.dynrels.len() + elf.dynrelas.len() + elf.pltrelocs.len();
        let items = dynamic_rel
            .chain(dynamic_rela)
            .chain(plt)
            .map(|(table, relocation)| {
                json!({
                    "table": table,
                    "offset": hex_u64(relocation.r_offset),
                    "symbol_index": relocation.r_sym,
                    "type": relocation.r_type,
                    "addend": relocation.r_addend,
                })
            });
        root.insert("relocations".into(), page(items, total, args));
    }

    Value::Object(root)
}

fn inspect_pe(args: &Args, bytes: &[u8], pe: &goblin::pe::PE<'_>) -> Value {
    let mut root = serde_json::Map::new();
    root.insert("schema".into(), json!(SCHEMA));
    root.insert("file".into(), path_json(&args.file));
    root.insert("size".into(), json!(bytes.len()));
    root.insert("format".into(), json!("pe"));
    root.insert("bits".into(), json!(if pe.is_64 { 64 } else { 32 }));
    root.insert(
        "architecture".into(),
        json!(pe_machine(pe.header.coff_header.machine)),
    );
    root.insert("entry".into(), json!(hex_u64(pe.entry as u64)));
    root.insert("image_base".into(), json!(hex_u64(pe.image_base)));
    root.insert("libraries".into(), json!(pe.libraries));
    root.insert(
        "counts".into(),
        json!({
            "sections": pe.sections.len(),
            "imports": pe.imports.len(),
            "exports": pe.exports.len(),
        }),
    );

    if args.sections {
        let items = pe.sections.iter().enumerate().map(|(index, section)| {
            json!({
                "index": index,
                "name": section.name().ok(),
                "virtual_address": hex_u64(section.virtual_address as u64),
                "virtual_size": section.virtual_size,
                "file_offset": section.pointer_to_raw_data,
                "file_size": section.size_of_raw_data,
                "characteristics": hex_u64(section.characteristics as u64),
            })
        });
        root.insert("sections".into(), page(items, pe.sections.len(), args));
    }

    if args.imports {
        let items = pe.imports.iter().map(|import| {
            json!({
                "library": import.dll,
                "name": import.name,
                "ordinal": import.ordinal,
                "rva": hex_u64(import.rva as u64),
            })
        });
        root.insert("imports".into(), page(items, pe.imports.len(), args));
    }

    if args.exports {
        let items = pe.exports.iter().map(|export| {
            json!({
                "name": export.name,
                "rva": hex_u64(export.rva as u64),
                "offset": export.offset,
            })
        });
        root.insert("exports".into(), page(items, pe.exports.len(), args));
    }

    Value::Object(root)
}

fn inspect_mach(args: &Args, mach: &goblin::mach::Mach<'_>) -> Value {
    match mach {
        goblin::mach::Mach::Binary(binary) => json!({
            "schema": SCHEMA,
            "file": path_json(&args.file),
            "format": "mach-o",
            "bits": if binary.is_64 { 64 } else { 32 },
            "entry": binary.entry,
            "libraries": binary.libs,
            "counts": {
                "segments": binary.segments.len(),
                "symbols": binary.symbols().count(),
            },
        }),
        goblin::mach::Mach::Fat(fat) => json!({
            "schema": SCHEMA,
            "file": path_json(&args.file),
            "format": "mach-o-fat",
            "architectures": fat.narches,
        }),
    }
}

fn inspect_archive(args: &Args, archive: &goblin::archive::Archive<'_>) -> Value {
    let members = archive.members();
    json!({
        "schema": SCHEMA,
        "file": path_json(&args.file),
        "format": "archive",
        "members": page(members.iter().map(|name| json!({ "name": name })), members.len(), args),
    })
}

fn elf_security(elf: &goblin::elf::Elf<'_>) -> Value {
    use goblin::elf::program_header::{PF_X, PT_GNU_RELRO, PT_GNU_STACK};

    let pie = elf_is_pie(elf);

    let stack = elf
        .program_headers
        .iter()
        .find(|header| header.p_type == PT_GNU_STACK);
    let nx = stack.is_some_and(|header| header.p_flags & PF_X == 0);

    let relro = elf
        .program_headers
        .iter()
        .any(|header| header.p_type == PT_GNU_RELRO);

    let bind_now = elf.dynamic.as_ref().is_some_and(|dynamic| {
        dynamic.dyns.iter().any(|entry| {
            entry.d_tag == goblin::elf::dynamic::DT_BIND_NOW
                || (entry.d_tag == goblin::elf::dynamic::DT_FLAGS
                    && entry.d_val & goblin::elf::dynamic::DF_BIND_NOW != 0)
                || (entry.d_tag == goblin::elf::dynamic::DT_FLAGS_1
                    && entry.d_val & goblin::elf::dynamic::DF_1_NOW != 0)
        })
    });

    let canary = elf.dynsyms.iter().any(|symbol| {
        matches!(
            elf.dynstrtab.get_at(symbol.st_name),
            Some("__stack_chk_fail" | "__stack_chk_guard")
        )
    });

    json!({
        "pie": pie,
        "nx": nx,
        "relro": if relro && bind_now { "full" } else if relro { "partial" } else { "none" },
        "stack_canary": canary,
        "stripped": elf.syms.is_empty(),
    })
}

fn elf_is_pie(elf: &goblin::elf::Elf<'_>) -> bool {
    elf.header.e_type == goblin::elf::header::ET_DYN
        && (elf.interpreter.is_some()
            || elf.dynamic.as_ref().is_some_and(|dynamic| {
                dynamic.dyns.iter().any(|entry| {
                    entry.d_tag == goblin::elf::dynamic::DT_FLAGS_1
                        && entry.d_val & goblin::elf::dynamic::DF_1_PIE != 0
                })
            }))
}

fn string_page(bytes: &[u8], minimum: usize, skip: usize, limit: usize) -> Value {
    let mut total = 0usize;
    let mut items = Vec::new();
    let mut start = 0usize;
    while start < bytes.len() {
        while start < bytes.len() && !is_string_byte(bytes[start]) {
            start += 1;
        }
        let mut end = start;
        while end < bytes.len() && is_string_byte(bytes[end]) {
            end += 1;
        }
        if end.saturating_sub(start) >= minimum {
            if total >= skip && items.len() < limit {
                items.push(json!({
                    "offset": start,
                    "offset_hex": hex_u64(start as u64),
                    "value": String::from_utf8_lossy(&bytes[start..end]),
                }));
            }
            total += 1;
        }
        start = end.saturating_add(1);
    }

    json!({
        "items": items,
        "total": total,
        "skip": skip,
        "limit": limit,
        "truncated": skip.saturating_add(limit) < total,
    })
}

fn is_string_byte(byte: u8) -> bool {
    byte == b'\t' || (0x20..=0x7e).contains(&byte)
}

fn hex_page(bytes: &[u8], offset: usize, length: usize) -> Value {
    let start = offset.min(bytes.len());
    let end = start.saturating_add(length).min(bytes.len());
    let lines = bytes[start..end]
        .chunks(16)
        .enumerate()
        .map(|(index, chunk)| {
            let address = start + index * 16;
            let hex = chunk
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let ascii = chunk
                .iter()
                .map(|byte| {
                    if (0x20..=0x7e).contains(byte) {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect::<String>();
            json!({
                "offset": address,
                "offset_hex": hex_u64(address as u64),
                "bytes": hex,
                "ascii": ascii,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "offset": start,
        "requested_length": length,
        "returned_length": end - start,
        "file_size": bytes.len(),
        "eof": end == bytes.len(),
        "lines": lines,
    })
}

fn page<I>(items: I, total: usize, args: &Args) -> Value
where
    I: IntoIterator<Item = Value>,
{
    let items = items
        .into_iter()
        .skip(args.skip)
        .take(args.limit)
        .collect::<Vec<_>>();

    json!({
        "items": items,
        "total": total,
        "skip": args.skip,
        "limit": args.limit,
        "truncated": args.skip.saturating_add(args.limit) < total,
    })
}

fn elf_machine(machine: u16) -> &'static str {
    use goblin::elf::header::*;
    match machine {
        EM_386 => "x86",
        EM_X86_64 => "x86-64",
        EM_ARM => "arm",
        EM_AARCH64 => "aarch64",
        EM_MIPS => "mips",
        EM_PPC => "powerpc",
        EM_PPC64 => "powerpc64",
        EM_RISCV => "riscv",
        EM_S390 => "s390",
        _ => "unknown",
    }
}

fn elf_type(kind: u16) -> &'static str {
    use goblin::elf::header::*;
    match kind {
        ET_NONE => "none",
        ET_REL => "relocatable",
        ET_EXEC => "executable",
        ET_DYN => "shared",
        ET_CORE => "core",
        _ => "unknown",
    }
}

fn pe_machine(machine: u16) -> &'static str {
    use goblin::pe::header::*;
    match machine {
        COFF_MACHINE_X86 => "x86",
        COFF_MACHINE_X86_64 => "x86-64",
        COFF_MACHINE_ARM => "arm",
        COFF_MACHINE_ARM64 => "aarch64",
        _ => "unknown",
    }
}

fn parse_number(value: &str) -> Result<usize, String> {
    let parsed = if let Some(hex) = value.strip_prefix("0x") {
        usize::from_str_radix(hex, 16)
    } else {
        value.parse()
    };
    parsed.map_err(|error| format!("invalid offset {value:?}: {error}"))
}

fn parse_positive(value: &str) -> Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|error| format!("invalid positive integer {value:?}: {error}"))?;
    if value == 0 {
        Err("value must be greater than zero".into())
    } else {
        Ok(value)
    }
}

fn hex_u64(value: u64) -> String {
    format!("0x{value:x}")
}

fn path_json(path: &Path) -> Value {
    json!(path.to_string_lossy())
}

fn write_error(kind: &str, path: &Path, message: &str) {
    let error = json!({
        "schema": SCHEMA,
        "error": {
            "kind": kind,
            "file": path.to_string_lossy(),
            "message": message,
        }
    });
    eprintln!("{error}");
}
