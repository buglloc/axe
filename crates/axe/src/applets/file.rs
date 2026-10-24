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
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
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
                let plain_text = *format == FileFormat::PlainText;
                let json = plain_text
                    && *complete
                    && serde_json::from_slice::<serde_json::Value>(sample).is_ok();
                let media_type = if json {
                    "application/json"
                } else {
                    format.media_type()
                };
                let charset = if !plain_text {
                    "binary"
                } else if sample.is_ascii() {
                    "us-ascii"
                } else {
                    "utf-8"
                };
                mode.render(
                    || {
                        if json {
                            "JSON text data".into()
                        } else if *format == FileFormat::ArbitraryBinaryData {
                            "data".into()
                        } else if plain_text && sample.is_ascii() {
                            "ASCII text".into()
                        } else if plain_text {
                            "Unicode text, UTF-8 text".into()
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
    let (sample, complete) = if format == FileFormat::PlainText {
        let mut sample = Vec::with_capacity(TEXT_SAMPLE_LIMIT as usize + 1);
        fs::File::open(path)
            .map_err(Failure::Open)?
            .take(TEXT_SAMPLE_LIMIT + 1)
            .read_to_end(&mut sample)
            .map_err(Failure::Open)?;
        let complete = sample.len() <= TEXT_SAMPLE_LIMIT as usize;
        sample.truncate(TEXT_SAMPLE_LIMIT as usize);
        (sample, complete)
    } else {
        (Vec::new(), false)
    };
    Ok(Kind::Content {
        format,
        sample,
        complete,
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
