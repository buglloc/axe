use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{ArgAction, Parser};
use file_format::FileFormat;

use super::errno::strerror;

const TEXT_SAMPLE_LIMIT: u64 = 64 * 1024;

#[derive(Parser)]
#[command(
    name = "file",
    version,
    about = "Determine file type",
    disable_help_flag = true,
    args_override_self = true
)]
struct Args {
    /// Omit the file name from output.
    #[arg(short = 'b', long)]
    brief: bool,

    /// Print media type and character set.
    #[arg(short = 'i', long)]
    mime: bool,

    /// Print only the media type.
    #[arg(long)]
    mime_type: bool,

    /// Print only the character set.
    #[arg(long)]
    mime_encoding: bool,

    /// Follow symbolic links.
    #[arg(short = 'L', long, overrides_with = "no_dereference")]
    dereference: bool,

    /// Inspect symbolic links instead of their targets.
    #[arg(short = 'h', long, overrides_with = "dereference")]
    no_dereference: bool,

    /// Return an error when a file cannot be inspected.
    #[arg(short = 'E', long = "error")]
    error: bool,

    /// Do not pad file names to equal width.
    #[arg(short = 'N', long)]
    no_pad: bool,

    #[arg(long = "help", action = ArgAction::Help)]
    _help: Option<bool>,

    #[arg(required = true, action = ArgAction::Append)]
    files: Vec<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MimeMode {
    Description,
    Type,
    Encoding,
    Full,
}

impl MimeMode {
    fn from_args(args: &Args) -> Self {
        match (args.mime || args.mime_type, args.mime || args.mime_encoding) {
            (false, false) => Self::Description,
            (true, false) => Self::Type,
            (false, true) => Self::Encoding,
            (true, true) => Self::Full,
        }
    }

    fn render(
        self,
        description: impl FnOnce() -> String,
        media_type: &str,
        charset: &str,
    ) -> String {
        match self {
            Self::Description => description(),
            Self::Type => media_type.into(),
            Self::Encoding => charset.into(),
            Self::Full => format!("{media_type}; charset={charset}"),
        }
    }
}

pub fn file(args: Vec<OsString>) -> i32 {
    let args = match Args::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            let code = if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                0
            } else {
                2
            };
            let _ = error.print();
            return code;
        }
    };

    match run(&args, &mut io::BufWriter::new(io::stdout().lock())) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("file: {error}");
            1
        }
    }
}

fn run(args: &Args, output: &mut impl Write) -> io::Result<i32> {
    let _locale = FilenameLocale::new();
    let mode = MimeMode::from_args(args);
    let names = args
        .files
        .iter()
        .map(|path| {
            if args.brief {
                return (String::new(), 0);
            }
            let display_path = if path == Path::new("-") {
                OsStr::new("/dev/stdin")
            } else {
                path.as_os_str()
            };
            escaped_name(display_path)
        })
        .collect::<Vec<_>>();
    let width = names.iter().map(|name| name.1).max().unwrap_or(0);
    let mut failed = false;
    for (path, (name, name_width)) in args.files.iter().zip(names) {
        let description = match describe(
            path,
            args.dereference && !args.no_dereference,
            mode,
            args.error,
        ) {
            Ok(kind) => kind.render(mode),
            Err(failure) => {
                failed |= args.error;
                failure.render(path, args.error)
            }
        };
        if !args.brief {
            let pad = if args.no_pad { 0 } else { width - name_width };
            write!(output, "{name}:{:pad$} ", "")?;
        }
        writeln!(output, "{description}")?;
    }
    output.flush()?;
    Ok(i32::from(failed))
}

enum Kind {
    Empty {
        stdin: bool,
    },
    Directory,
    Symlink {
        target: PathBuf,
        broken: bool,
    },
    #[cfg(unix)]
    Device {
        metadata: fs::Metadata,
        block: bool,
    },
    #[cfg(unix)]
    Socket,
    #[cfg(unix)]
    Fifo,
    Content {
        format: FileFormat,
        sample: Vec<u8>,
        complete: bool,
    },
}

impl Kind {
    fn render(&self, mode: MimeMode) -> String {
        match self {
            Self::Empty { stdin } => mode.render(
                || "empty".into(),
                if *stdin {
                    "application/x-empty"
                } else {
                    "inode/x-empty"
                },
                "binary",
            ),
            Self::Directory => mode.render(|| "directory".into(), "inode/directory", "binary"),
            Self::Symlink { target, broken } => {
                if *broken && mode != MimeMode::Description {
                    return if mode == MimeMode::Encoding {
                        String::new()
                    } else {
                        "inode/symlink".into()
                    };
                }
                mode.render(
                    || {
                        format!(
                            "{}symbolic link to {}",
                            if *broken { "broken " } else { "" },
                            escaped_name(target.as_os_str()).0
                        )
                    },
                    "inode/symlink",
                    "binary",
                )
            }
            #[cfg(unix)]
            Self::Device { metadata, block } => mode.render(
                || {
                    device_text(
                        metadata,
                        if *block {
                            "block special"
                        } else {
                            "character special"
                        },
                    )
                },
                if *block {
                    "inode/blockdevice"
                } else {
                    "inode/chardevice"
                },
                "binary",
            ),
            #[cfg(unix)]
            Self::Socket => mode.render(|| "socket".into(), "inode/socket", "binary"),
            #[cfg(unix)]
            Self::Fifo => mode.render(|| "fifo (named pipe)".into(), "inode/fifo", "binary"),
            Self::Content {
                format,
                sample,
                complete,
            } => {
                let charset = content_charset(sample, *complete);
                let plain_text = *format == FileFormat::PlainText
                    || (*format == FileFormat::ArbitraryBinaryData && charset != "binary");
                let json = plain_text
                    && *complete
                    && serde_json::from_slice::<serde_json::Value>(sample).is_ok();
                let media_type = if json {
                    "application/json"
                } else if let Some(media_type) = elf_media_type(sample) {
                    media_type
                } else if plain_text {
                    "text/plain"
                } else {
                    format.media_type()
                };
                mode.render(
                    || {
                        if json {
                            "JSON text data".into()
                        } else if plain_text {
                            match charset {
                                "us-ascii" => "ASCII text",
                                "utf-8" => "Unicode text, UTF-8 text",
                                "utf-16le" => "Unicode text, UTF-16, little-endian text",
                                "utf-16be" => "Unicode text, UTF-16, big-endian text",
                                "iso-8859-1" => "ISO-8859 text",
                                _ => "data",
                            }
                            .into()
                        } else if *format == FileFormat::ArbitraryBinaryData {
                            "data".into()
                        } else {
                            format.name().into()
                        }
                    },
                    media_type,
                    charset,
                )
            }
        }
    }
}

enum Failure {
    Stat(io::Error),
    Open(io::Error),
    BrokenLink { target: PathBuf, error: io::Error },
}

impl Failure {
    fn render(&self, path: &Path, hard: bool) -> String {
        let (message, error) = match self {
            Self::Stat(error) if hard => (
                format!("cannot stat `{}'", escaped_name(path.as_os_str()).0),
                error,
            ),
            Self::Stat(error) | Self::Open(error) => (
                format!("cannot open `{}'", escaped_name(path.as_os_str()).0),
                error,
            ),
            Self::BrokenLink { target, error } => (
                format!(
                    "broken symbolic link to {}",
                    escaped_name(target.as_os_str()).0
                ),
                error,
            ),
        };
        format!(
            "{}{message} ({})",
            if hard { "ERROR: " } else { "" },
            strerror(error)
        )
    }
}

fn describe(path: &Path, dereference: bool, mode: MimeMode, hard: bool) -> Result<Kind, Failure> {
    if path == Path::new("-") {
        let mut sample = Vec::new();
        io::stdin()
            .read_to_end(&mut sample)
            .map_err(Failure::Open)?;
        if sample.is_empty() {
            return Ok(Kind::Empty { stdin: true });
        }
        let format = FileFormat::from_reader(Cursor::new(&sample)).map_err(Failure::Open)?;
        return Ok(Kind::Content {
            format,
            sample,
            complete: true,
        });
    }

    let metadata = if dereference {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    }
    .map_err(Failure::Stat)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        let target = fs::read_link(path).map_err(Failure::Open)?;
        let error = fs::metadata(path).err();
        let broken = error.is_some();
        if hard
            && mode == MimeMode::Description
            && let Some(error) = error
        {
            return Err(Failure::BrokenLink { target, error });
        }
        return Ok(Kind::Symlink { target, broken });
    }
    if file_type.is_dir() {
        return Ok(Kind::Directory);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if file_type.is_socket() {
            return Ok(Kind::Socket);
        }
        if file_type.is_fifo() {
            return Ok(Kind::Fifo);
        }
        if file_type.is_block_device() || file_type.is_char_device() {
            return Ok(Kind::Device {
                metadata,
                block: file_type.is_block_device(),
            });
        }
    }
    if metadata.len() == 0 {
        return Ok(Kind::Empty { stdin: false });
    }

    let format = FileFormat::from_file(path).map_err(Failure::Open)?;
    let mut sample = Vec::with_capacity(TEXT_SAMPLE_LIMIT as usize + 1);
    fs::File::open(path)
        .map_err(Failure::Open)?
        .take(TEXT_SAMPLE_LIMIT + 1)
        .read_to_end(&mut sample)
        .map_err(Failure::Open)?;
    let complete = sample.len() <= TEXT_SAMPLE_LIMIT as usize;
    sample.truncate(TEXT_SAMPLE_LIMIT as usize);
    Ok(Kind::Content {
        format,
        sample,
        complete,
    })
}

fn content_charset(sample: &[u8], complete: bool) -> &'static str {
    if sample.starts_with(b"\xff\xfe") || sample.starts_with(b"\xfe\xff") {
        let little_endian = sample.starts_with(b"\xff\xfe");
        let mut bytes = &sample[2..];
        if complete && !bytes.len().is_multiple_of(2) {
            return "binary";
        }
        let decode_unit = |unit: &[u8; 2]| {
            if little_endian {
                u16::from_le_bytes([unit[0], unit[1]])
            } else {
                u16::from_be_bytes([unit[0], unit[1]])
            }
        };
        bytes = &bytes[..bytes.len() / 2 * 2];
        if !complete
            && bytes
                .last_chunk::<2>()
                .is_some_and(|unit| (0xd800..=0xdbff).contains(&decode_unit(unit)))
        {
            bytes = &bytes[..bytes.len() - 2];
        }
        let units = bytes.as_chunks::<2>().0.iter().map(decode_unit);
        if char::decode_utf16(units).all(|character| {
            character.is_ok_and(|character| {
                !character.is_control() || matches!(character, '\t' | '\n' | '\r' | '\u{c}')
            })
        }) {
            return if little_endian {
                "utf-16le"
            } else {
                "utf-16be"
            };
        }
        return "binary";
    }
    let text_byte = |byte: u8| {
        byte >= 0x20 && byte != 0x7f
            || matches!(
                byte,
                b'\x07' | b'\x08' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r' | b'\x1b'
            )
    };
    if !sample.iter().copied().all(text_byte) {
        return "binary";
    }
    if sample.is_ascii() {
        return "us-ascii";
    }
    match std::str::from_utf8(sample) {
        Ok(_) => return "utf-8",
        Err(error) if !complete && error.error_len().is_none() => return "utf-8",
        Err(_) => {}
    }
    if sample.iter().all(|byte| *byte < 0x80 || *byte >= 0xa0) {
        "iso-8859-1"
    } else {
        "binary"
    }
}

fn elf_media_type(sample: &[u8]) -> Option<&'static str> {
    if !sample.starts_with(b"\x7fELF") {
        return None;
    }
    let little_endian = match sample.get(5)? {
        1 => true,
        2 => false,
        _ => return None,
    };
    let integer = |offset: usize, width: usize| -> Option<u64> {
        let bytes = sample.get(offset..offset.checked_add(width)?)?;
        Some(if little_endian {
            bytes
                .iter()
                .rev()
                .fold(0, |value, byte| value << 8 | u64::from(*byte))
        } else {
            bytes
                .iter()
                .fold(0, |value, byte| value << 8 | u64::from(*byte))
        })
    };
    Some(match integer(16, 2)? {
        1 => "application/x-object",
        2 => "application/x-executable",
        3 => {
            let (offset, entry_size, count, wide) = match sample.get(4)? {
                1 => (integer(28, 4)?, integer(42, 2)?, integer(44, 2)?, false),
                2 => (integer(32, 8)?, integer(54, 2)?, integer(56, 2)?, true),
                _ => return None,
            };
            let mut pie = false;
            for index in 0..count {
                let entry =
                    usize::try_from(offset.checked_add(index.checked_mul(entry_size)?)?).ok()?;
                match integer(entry, 4) {
                    Some(3) => pie = true, // PT_INTERP
                    Some(2) => {
                        let (file_offset, file_size, word) = if wide {
                            (integer(entry + 8, 8)?, integer(entry + 32, 8)?, 8)
                        } else {
                            (integer(entry + 4, 4)?, integer(entry + 16, 4)?, 4)
                        };
                        let start = usize::try_from(file_offset).ok()?;
                        let size = usize::try_from(file_size).ok()?;
                        let end = start.checked_add(size)?.min(sample.len());
                        let mut cursor = start;
                        while cursor.checked_add(word * 2).is_some_and(|next| next <= end) {
                            let tag = integer(cursor, word)?;
                            if tag == 0 {
                                break;
                            }
                            if tag == 0x6fff_fffb
                                && integer(cursor + word, word)? & 0x0800_0000 != 0
                            {
                                pie = true; // DT_FLAGS_1 / DF_1_PIE
                            }
                            cursor += word * 2;
                        }
                    }
                    _ => {}
                }
            }
            if pie {
                "application/x-pie-executable"
            } else {
                "application/x-sharedlib"
            }
        }
        4 => "application/x-coredump",
        _ => return None,
    })
}

#[cfg(unix)]
fn device_text(metadata: &fs::Metadata, kind: &str) -> String {
    use std::os::unix::fs::MetadataExt;
    let device = metadata.rdev() as libc::dev_t;
    format!("{kind} ({}/{})", libc::major(device), libc::minor(device))
}

fn escaped_name(name: &OsStr) -> (String, usize) {
    use std::fmt::Write as _;
    let mut text = String::new();
    let mut width = 0;
    for chunk in name.as_encoded_bytes().utf8_chunks() {
        for character in chunk.valid().chars() {
            if let Some(columns) = character_width(character) {
                text.push(character);
                width += columns;
            } else {
                for byte in character.encode_utf8(&mut [0; 4]).bytes() {
                    let _ = write!(text, "\\{byte:03o}");
                    width += 4;
                }
            }
        }
        for byte in chunk.invalid() {
            let _ = write!(text, "\\{byte:03o}");
            width += 4;
        }
    }
    (text, width)
}

#[cfg(unix)]
fn character_width(character: char) -> Option<usize> {
    unsafe extern "C" {
        fn wcwidth(character: libc::wchar_t) -> libc::c_int;
    }
    // SAFETY: wcwidth accepts any wchar_t value and only reads the thread locale.
    let width = unsafe { wcwidth(character as libc::wchar_t) };
    (width >= 0).then_some(width.max(1) as usize)
}

#[cfg(not(unix))]
fn character_width(character: char) -> Option<usize> {
    (!character.is_control()).then_some(1)
}

#[cfg(unix)]
struct FilenameLocale {
    locale: libc::locale_t,
    previous: libc::locale_t,
}

#[cfg(unix)]
impl FilenameLocale {
    fn new() -> Self {
        // SAFETY: the empty NUL-terminated locale name selects the environment;
        // the null base requests an independently owned locale object.
        let locale =
            unsafe { libc::newlocale(libc::LC_CTYPE_MASK, c"".as_ptr(), std::ptr::null_mut()) };
        let previous = if locale.is_null() {
            std::ptr::null_mut()
        } else {
            // SAFETY: locale was returned by newlocale and lives until Drop.
            unsafe { libc::uselocale(locale) }
        };
        Self { locale, previous }
    }
}

#[cfg(unix)]
impl Drop for FilenameLocale {
    fn drop(&mut self) {
        if !self.locale.is_null() {
            // SAFETY: restore the calling thread's live previous locale before
            // freeing the locale object exclusively owned by this guard.
            unsafe {
                libc::uselocale(self.previous);
                libc::freelocale(self.locale);
            }
        }
    }
}

#[cfg(not(unix))]
struct FilenameLocale;

#[cfg(not(unix))]
impl FilenameLocale {
    fn new() -> Self {
        Self
    }
}
