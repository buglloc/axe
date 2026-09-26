use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, SyncSender};

use goblin::Object;
use memmap2::Mmap;
use serde::Serialize;

const PARALLEL_OUTPUT_CHUNK: usize = 64 * 1024;
const PARALLEL_OUTPUT_QUEUE: usize = 2;

const HELP: &str = "Usage: strings [OPTION]... [FILE]...\n\
Display printable strings in FILEs (standard input by default).\n\
\n\
  -a, --all                 scan the entire file (default)\n\
  -d, --data                scan initialized, loaded object-file sections\n\
  -f, --print-file-name     print the file name before each string\n\
  -n, --bytes=NUMBER        require at least NUMBER characters (default: 4)\n\
  -t, --radix={o,d,x}       print offsets in octal, decimal, or hexadecimal\n\
  -w, --include-all-whitespace\n\
                            include all whitespace in strings\n\
  -o                        alias for --radix=o\n\
  -e, --encoding={s,S,b,l,B,L,utf8,utf16le}\n\
                            select input character encoding\n\
  -s, --output-separator=STRING\n\
                            separate output records with STRING\n\
      --json                emit one JSON object per string\n\
      --null                terminate output records with NUL\n\
      --parallel[=JOBS]     scan input files concurrently, preserving order\n\
  -h, --help                display this help and exit\n\
  -v, -V, --version         output version information and exit";

#[derive(Clone, Copy)]
enum Encoding {
    SevenBit,
    EightBit,
    Big16,
    Little16,
    Big32,
    Little32,
    Utf8,
    Utf16Le,
}

impl Encoding {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "s" => Ok(Self::SevenBit),
            "S" => Ok(Self::EightBit),
            "b" => Ok(Self::Big16),
            "l" => Ok(Self::Little16),
            "B" => Ok(Self::Big32),
            "L" => Ok(Self::Little32),
            "utf8" | "utf-8" => Ok(Self::Utf8),
            "utf16le" | "utf-16le" => Ok(Self::Utf16Le),
            _ => Err(format!("invalid encoding '{value}'")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::SevenBit => "s",
            Self::EightBit => "S",
            Self::Big16 => "b",
            Self::Little16 => "l",
            Self::Big32 => "B",
            Self::Little32 => "L",
            Self::Utf8 => "utf8",
            Self::Utf16Le => "utf16le",
        }
    }
}

#[derive(Clone, Copy)]
enum Radix {
    Octal,
    Decimal,
    Hexadecimal,
}

impl Radix {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "o" => Ok(Self::Octal),
            "d" => Ok(Self::Decimal),
            "x" => Ok(Self::Hexadecimal),
            _ => Err(format!("invalid radix '{value}'")),
        }
    }
}

struct Options {
    data_only: bool,
    print_file_name: bool,
    minimum: usize,
    radix: Option<Radix>,
    include_all_whitespace: bool,
    encoding: Encoding,
    separator: Vec<u8>,
    json: bool,
    parallelism: Option<usize>,
    files: Vec<OsString>,
}

enum ParseOutcome {
    Run(Options),
    Exit(i32),
}

impl Options {
    fn parse(args: impl IntoIterator<Item = OsString>) -> Result<ParseOutcome, String> {
        let mut options = Self {
            data_only: false,
            print_file_name: false,
            minimum: 4,
            radix: None,
            include_all_whitespace: false,
            encoding: Encoding::SevenBit,
            separator: vec![b'\n'],
            json: false,
            parallelism: None,
            files: Vec::new(),
        };
        let mut args = args.into_iter();
        let mut positional = false;

        while let Some(argument) = args.next() {
            let Some(text) = argument.to_str() else {
                options.files.push(argument);
                continue;
            };

            if positional {
                options.files.push(argument);
                continue;
            }
            if text == "--" {
                positional = true;
                continue;
            }
            if text == "-" {
                options.data_only = false;
                continue;
            }
            if !text.starts_with('-') || text == "-" {
                options.files.push(argument);
                continue;
            }

            if let Some(long) = text.strip_prefix("--") {
                let (name, attached) = long
                    .split_once('=')
                    .map_or((long, None), |(name, value)| (name, Some(value)));
                match name {
                    "all" if attached.is_none() => options.data_only = false,
                    "data" if attached.is_none() => options.data_only = true,
                    "print-file-name" if attached.is_none() => options.print_file_name = true,
                    "include-all-whitespace" if attached.is_none() => {
                        options.include_all_whitespace = true;
                    }
                    "json" if attached.is_none() => options.json = true,
                    "null" if attached.is_none() => options.separator = vec![0],
                    "parallel" => {
                        options.parallelism = Some(match attached {
                            Some(value) => parse_positive(value, "parallel job count")?,
                            None => std::thread::available_parallelism()
                                .map(usize::from)
                                .unwrap_or(1),
                        });
                    }
                    "bytes" => {
                        let value = option_value(attached, &mut args, "--bytes")?;
                        options.minimum = parse_positive(&value, "minimum string length")?;
                    }
                    "radix" => {
                        let value = option_value(attached, &mut args, "--radix")?;
                        options.radix = Some(Radix::parse(&value)?);
                    }
                    "encoding" => {
                        let value = option_value(attached, &mut args, "--encoding")?;
                        options.encoding = Encoding::parse(&value)?;
                    }
                    "output-separator" => {
                        options.separator =
                            option_os_value(attached, &mut args, "--output-separator")?;
                    }
                    "help" if attached.is_none() => {
                        println!("{HELP}");
                        return Ok(ParseOutcome::Exit(0));
                    }
                    "version" if attached.is_none() => {
                        print_version();
                        return Ok(ParseOutcome::Exit(0));
                    }
                    _ => return Err(format!("unrecognized option '--{long}'")),
                }
                continue;
            }

            if text[1..].bytes().all(|byte| byte.is_ascii_digit()) {
                options.minimum = parse_positive(&text[1..], "minimum string length")?;
                continue;
            }

            for (index, short) in text[1..].char_indices() {
                match short {
                    'a' => options.data_only = false,
                    'd' => options.data_only = true,
                    'f' => options.print_file_name = true,
                    'w' => options.include_all_whitespace = true,
                    'o' => options.radix = Some(Radix::Octal),
                    'h' => {
                        println!("{HELP}");
                        return Ok(ParseOutcome::Exit(0));
                    }
                    'v' | 'V' => {
                        print_version();
                        return Ok(ParseOutcome::Exit(0));
                    }
                    'n' | 't' | 'e' | 's' => {
                        let value_start = 1 + index + short.len_utf8();
                        let attached = (value_start < text.len()).then(|| &text[value_start..]);
                        match short {
                            'n' => {
                                let value = option_value(attached, &mut args, "-n")?;
                                options.minimum = parse_positive(&value, "minimum string length")?;
                            }
                            't' => {
                                let value = option_value(attached, &mut args, "-t")?;
                                options.radix = Some(Radix::parse(&value)?);
                            }
                            'e' => {
                                let value = option_value(attached, &mut args, "-e")?;
                                options.encoding = Encoding::parse(&value)?;
                            }
                            's' => {
                                options.separator = option_os_value(attached, &mut args, "-s")?;
                            }
                            _ => unreachable!(),
                        }
                        break;
                    }
                    _ => return Err(format!("invalid option -- '{short}'")),
                }
            }
        }

        Ok(ParseOutcome::Run(options))
    }
}

fn option_value(
    attached: Option<&str>,
    args: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<String, String> {
    if let Some(value) = attached {
        return Ok(value.to_owned());
    }

    args.next()
        .ok_or_else(|| format!("option '{option}' requires an argument"))?
        .into_string()
        .map_err(|_| format!("argument for '{option}' is not valid UTF-8"))
}

fn option_os_value(
    attached: Option<&str>,
    args: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<Vec<u8>, String> {
    let value = match attached {
        Some(value) => OsString::from(value),
        None => args
            .next()
            .ok_or_else(|| format!("option '{option}' requires an argument"))?,
    };
    Ok(value.as_encoded_bytes().to_vec())
}

fn parse_positive(value: &str, description: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("invalid {description} '{value}'"))
}

fn print_version() {
    println!("strings (axe) {}", env!("CARGO_PKG_VERSION"));
}

pub fn strings(args: Vec<OsString>) -> i32 {
    let options = match Options::parse(args.into_iter().skip(1)) {
        Ok(ParseOutcome::Run(options)) => options,
        Ok(ParseOutcome::Exit(code)) => return code,
        Err(error) => {
            eprintln!("strings: {error}");
            eprintln!("Try 'strings --help' for more information.");
            return 1;
        }
    };

    let stdout = io::stdout();
    let mut output = BufWriter::with_capacity(64 * 1024, stdout.lock());
    let result = if options.files.is_empty() {
        render_stdin(&options, &mut output).map_err(|error| (None, error))
    } else if options.parallelism.is_some_and(|jobs| jobs > 1) && options.files.len() > 1 {
        render_parallel(&options, &mut output)
    } else {
        render_sequential(&options, &mut output)
    };

    let failed = match result {
        Ok(failed) => failed,
        Err((path, RunError::Input(error))) => {
            print_input_error(path.as_deref(), &error);
            true
        }
        Err((_, RunError::Output(error))) if error.kind() == io::ErrorKind::BrokenPipe => {
            return 0;
        }
        Err((_, RunError::Output(error))) => {
            eprintln!("strings: write error: {error}");
            return 1;
        }
    };

    if let Err(error) = output.flush() {
        return if error.kind() == io::ErrorKind::BrokenPipe {
            0
        } else {
            eprintln!("strings: write error: {error}");
            1
        };
    }

    i32::from(failed)
}

#[derive(Debug)]
enum RunError {
    Input(String),
    Output(io::Error),
}

fn render_stdin(options: &Options, output: &mut impl Write) -> Result<bool, RunError> {
    if !options.data_only && matches!(options.encoding, Encoding::SevenBit | Encoding::EightBit) {
        let stdin = io::stdin();
        scan_single_byte_reader(
            stdin.lock(),
            OsStr::new("{standard input}"),
            options,
            output,
        )?;
        return Ok(false);
    }

    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .read_to_end(&mut bytes)
        .map_err(|error| RunError::Input(error.to_string()))?;

    render_bytes(&bytes, OsStr::new("{standard input}"), options, output)?;
    Ok(false)
}

fn scan_single_byte_reader(
    mut reader: impl Read,
    label: &OsStr,
    options: &Options,
    output: &mut impl Write,
) -> Result<(), RunError> {
    let eight_bit = matches!(options.encoding, Encoding::EightBit);
    let mut emitter = Emitter {
        output,
        label,
        options,
    };
    let mut input = [0; 64 * 1024];
    let mut run = Vec::with_capacity(256);
    let mut offset = 0u64;
    let mut run_start = 0u64;

    loop {
        let read = match reader.read(&mut input) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(RunError::Input(error.to_string())),
        };

        for byte in &input[..read] {
            if is_display_byte(*byte, eight_bit, options.include_all_whitespace) {
                if run.is_empty() {
                    run_start = offset;
                }
                run.push(*byte);
            } else {
                if run.len() >= options.minimum {
                    emitter.emit(run_start, &run)?;
                }
                run.clear();
            }
            offset += 1;
        }
    }

    if run.len() >= options.minimum {
        emitter.emit(run_start, &run)?;
    }
    Ok(())
}

fn render_sequential(
    options: &Options,
    output: &mut impl Write,
) -> Result<bool, (Option<OsString>, RunError)> {
    let mut failed = false;

    for path in &options.files {
        match render_path(path, options, output) {
            Ok(()) => {}
            Err(RunError::Input(error)) => {
                print_input_error(Some(path), &error);
                failed = true;
            }
            Err(error @ RunError::Output(_)) => return Err((Some(path.clone()), error)),
        }
    }

    Ok(failed)
}

fn render_parallel(
    options: &Options,
    output: &mut impl Write,
) -> Result<bool, (Option<OsString>, RunError)> {
    let jobs = options
        .parallelism
        .unwrap_or(1)
        .min(options.files.len())
        .max(1);
    let mut failed = false;

    for batch in options.files.chunks(jobs) {
        let batch_failed = std::thread::scope(|scope| {
            let mut receivers = Vec::with_capacity(batch.len());
            let mut handles = Vec::with_capacity(batch.len());
            for path in batch {
                let (sender, receiver) = mpsc::sync_channel(PARALLEL_OUTPUT_QUEUE);
                receivers.push(receiver);
                handles.push(scope.spawn(move || {
                    let mut writer = ChannelWriter::new(&sender);
                    let render_result = render_path(path, options, &mut writer);
                    let result = match writer.flush() {
                        Ok(()) => render_result,
                        Err(error) => Err(RunError::Output(error)),
                    };
                    let _ = sender.send(RenderMessage::Done(result));
                }));
            }

            let mut batch_failed = false;
            for ((path, receiver), handle) in batch.iter().zip(receivers).zip(handles) {
                let mut completed = None;
                while let Ok(message) = receiver.recv() {
                    match message {
                        RenderMessage::Chunk(bytes) => output
                            .write_all(&bytes)
                            .map_err(|error| (Some(path.clone()), RunError::Output(error)))?,
                        RenderMessage::Done(result) => {
                            completed = Some(result);
                            break;
                        }
                    }
                }
                if handle.join().is_err() {
                    print_input_error(Some(path), "parallel scanner thread panicked");
                    batch_failed = true;
                    continue;
                }
                match completed {
                    Some(Ok(())) => {}
                    Some(Err(RunError::Input(error))) => {
                        print_input_error(Some(path), &error);
                        batch_failed = true;
                    }
                    Some(Err(error @ RunError::Output(_))) => {
                        return Err((Some(path.clone()), error));
                    }
                    None => {
                        print_input_error(Some(path), "parallel scanner channel closed");
                        batch_failed = true;
                    }
                }
            }
            Ok(batch_failed)
        })?;
        failed |= batch_failed;
    }

    Ok(failed)
}

enum RenderMessage {
    Chunk(Vec<u8>),
    Done(Result<(), RunError>),
}

struct ChannelWriter<'a> {
    sender: &'a SyncSender<RenderMessage>,
    buffer: Vec<u8>,
}

impl<'a> ChannelWriter<'a> {
    fn new(sender: &'a SyncSender<RenderMessage>) -> Self {
        Self {
            sender,
            buffer: Vec::with_capacity(PARALLEL_OUTPUT_CHUNK),
        }
    }

    fn send_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::replace(&mut self.buffer, Vec::with_capacity(PARALLEL_OUTPUT_CHUNK));
        self.sender
            .send(RenderMessage::Chunk(bytes))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "parallel output closed"))
    }
}

impl Write for ChannelWriter<'_> {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let written = bytes.len();
        while !bytes.is_empty() {
            let available = PARALLEL_OUTPUT_CHUNK - self.buffer.len();
            let take = available.min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() == PARALLEL_OUTPUT_CHUNK {
                self.send_buffer()?;
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_buffer()
    }
}

fn print_input_error(path: Option<&OsStr>, error: &str) {
    match path {
        Some(path) => eprintln!("strings: '{}': {error}", path.to_string_lossy()),
        None => eprintln!("strings: {error}"),
    }
}

fn render_path(path: &OsStr, options: &Options, output: &mut impl Write) -> Result<(), RunError> {
    let path_ref = Path::new(path);
    let mut file = File::open(path_ref).map_err(|error| RunError::Input(error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| RunError::Input(error.to_string()))?;

    if metadata.is_file() && metadata.len() > 0 {
        // SAFETY: the read-only mapping cannot outlive `file` in this branch and the scanner only
        // reads within the mapped slice. As with other mmap readers, callers must not truncate a
        // file concurrently with this invocation.
        if let Ok(mapping) = unsafe { Mmap::map(&file) } {
            return render_bytes(&mapping, path, options, output);
        }
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| RunError::Input(error.to_string()))?;
    render_bytes(&bytes, path, options, output)
}

fn render_bytes(
    bytes: &[u8],
    label: &OsStr,
    options: &Options,
    output: &mut impl Write,
) -> Result<(), RunError> {
    if options.data_only {
        scan_data_object(bytes, label, options, output, 0)
    } else {
        scan_region(bytes, 0, label, options, output)
    }
}

fn scan_data_object(
    bytes: &[u8],
    label: &OsStr,
    options: &Options,
    output: &mut impl Write,
    depth: usize,
) -> Result<(), RunError> {
    if depth > 8 {
        return Err(RunError::Input("object nesting is too deep".into()));
    }

    let object = Object::parse(bytes).map_err(|error| RunError::Input(error.to_string()))?;
    match object {
        Object::Elf(elf) => {
            use goblin::elf::section_header::{SHF_ALLOC, SHT_NOBITS};

            for section in &elf.section_headers {
                if section.sh_flags & u64::from(SHF_ALLOC) == 0 || section.sh_type == SHT_NOBITS {
                    continue;
                }
                let Some((offset, data)) =
                    checked_region(bytes, section.sh_offset, section.sh_size)
                else {
                    continue;
                };
                scan_region(data, offset, label, options, output)?;
            }
        }
        Object::PE(pe) => {
            const IMAGE_SCN_MEMORY: u32 = 0xe000_0000;

            for section in &pe.sections {
                if section.characteristics & IMAGE_SCN_MEMORY == 0 {
                    continue;
                }
                let Some((offset, data)) = checked_region(
                    bytes,
                    u64::from(section.pointer_to_raw_data),
                    u64::from(section.size_of_raw_data),
                ) else {
                    continue;
                };
                scan_region(data, offset, label, options, output)?;
            }
        }
        Object::Mach(goblin::mach::Mach::Binary(binary)) => {
            for segment in &binary.segments {
                if segment.initprot == 0 {
                    continue;
                }
                for section in segment {
                    let (section, data) =
                        section.map_err(|error| RunError::Input(error.to_string()))?;
                    if !data.is_empty() {
                        scan_region(data, u64::from(section.offset), label, options, output)?;
                    }
                }
            }
        }
        Object::Archive(archive) => {
            for index in 0..archive.len() {
                let Some(member) = archive.get_at(index) else {
                    continue;
                };
                let start = usize::try_from(member.offset).ok();
                let member_bytes = start.and_then(|start| {
                    start
                        .checked_add(member.size())
                        .and_then(|end| bytes.get(start..end))
                });
                let Some(member_bytes) = member_bytes else {
                    continue;
                };
                if Object::parse(member_bytes).is_err() {
                    continue;
                }

                let member_label =
                    format!("{}({})", label.to_string_lossy(), member.extended_name());
                scan_data_object(
                    member_bytes,
                    OsStr::new(&member_label),
                    options,
                    output,
                    depth + 1,
                )?;
            }
        }
        _ => return Err(RunError::Input("file format not recognized".into())),
    }

    Ok(())
}

fn checked_region(bytes: &[u8], offset: u64, size: u64) -> Option<(u64, &[u8])> {
    let start = usize::try_from(offset).ok()?;
    let size = usize::try_from(size).ok()?;
    let end = start.checked_add(size)?;
    Some((offset, bytes.get(start..end)?))
}

fn scan_region(
    bytes: &[u8],
    base_offset: u64,
    label: &OsStr,
    options: &Options,
    output: &mut impl Write,
) -> Result<(), RunError> {
    let mut emitter = Emitter {
        output,
        label,
        options,
    };
    let mut emit = |offset, value: &[u8]| emitter.emit(offset, value);

    match options.encoding {
        Encoding::SevenBit => scan_single_byte(bytes, base_offset, options, false, &mut emit),
        Encoding::EightBit => scan_single_byte(bytes, base_offset, options, true, &mut emit),
        Encoding::Big16 => scan_wide(bytes, base_offset, options, 2, true, &mut emit),
        Encoding::Little16 => scan_wide(bytes, base_offset, options, 2, false, &mut emit),
        Encoding::Big32 => scan_wide(bytes, base_offset, options, 4, true, &mut emit),
        Encoding::Little32 => scan_wide(bytes, base_offset, options, 4, false, &mut emit),
        Encoding::Utf8 => scan_utf8(bytes, base_offset, options, &mut emit),
        Encoding::Utf16Le => scan_utf16le(bytes, base_offset, options, &mut emit),
    }
}

fn scan_single_byte(
    bytes: &[u8],
    base_offset: u64,
    options: &Options,
    eight_bit: bool,
    emit: &mut impl FnMut(u64, &[u8]) -> Result<(), RunError>,
) -> Result<(), RunError> {
    let mut cursor = 0;

    while cursor < bytes.len() {
        while cursor < bytes.len()
            && !is_display_byte(bytes[cursor], eight_bit, options.include_all_whitespace)
        {
            cursor += 1;
        }
        let start = cursor;
        while cursor < bytes.len()
            && is_display_byte(bytes[cursor], eight_bit, options.include_all_whitespace)
        {
            cursor += 1;
        }
        if cursor - start >= options.minimum {
            emit(base_offset + start as u64, &bytes[start..cursor])?;
        }
    }

    Ok(())
}

fn scan_wide(
    bytes: &[u8],
    base_offset: u64,
    options: &Options,
    width: usize,
    big_endian: bool,
    emit: &mut impl FnMut(u64, &[u8]) -> Result<(), RunError>,
) -> Result<(), RunError> {
    let mut cursor = 0;
    let mut run_start = 0;
    let mut run = Vec::new();

    while cursor + width <= bytes.len() {
        let unit = &bytes[cursor..cursor + width];
        let value = if big_endian {
            unit.iter()
                .fold(0u32, |value, byte| (value << 8) | u32::from(*byte))
        } else {
            unit.iter()
                .rev()
                .fold(0u32, |value, byte| (value << 8) | u32::from(*byte))
        };
        let display = u8::try_from(value)
            .ok()
            .filter(|byte| is_display_byte(*byte, true, options.include_all_whitespace));

        if let Some(byte) = display {
            if run.is_empty() {
                run_start = cursor;
            }
            run.push(byte);
        } else {
            emit_owned_run(&mut run, run_start, base_offset, options.minimum, emit)?;
        }
        cursor += width;
    }

    emit_owned_run(&mut run, run_start, base_offset, options.minimum, emit)
}

fn scan_utf8(
    bytes: &[u8],
    base_offset: u64,
    options: &Options,
    emit: &mut impl FnMut(u64, &[u8]) -> Result<(), RunError>,
) -> Result<(), RunError> {
    let mut cursor = 0;

    while cursor < bytes.len() {
        match std::str::from_utf8(&bytes[cursor..]) {
            Ok(valid) => {
                scan_valid_utf8(valid, cursor, base_offset, options, emit)?;
                break;
            }
            Err(error) => {
                let valid_end = cursor + error.valid_up_to();
                if valid_end > cursor {
                    let valid = std::str::from_utf8(&bytes[cursor..valid_end])
                        .expect("valid_up_to always denotes valid UTF-8");
                    scan_valid_utf8(valid, cursor, base_offset, options, emit)?;
                }
                cursor = valid_end + error.error_len().unwrap_or(bytes.len() - valid_end);
            }
        }
    }

    Ok(())
}

fn scan_valid_utf8(
    text: &str,
    text_offset: usize,
    base_offset: u64,
    options: &Options,
    emit: &mut impl FnMut(u64, &[u8]) -> Result<(), RunError>,
) -> Result<(), RunError> {
    let mut run_start = None;
    let mut run_characters = 0;

    for (index, character) in text.char_indices() {
        if is_display_char(character, options.include_all_whitespace) {
            run_start.get_or_insert(index);
            run_characters += 1;
        } else if let Some(start) = run_start.take() {
            if run_characters >= options.minimum {
                emit(
                    base_offset + (text_offset + start) as u64,
                    &text.as_bytes()[start..index],
                )?;
            }
            run_characters = 0;
        }
    }

    if let Some(start) = run_start
        && run_characters >= options.minimum
    {
        emit(
            base_offset + (text_offset + start) as u64,
            &text.as_bytes()[start..],
        )?;
    }

    Ok(())
}

fn scan_utf16le(
    bytes: &[u8],
    base_offset: u64,
    options: &Options,
    emit: &mut impl FnMut(u64, &[u8]) -> Result<(), RunError>,
) -> Result<(), RunError> {
    let mut cursor = 0;
    let mut run_start = 0;
    let mut run_characters = 0;
    let mut run = Vec::new();

    while cursor + 2 <= bytes.len() {
        let first = u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
        let (character, consumed) = if (0xd800..=0xdbff).contains(&first)
            && cursor + 4 <= bytes.len()
        {
            let second = u16::from_le_bytes([bytes[cursor + 2], bytes[cursor + 3]]);
            if (0xdc00..=0xdfff).contains(&second) {
                let scalar =
                    0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00);
                (char::from_u32(scalar), 4)
            } else {
                (None, 2)
            }
        } else if (0xd800..=0xdfff).contains(&first) {
            (None, 2)
        } else {
            (char::from_u32(u32::from(first)), 2)
        };

        if let Some(character) = character
            .filter(|character| is_display_char(*character, options.include_all_whitespace))
        {
            if run.is_empty() {
                run_start = cursor;
            }
            let mut encoded = [0; 4];
            run.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            run_characters += 1;
        } else {
            if run_characters >= options.minimum {
                emit(base_offset + run_start as u64, &run)?;
            }
            run.clear();
            run_characters = 0;
        }
        cursor += consumed;
    }

    if run_characters >= options.minimum {
        emit(base_offset + run_start as u64, &run)?;
    }
    Ok(())
}

fn emit_owned_run(
    run: &mut Vec<u8>,
    start: usize,
    base_offset: u64,
    minimum: usize,
    emit: &mut impl FnMut(u64, &[u8]) -> Result<(), RunError>,
) -> Result<(), RunError> {
    if run.len() >= minimum {
        emit(base_offset + start as u64, run)?;
    }
    run.clear();
    Ok(())
}

fn is_display_byte(byte: u8, eight_bit: bool, include_all_whitespace: bool) -> bool {
    byte == b'\t'
        || (0x20..=0x7e).contains(&byte)
        || (eight_bit && byte >= 0x80)
        || (include_all_whitespace && byte.is_ascii_whitespace())
}

fn is_display_char(character: char, include_all_whitespace: bool) -> bool {
    character == '\t'
        || !character.is_control()
        || (include_all_whitespace && character.is_whitespace())
}

struct Emitter<'a, W> {
    output: &'a mut W,
    label: &'a OsStr,
    options: &'a Options,
}

#[derive(Serialize)]
struct JsonRecord<'a> {
    file: Cow<'a, str>,
    offset: u64,
    encoding: &'static str,
    string: Cow<'a, str>,
}

impl<W: Write> Emitter<'_, W> {
    fn emit(&mut self, offset: u64, value: &[u8]) -> Result<(), RunError> {
        if self.options.json {
            let record = JsonRecord {
                file: self.label.to_string_lossy(),
                offset,
                encoding: self.options.encoding.name(),
                string: json_string(value),
            };
            serde_json::to_writer(&mut self.output, &record)
                .map_err(|error| RunError::Output(io::Error::other(error)))?;
        } else {
            if self.options.print_file_name {
                self.output
                    .write_all(self.label.as_encoded_bytes())
                    .and_then(|()| self.output.write_all(b": "))
                    .map_err(RunError::Output)?;
            }
            if let Some(radix) = self.options.radix {
                match radix {
                    Radix::Octal => write!(self.output, "{offset:7o} "),
                    Radix::Decimal => write!(self.output, "{offset:7} "),
                    Radix::Hexadecimal => write!(self.output, "{offset:7x} "),
                }
                .map_err(RunError::Output)?;
            }
            self.output.write_all(value).map_err(RunError::Output)?;
        }

        self.output
            .write_all(&self.options.separator)
            .map_err(RunError::Output)
    }
}

fn json_string(bytes: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(bytes.iter().map(|byte| char::from(*byte)).collect()),
    }
}
