use std::borrow::Cow;
#[cfg(unix)]
use std::cell::UnsafeCell;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;

use bzip2::bufread::BzDecoder;
use bzip2::read::BzEncoder;
use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;
use lzma_rust2::{XzOptions, XzReader, XzWriter, XzWriterMt};

#[derive(Clone, Copy)]
enum Format {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
}

impl Format {
    fn default_name(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Bzip2 => "bzip2",
            Self::Xz => "xz",
            Self::Zstd => "zstd",
        }
    }

    fn usage(self) -> &'static str {
        match self {
            Self::Gzip => "[-cdfkltqvr] [-1..-9] [--best|--fast] [-S SUF] [FILE ...]",
            Self::Bzip2 => "[-cdfktqv] [-1..-9] [FILE ...]",
            Self::Xz => "[-cdfkltqvr] [-T THREADS] [-S SUF] [-1..-9] [FILE ...]",
            Self::Zstd => {
                "[-cdfktqv] [-o FILE] [-T THREADS] [-1..-19] [--ultra -20..-22] [--fast[=N]] [--rm] [FILE ...]"
            }
        }
    }
}

pub fn gzip(args: Vec<OsString>) -> i32 {
    entry(args, Format::Gzip)
}

pub fn bzip2(args: Vec<OsString>) -> i32 {
    entry(args, Format::Bzip2)
}

pub fn xz(args: Vec<OsString>) -> i32 {
    entry(args, Format::Xz)
}

pub fn zstd(args: Vec<OsString>) -> i32 {
    entry(args, Format::Zstd)
}

fn entry(args: Vec<OsString>, format: Format) -> i32 {
    let name = args
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .and_then(OsStr::to_str)
        .unwrap_or(format.default_name())
        .to_owned();

    match Options::parse(name.clone(), format, args.into_iter().skip(1)).and_then(Options::run) {
        Ok(code) => code,
        // --help and --version finish parsing with an empty Interrupted error.
        Err(error)
            if error.kind() == io::ErrorKind::Interrupted && error.to_string().is_empty() =>
        {
            0
        }
        Err(error) => {
            eprintln!("{name}: {error}");
            1
        }
    }
}

struct Options {
    program: String,
    format: Format,
    decompress: bool,
    stdout: bool,
    keep: bool,
    force: bool,
    test: bool,
    list: bool,
    quiet: bool,
    verbose: bool,
    recursive: bool,
    level: i32,
    threads: u32,
    files: Vec<OsString>,
    output: Option<PathBuf>,
    suffix: Option<OsString>,
    ultra: bool,
}

impl Options {
    fn parse(
        program: String,
        format: Format,
        args: impl Iterator<Item = OsString>,
    ) -> io::Result<Self> {
        let decompress = matches!(
            program.as_str(),
            "gunzip" | "zcat" | "gzcat" | "bunzip2" | "bzcat" | "unxz" | "xzcat"
        );
        let stdout = matches!(program.as_str(), "zcat" | "gzcat" | "bzcat" | "xzcat");
        let mut options = Self {
            program,
            format,
            decompress,
            stdout,
            keep: matches!(format, Format::Zstd),
            force: false,
            test: false,
            list: false,
            quiet: false,
            verbose: false,
            recursive: false,
            level: match format {
                Format::Bzip2 => 9,
                Format::Zstd => 3,
                Format::Gzip | Format::Xz => 6,
            },
            threads: 1,
            files: Vec::new(),
            output: None,
            suffix: None,
            ultra: false,
        };

        let mut positional = false;
        let mut args = args;
        while let Some(argument) = args.next() {
            let Some(text) = argument.to_str() else {
                options.files.push(argument);
                continue;
            };
            if positional {
                options.files.push(argument);
            } else if text == "--" {
                positional = true;
            } else if text == "--help" {
                println!("Usage: {} {}", options.program, format.usage());
                return Err(done());
            } else if text == "--version" {
                println!("{} (axe) {}", options.program, env!("CARGO_PKG_VERSION"));
                return Err(done());
            } else if options.long_option(text, &mut args)? {
            } else if text.starts_with('-') && text != "-" {
                let mut flags = text[1..].chars();
                while let Some(flag) = flags.next() {
                    if flag == 'S' {
                        if !matches!(format, Format::Gzip | Format::Xz) {
                            return Err(invalid("unsupported option -S"));
                        }
                        let suffix = if flags.as_str().is_empty() {
                            args.next().ok_or_else(|| invalid("-S requires a suffix"))?
                        } else {
                            OsString::from(flags.as_str())
                        };
                        options.suffix = Some(suffix);
                        break;
                    }
                    options.short_option(flag)?;
                }
            } else {
                options.files.push(argument);
            }
        }

        if matches!(format, Format::Zstd) {
            if options.level > 19 && !options.ultra {
                return Err(invalid("levels above 19 require --ultra"));
            }
            if options.output.is_some() && options.files.len() > 1 {
                return Err(invalid("-o requires a single input"));
            }
            if options.output.as_deref() == Some(Path::new("-")) {
                options.stdout = true;
                options.output = None;
            }
        }

        if let Some(suffix) = options.suffix.as_deref() {
            let bytes = suffix.as_encoded_bytes();
            if bytes.is_empty() {
                return Err(invalid("invalid suffix ''"));
            }
            if bytes.contains(&b'/') {
                return Err(invalid("suffix contains '/'"));
            }
        }
        if options.recursive && !matches!(format, Format::Gzip | Format::Xz) {
            return Err(invalid("unsupported option --recursive"));
        }

        options.decompress |= options.test;
        Ok(options)
    }

    /// Returns false for words that must be parsed as short options or operands.
    fn long_option(
        &mut self,
        text: &str,
        args: &mut impl Iterator<Item = OsString>,
    ) -> io::Result<bool> {
        match text {
            "--decompress" | "--uncompress" => self.decompress = true,
            "--compress" if !matches!(self.format, Format::Gzip) => self.decompress = false,
            "--stdout" | "--to-stdout" => self.stdout = true,
            "--keep" => self.keep = true,
            "--force" => self.force = true,
            "--test" => {
                self.test = true;
                self.decompress = true;
            }
            "--quiet" => self.quiet = true,
            "--verbose" => self.verbose = true,
            _ => return self.codec_option(text, args),
        }
        Ok(true)
    }

    fn codec_option(
        &mut self,
        text: &str,
        args: &mut impl Iterator<Item = OsString>,
    ) -> io::Result<bool> {
        if text == "--recursive" {
            self.recursive = true;
            return Ok(true);
        }
        if text == "--suffix" {
            self.suffix = Some(
                args.next()
                    .ok_or_else(|| invalid("--suffix requires a suffix"))?,
            );
            return Ok(true);
        }
        if let Some(value) = text.strip_prefix("--suffix=") {
            self.suffix = Some(OsString::from(value));
            return Ok(true);
        }
        if matches!(self.format, Format::Zstd) {
            match text {
                "--rm" => self.keep = false,
                "--ultra" => self.ultra = true,
                "--fast" => self.level = -1,
                "-o" => {
                    self.output = Some(
                        args.next()
                            .ok_or_else(|| invalid("-o requires an output file"))?
                            .into(),
                    );
                }
                _ => {
                    if let Some(value) = text.strip_prefix("--fast=") {
                        self.level = -value
                            .parse::<i32>()
                            .ok()
                            .filter(|value| (1..=131_072).contains(value))
                            .ok_or_else(|| invalid("invalid fast compression level"))?;
                    } else if let Some(value) = text.strip_prefix('-').filter(|value| {
                        !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
                    }) {
                        self.level = value
                            .parse::<i32>()
                            .ok()
                            .filter(|value| (1..=22).contains(value))
                            .ok_or_else(|| invalid("invalid zstd compression level"))?;
                    } else {
                        return self.thread_option(text, args);
                    }
                }
            }
            return Ok(true);
        }
        match self.format {
            Format::Gzip => match text {
                "--best" => self.level = 9,
                "--fast" => self.level = 1,
                "--no-name" => {}
                "--list" => {
                    self.list = true;
                    self.decompress = true;
                }
                _ => return Ok(false),
            },
            Format::Bzip2 | Format::Xz => {
                if text == "--list" || text == "-l" {
                    if !matches!(self.format, Format::Xz) {
                        return Err(invalid("--list is only supported by xz"));
                    }
                    self.list = true;
                } else if matches!(self.format, Format::Xz) {
                    return self.thread_option(text, args);
                } else {
                    return Ok(false);
                }
            }
            Format::Zstd => unreachable!("zstd options are handled above"),
        }

        Ok(true)
    }

    fn thread_option(
        &mut self,
        text: &str,
        args: &mut impl Iterator<Item = OsString>,
    ) -> io::Result<bool> {
        if text == "-T" || text == "--threads" {
            self.threads = parse_threads(
                &args
                    .next()
                    .ok_or_else(|| invalid("-T requires a thread count"))?,
            )?;
        } else if let Some(value) = text.strip_prefix("--threads=") {
            self.threads = parse_threads(OsStr::new(value))?;
        } else if let Some(value) = text.strip_prefix("-T").filter(|value| !value.is_empty()) {
            self.threads = parse_threads(OsStr::new(value))?;
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn short_option(&mut self, flag: char) -> io::Result<()> {
        match (self.format, flag) {
            (_, 'c') => self.stdout = true,
            (_, 'd') => self.decompress = true,
            (_, 'f') => self.force = true,
            (_, 'k') => self.keep = true,
            (_, 't') => {
                self.test = true;
                self.decompress = true;
            }
            (Format::Gzip | Format::Xz, 'r') => self.recursive = true,
            (Format::Gzip, 'n') => {}
            (Format::Gzip, 'q') => self.quiet = true,
            (Format::Gzip, 'v') => self.verbose = true,
            (Format::Gzip, 'l') => {
                self.list = true;
                self.decompress = true;
            }
            (Format::Bzip2 | Format::Xz | Format::Zstd, 'z') => self.decompress = false,
            (Format::Bzip2 | Format::Xz, 'q') => self.quiet = true,
            (Format::Bzip2 | Format::Xz, 'v') => self.verbose = true,
            (Format::Zstd, 'q') => self.quiet = true,
            (Format::Zstd, 'v') => self.verbose = true,
            (Format::Xz, 'l') => self.list = true,
            (Format::Gzip | Format::Bzip2 | Format::Xz, '1'..='9') => {
                self.level = flag.to_digit(10).expect("digit") as i32;
            }
            _ => return Err(invalid(format!("unsupported option -{flag}"))),
        }

        Ok(())
    }

    fn run(self) -> io::Result<i32> {
        if self.list {
            return self.list_contents();
        }
        if self.files.is_empty() {
            return self.finish_operand("(stdin)", self.filter_stdin());
        }

        let files = self.input_files()?;
        let mut status = 0;
        for file in &files {
            let result = if file == OsStr::new("-") {
                self.filter_stdin()
            } else {
                self.process_file(Path::new(file))
            };
            let code = self.finish_operand(&file.to_string_lossy(), result)?;
            status = self.merge_status(status, code);
        }
        Ok(status)
    }

    fn input_files(&self) -> io::Result<Vec<OsString>> {
        let mut files = Vec::new();
        for file in &self.files {
            collect_input_files(Path::new(file), self.recursive, &mut files)?;
        }
        Ok(files)
    }

    fn merge_status(&self, previous: i32, code: i32) -> i32 {
        match self.format {
            Format::Gzip | Format::Xz if previous == 1 || code == 1 => 1,
            _ => previous.max(code),
        }
    }

    fn finish_operand(&self, name: &str, result: io::Result<i32>) -> io::Result<i32> {
        match result {
            Ok(code) => Ok(code),
            Err(error) => {
                if error.kind() == io::ErrorKind::BrokenPipe {
                    #[cfg(unix)]
                    {
                        // SAFETY: SIGPIPE is deliberately sent only to this
                        // process; its disposition was restored before the
                        // applet was invoked.
                        unsafe {
                            libc::raise(libc::SIGPIPE);
                        }
                    }
                }
                let message = error.to_string();
                let message = if error.raw_os_error().is_some() {
                    message
                        .rsplit_once(" (os error ")
                        .map_or(message.as_str(), |(message, _)| message)
                } else {
                    message.as_str()
                };
                eprintln!("{}: {name}: {message}", self.program);
                Ok(match (&self.format, error.kind()) {
                    (Format::Bzip2, io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof)
                    | (Format::Gzip, io::ErrorKind::AlreadyExists) => 2,
                    _ => 1,
                })
            }
        }
    }

    fn skip(&self, code: i32, message: impl std::fmt::Display) -> i32 {
        if !self.quiet {
            eprintln!("{}: {message}", self.program);
        }
        code
    }

    fn process_file(&self, path: &Path) -> io::Result<i32> {
        let link_metadata = fs::symlink_metadata(path)?;
        let to_file = !self.stdout && !self.test;
        let follow_links =
            !to_file || self.force || (matches!(self.format, Format::Xz) && self.keep);
        if link_metadata.file_type().is_symlink() && !follow_links {
            return Ok(match self.format {
                Format::Gzip => self.skip(
                    1,
                    format_args!("{}: Too many levels of symbolic links", path.display()),
                ),
                Format::Bzip2 => self.skip(
                    1,
                    format_args!("Input file {} is not a normal file.", path.display()),
                ),
                Format::Xz => self.skip(
                    2,
                    format_args!("{}: Is a symbolic link, skipping", path.display()),
                ),
                Format::Zstd => {
                    if !self.quiet {
                        eprintln!("Warning : {} is a symbolic link, ignoring ", path.display());
                    }
                    1
                }
            });
        }
        let mut open = OpenOptions::new();
        open.read(true);
        #[cfg(unix)]
        if to_file {
            use std::os::unix::fs::OpenOptionsExt;
            open.custom_flags(libc::O_NONBLOCK | if follow_links { 0 } else { libc::O_NOFOLLOW });
        }
        let input = open.open(path)?;
        let metadata = input.metadata()?;
        if metadata.is_dir() || (to_file && !metadata.is_file()) {
            return Ok(self.skip(
                if matches!(self.format, Format::Gzip | Format::Xz) {
                    2
                } else {
                    1
                },
                format_args!("{}: Not a regular file, skipping", path.display()),
            ));
        }
        #[cfg(unix)]
        if to_file {
            use std::os::unix::fs::MetadataExt;
            let protect_links = !self.force
                && !matches!(self.format, Format::Zstd)
                && !(matches!(self.format, Format::Xz) && self.keep);
            if protect_links && metadata.nlink() > 1 {
                return Ok(match self.format {
                    Format::Gzip => self.skip(
                        2,
                        format_args!(
                            "{} has {} other link{} -- file ignored",
                            path.display(),
                            metadata.nlink() - 1,
                            if metadata.nlink() == 2 { "" } else { "s" }
                        ),
                    ),
                    Format::Bzip2 => self.skip(
                        1,
                        format_args!(
                            "Input file {} has {} other link{}.",
                            path.display(),
                            metadata.nlink() - 1,
                            if metadata.nlink() == 2 { "" } else { "s" }
                        ),
                    ),
                    Format::Xz => self.skip(
                        2,
                        format_args!(
                            "{}: Input file has more than one hard link, skipping",
                            path.display()
                        ),
                    ),
                    Format::Zstd => unreachable!("zstd accepts multiply-linked inputs"),
                });
            }
            let special = metadata.mode() & 0o7000;
            if matches!(self.format, Format::Gzip)
                && (special & 0o6000 != 0 || (special & 0o1000 != 0 && !self.force))
            {
                let reason = if special & 0o4000 != 0 {
                    "is set-user-ID on execution - ignored"
                } else if special & 0o2000 != 0 {
                    "is set-group-ID on execution - ignored"
                } else {
                    "has the sticky bit set - file ignored"
                };
                return Ok(self.skip(2, format_args!("{} {reason}", path.display())));
            }
            if matches!(self.format, Format::Xz) && protect_links && special != 0 {
                let reason = if special & 0o6000 != 0 {
                    "File has setuid or setgid bit set, skipping"
                } else {
                    "File has sticky bit set, skipping"
                };
                return Ok(self.skip(2, format_args!("{}: {reason}", path.display())));
            }
        }
        if !self.decompress
            && !self.test
            && (to_file || matches!(self.format, Format::Bzip2))
            && let Some(suffix) = compressed_suffix(path, self.format, self.suffix.as_deref())
            && !(matches!(self.format, Format::Gzip) && self.force)
        {
            let suffix = suffix.to_string_lossy();
            return Ok(match self.format {
                Format::Gzip => self.skip(
                    0,
                    format_args!(
                        "{} already has {suffix} suffix -- unchanged",
                        path.display()
                    ),
                ),
                Format::Bzip2 => self.skip(
                    1,
                    format_args!("Input file {} already has {suffix} suffix.", path.display()),
                ),
                Format::Xz => self.skip(
                    2,
                    format_args!(
                        "{}: File already has '{suffix}' suffix, skipping",
                        path.display()
                    ),
                ),
                Format::Zstd => unreachable!("zstd accepts repeated compression"),
            });
        }
        let destination = if to_file {
            Some(match &self.output {
                Some(output) => Cow::Borrowed(output.as_path()),
                None => {
                    match output_path(path, self.decompress, self.format, self.suffix.as_deref()) {
                        Ok(output) => Cow::Owned(output),
                        Err(error) => {
                            return Ok(self.skip(
                                if matches!(self.format, Format::Gzip | Format::Xz) {
                                    2
                                } else {
                                    1
                                },
                                error,
                            ));
                        }
                    }
                }
            })
        } else {
            None
        };
        if let Some(destination) = &destination {
            if matches!(self.format, Format::Zstd) && same_file(&metadata, destination)? {
                return Ok(self.skip(
                    1,
                    "Refusing to open an output file which will overwrite the input file",
                ));
            }
            if !self.force && fs::symlink_metadata(destination).is_ok() {
                return Ok(match self.format {
                    Format::Gzip => self.skip(
                        2,
                        format_args!("{} already exists;\tnot overwritten", destination.display()),
                    ),
                    Format::Bzip2 => self.skip(
                        1,
                        format_args!("Output file {} already exists.", destination.display()),
                    ),
                    Format::Xz => {
                        self.skip(1, format_args!("{}: File exists", destination.display()))
                    }
                    Format::Zstd => self.skip(
                        1,
                        format_args!("{} already exists; not overwritten", destination.display()),
                    ),
                });
            }
            if self.decompress
                && matches!(self.format, Format::Bzip2)
                && compressed_suffix(path, self.format, self.suffix.as_deref()).is_none()
                && self.output.is_none()
                && !self.quiet
            {
                eprintln!(
                    "{}: Can't guess original name for {} -- using {}",
                    self.program,
                    path.display(),
                    destination.display()
                );
            }
        }
        let transfer = if self.test {
            self.transcode(input, io::sink(), true)?
        } else if let Some(destination) = &destination {
            self.transcode_file(input, destination, Some(&metadata))?
        } else {
            self.transcode(input, io::stdout().lock(), true)?
        };
        if to_file && !self.keep {
            let current = fs::symlink_metadata(path)?;
            if !same_identity(&current, &link_metadata) {
                return Ok(self.skip(
                    if matches!(self.format, Format::Gzip | Format::Xz) {
                        2
                    } else {
                        1
                    },
                    format_args!(
                        "{}: File seems to have been moved, not removing",
                        path.display()
                    ),
                ));
            }
            fs::remove_file(path)?;
        }
        if !self.quiet
            && (self.verbose
                || transfer.trailing_garbage
                || (matches!(self.format, Format::Zstd) && (to_file || self.test)))
        {
            let name = path.display().to_string();
            self.report_transfer(&name, destination.as_deref(), &transfer);
            self.report_trailing_garbage(&name, transfer.trailing_garbage);
        }
        Ok(self.trailing_status(transfer.trailing_garbage))
    }

    fn filter_stdin(&self) -> io::Result<i32> {
        let stdin = io::stdin().lock();
        let transfer = if self.test {
            self.transcode(stdin, io::sink(), true)?
        } else if !self.stdout
            && let Some(path) = &self.output
        {
            self.transcode_file(stdin, path, None)?
        } else {
            self.transcode(stdin, io::stdout().lock(), true)?
        };
        self.report_transfer("(stdin)", self.output.as_deref(), &transfer);
        self.report_trailing_garbage("(stdin)", transfer.trailing_garbage);
        Ok(self.trailing_status(transfer.trailing_garbage))
    }

    fn trailing_status(&self, trailing: bool) -> i32 {
        if trailing && !matches!(self.format, Format::Bzip2) {
            2
        } else {
            0
        }
    }

    fn report_trailing_garbage(&self, name: &str, warning: bool) {
        if warning && !self.quiet {
            let name = if matches!(self.format, Format::Gzip) && name == "(stdin)" {
                "stdin"
            } else {
                name
            };
            eprintln!(
                "\n{}: {name}: {}",
                self.program,
                if matches!(self.format, Format::Bzip2) {
                    "trailing garbage after EOF ignored"
                } else {
                    "decompression OK, trailing garbage ignored"
                }
            );
        }
    }

    fn report_transfer(&self, name: &str, destination: Option<&Path>, transfer: &Transfer) {
        if self.quiet
            || (!self.verbose
                && !(matches!(self.format, Format::Zstd) && (destination.is_some() || self.test)))
        {
            return;
        }
        let name = if matches!(self.format, Format::Zstd) && name == "(stdin)" {
            "/*stdin*\\"
        } else {
            name
        };
        let (plain, compressed) = if self.decompress {
            (transfer.output, transfer.input)
        } else {
            (transfer.input, transfer.output)
        };
        match self.format {
            Format::Gzip if self.test => eprintln!("{name}:\t OK"),
            Format::Gzip => {
                let ratio = if plain == 0 {
                    0.0
                } else {
                    100.0 - compressed.saturating_sub(18) as f64 * 100.0 / plain as f64
                };
                eprint!("{name}:\t{ratio:5.1}%");
                if let Some(destination) = destination {
                    eprint!(
                        " -- {} {}",
                        if self.keep {
                            "created"
                        } else {
                            "replaced with"
                        },
                        destination.display()
                    );
                }
                eprintln!();
            }
            Format::Bzip2 if self.test => eprintln!("  {name}: ok"),
            Format::Bzip2 if self.decompress => eprintln!("  {name}: done"),
            Format::Bzip2 if plain == 0 => eprintln!("  {name}: no data compressed."),
            Format::Bzip2 => eprintln!(
                "  {name}: {:.3}:1, {:.3} bits/byte, {:.2}% saved, {plain} in, {compressed} out.",
                plain as f64 / compressed as f64,
                compressed as f64 * 8.0 / plain as f64,
                100.0 - compressed as f64 * 100.0 / plain as f64
            ),
            Format::Xz => eprintln!(
                "{name}: {} / {} = {}",
                nice_size(compressed),
                nice_size(plain),
                xz_ratio(compressed, plain)
            ),
            Format::Zstd if self.decompress => eprintln!("{name:20}: {} bytes ", transfer.output),
            Format::Zstd => {
                let ratio = if plain == 0 {
                    0.0
                } else {
                    compressed as f64 * 100.0 / plain as f64
                };
                eprint!("{name:20} :{ratio:6.2}%   ({plain:6} B => {compressed:6} B, ");
                if let Some(path) = destination {
                    eprintln!("{}) ", path.display());
                } else {
                    eprintln!("/*stdout*\\ ) ");
                }
            }
        }
    }

    fn transcode_file(
        &self,
        input: impl Read,
        path: &Path,
        metadata: Option<&Metadata>,
    ) -> io::Result<Transfer> {
        let mut output = PendingOutput::new(path, self.force, metadata.is_some())?;
        let result = self
            .transcode(input, &mut output.file, false)
            .and_then(|transfer| {
                output.publish(path, self.force, metadata)?;
                Ok(transfer)
            });
        match result {
            Ok(transfer) => Ok(transfer),
            Err(error) => Err(output.abort(error)),
        }
    }

    fn transcode(
        &self,
        input: impl Read,
        output: impl Write,
        to_stdout: bool,
    ) -> io::Result<Transfer> {
        let mut input = CountRead {
            inner: input,
            bytes: 0,
        };
        let mut output = CountWrite {
            inner: output,
            bytes: 0,
        };
        let trailing_garbage = if self.decompress {
            self.decode(&mut input, &mut output, to_stdout)?
                .trailing_garbage
        } else {
            self.encode(&mut input, &mut output)?;
            false
        };
        Ok(Transfer {
            input: input.bytes,
            output: output.bytes,
            trailing_garbage,
        })
    }

    fn encode(&self, mut input: impl Read, mut output: impl Write) -> io::Result<()> {
        match self.format {
            Format::Gzip => {
                let mut encoder =
                    GzEncoder::new(output, flate2::Compression::new(self.level as u32));
                io::copy(&mut input, &mut encoder)?;
                encoder.try_finish()
            }
            Format::Bzip2 => {
                let mut encoder = BzEncoder::new(input, bzip2::Compression::new(self.level as u32));
                io::copy(&mut encoder, &mut output)?;
                output.flush()
            }
            Format::Xz => {
                let options = XzOptions::with_preset(self.level as u32);
                let mut output = if self.threads == 1 {
                    let mut encoder = XzWriter::new(output, options).map_err(io::Error::other)?;
                    io::copy(&mut input, &mut encoder)?;
                    encoder.finish().map_err(io::Error::other)?
                } else {
                    let workers = if self.threads == 0 {
                        std::thread::available_parallelism()
                            .map_or(1, |count| count.get())
                            .min(256) as u32
                    } else {
                        self.threads.min(256)
                    };
                    let mut options = options;
                    options.set_block_size(NonZeroU64::new(16 * 1024 * 1024));
                    let mut encoder =
                        XzWriterMt::new(output, options, workers).map_err(io::Error::other)?;
                    io::copy(&mut input, &mut encoder)?;
                    encoder.finish().map_err(io::Error::other)?
                };
                output.flush()
            }
            Format::Zstd => {
                let mut encoder = zstd::stream::write::Encoder::new(output, self.level)?;
                encoder.include_checksum(true)?;
                let workers = if self.threads == 0 {
                    std::thread::available_parallelism().map_or(1, |count| count.get() as u32)
                } else {
                    self.threads
                };
                if workers > 1 {
                    encoder.multithread(workers.min(256))?;
                }
                io::copy(&mut input, &mut encoder)?;
                encoder.finish()?.flush()
            }
        }
    }

    fn decode(
        &self,
        input: impl Read,
        mut output: impl Write,
        to_stdout: bool,
    ) -> io::Result<Decoded> {
        let passthrough = self.force
            && to_stdout
            && !self.list
            && (!matches!(self.format, Format::Xz) || self.stdout);
        let decoded = match self.format {
            Format::Gzip => decode_gzip(input, &mut output, passthrough)?,
            Format::Bzip2 => decode_bzip2(input, &mut output, passthrough && !self.test)?,
            Format::Xz | Format::Zstd => {
                let mut input = input;
                let mut magic = [0; 6];
                let length = read_prefix(&mut input, &mut magic)?;
                let prefix = &magic[..length];
                let recognized = match self.format {
                    Format::Xz => matches_magic(prefix, b"\xfd7zXZ\0"),
                    Format::Zstd => zstd_magic(prefix),
                    _ => unreachable!("only xz and zstd need this probe"),
                };
                let mut input = prefix.chain(input);
                if passthrough && !self.test && !recognized {
                    Decoded::clean(io::copy(&mut input, &mut output)?)
                } else if matches!(self.format, Format::Xz) {
                    Decoded::clean(io::copy(&mut XzReader::new(input, true), &mut output)?)
                } else {
                    Decoded::clean(io::copy(
                        &mut zstd::stream::read::Decoder::new(input)?,
                        &mut output,
                    )?)
                }
            }
        };
        output.flush()?;
        Ok(decoded)
    }

    fn list_contents(&self) -> io::Result<i32> {
        let mut header_printed = false;
        let mut totals = ListingTotals::default();
        let mut status = 0;
        if self.files.is_empty() {
            let result = read_stdin().and_then(|bytes| {
                self.print_listing("-", &bytes, &mut header_printed, &mut totals)
            });
            return self.finish_operand("(stdin)", result);
        }
        let files = self.input_files()?;
        for file in &files {
            let result = if file == OsStr::new("-") {
                read_stdin()
            } else {
                fs::read(file)
            }
            .and_then(|bytes| {
                let name = if matches!(self.format, Format::Gzip) {
                    Cow::Owned(
                        output_path(Path::new(file), true, self.format, self.suffix.as_deref())
                            .unwrap_or_else(|_| PathBuf::from(file))
                            .display()
                            .to_string(),
                    )
                } else {
                    file.to_string_lossy()
                };
                self.print_listing(&name, &bytes, &mut header_printed, &mut totals)
            });
            let code = self.finish_operand(&file.to_string_lossy(), result)?;
            status = self.merge_status(status, code);
        }
        if totals.files > 1 {
            if matches!(self.format, Format::Xz) {
                println!("{:-<79}", "");
                totals.xz.print(format_args!("{} files", totals.files));
            } else {
                print_gzip_listing("(totals)", totals.compressed, totals.plain, totals.overhead);
            }
        }
        Ok(status)
    }

    fn print_listing(
        &self,
        name: &str,
        compressed: &[u8],
        header_printed: &mut bool,
        totals: &mut ListingTotals,
    ) -> io::Result<i32> {
        let (decoded, xz) = if matches!(self.format, Format::Xz) {
            (Decoded::clean(0), Some(XzListing::parse(compressed)?))
        } else {
            (self.decode(compressed, io::sink(), false)?, None)
        };
        if !*header_printed {
            println!(
                "{}",
                if matches!(self.format, Format::Gzip) {
                    "         compressed        uncompressed  ratio uncompressed_name"
                } else {
                    "Strms  Blocks   Compressed Uncompressed  Ratio  Check   Filename"
                }
            );
            *header_printed = true;
        }
        if let Some(xz) = xz {
            xz.print(name);
            totals.xz.add(&xz)?;
        } else {
            print_gzip_listing(
                name,
                compressed.len() as u64,
                decoded.bytes,
                decoded.overhead,
            );
            totals.compressed += compressed.len() as u64;
            totals.plain += decoded.bytes;
            totals.overhead += decoded.overhead;
        }
        totals.files += 1;
        self.report_trailing_garbage(name, decoded.trailing_garbage);
        Ok(self.trailing_status(decoded.trailing_garbage))
    }
}

struct Transfer {
    input: u64,
    output: u64,
    trailing_garbage: bool,
}

struct CountRead<R> {
    inner: R,
    bytes: u64,
}

impl<R: Read> Read for CountRead<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.bytes += count as u64;
        Ok(count)
    }
}

impl<R: BufRead> BufRead for CountRead<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
        self.bytes += amount as u64;
    }
}

struct CountWrite<W> {
    inner: W,
    bytes: u64,
}

impl<W: Write> Write for CountWrite<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(buffer)?;
        self.bytes += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Default)]
struct ListingTotals {
    files: u64,
    compressed: u64,
    plain: u64,
    overhead: u64,
    xz: XzListing,
}

fn print_gzip_listing(name: &str, compressed: u64, plain: u64, overhead: u64) {
    let ratio = if plain == 0 {
        0.0
    } else {
        100.0 - compressed.saturating_sub(overhead) as f64 * 100.0 / plain as f64
    };
    println!("{compressed:>19} {plain:>19} {ratio:>5.1}% {name}");
}

#[derive(Default)]
struct XzListing {
    streams: u64,
    blocks: u64,
    compressed: u64,
    plain: u64,
    checks: u16,
}

impl XzListing {
    fn parse(bytes: &[u8]) -> io::Result<Self> {
        let mut listing = Self {
            compressed: bytes.len() as u64,
            ..Self::default()
        };
        let mut end = bytes.len();
        while end != 0 {
            let mut padding = 0;
            while end != 0 && bytes[end - 1] == 0 {
                end -= 1;
                padding += 1;
            }
            if padding % 4 != 0 || end < 32 {
                return Err(bad_xz());
            }
            let footer_start = end - 12;
            let footer = &bytes[footer_start..end];
            if &footer[10..] != b"YZ"
                || !valid_crc32(&footer[4..10], &footer[..4])
                || footer[8] != 0
                || footer[9] > 15
            {
                return Err(bad_xz());
            }
            let backward =
                u32::from_le_bytes(footer[4..8].try_into().expect("four-byte backward size"));
            let index_size =
                usize::try_from((u64::from(backward) + 1) * 4).map_err(|_| bad_xz())?;
            let index_start = footer_start.checked_sub(index_size).ok_or_else(bad_xz)?;
            let index = &bytes[index_start..footer_start];
            if index.len() < 8
                || index[0] != 0
                || !valid_crc32(&index[..index.len() - 4], &index[index.len() - 4..])
            {
                return Err(bad_xz());
            }
            let mut records = &index[1..index.len() - 4];
            let count = xz_vli(&mut records)?;
            if count > records.len() as u64 / 2 {
                return Err(bad_xz());
            }
            let mut block_size = 0_u64;
            let mut plain = 0_u64;
            for _ in 0..count {
                let unpadded = xz_vli(&mut records)?;
                let uncompressed = xz_vli(&mut records)?;
                if unpadded < 5 {
                    return Err(bad_xz());
                }
                let padded = unpadded.checked_add(3).ok_or_else(bad_xz)? & !3;
                block_size = block_size.checked_add(padded).ok_or_else(bad_xz)?;
                plain = plain
                    .checked_add(uncompressed)
                    .filter(|size| *size < (1_u64 << 63))
                    .ok_or_else(bad_xz)?;
            }
            if records.len() > 3 || records.iter().any(|byte| *byte != 0) {
                return Err(bad_xz());
            }
            let before_index = usize::try_from(block_size)
                .map_err(|_| bad_xz())?
                .checked_add(12)
                .ok_or_else(bad_xz)?;
            let stream_start = index_start.checked_sub(before_index).ok_or_else(bad_xz)?;
            let header = &bytes[stream_start..stream_start + 12];
            if &header[..6] != b"\xfd7zXZ\0"
                || header[6..8] != footer[8..10]
                || !valid_crc32(&header[6..8], &header[8..12])
            {
                return Err(bad_xz());
            }
            listing.streams += 1;
            listing.blocks = listing.blocks.checked_add(count).ok_or_else(bad_xz)?;
            listing.plain = listing.plain.checked_add(plain).ok_or_else(bad_xz)?;
            listing.checks |= 1 << footer[9];
            end = stream_start;
        }
        if listing.streams == 0 {
            return Err(bad_xz());
        }
        Ok(listing)
    }

    fn add(&mut self, other: &Self) -> io::Result<()> {
        self.streams = self.streams.checked_add(other.streams).ok_or_else(bad_xz)?;
        self.blocks = self.blocks.checked_add(other.blocks).ok_or_else(bad_xz)?;
        self.compressed = self
            .compressed
            .checked_add(other.compressed)
            .ok_or_else(bad_xz)?;
        self.plain = self.plain.checked_add(other.plain).ok_or_else(bad_xz)?;
        self.checks |= other.checks;
        Ok(())
    }

    fn print(&self, name: impl std::fmt::Display) {
        println!(
            "{:>5} {:>7}  {:>11}  {:>11}  {:>5}  {:<7} {name}",
            self.streams,
            self.blocks,
            nice_size(self.compressed),
            nice_size(self.plain),
            xz_ratio(self.compressed, self.plain),
            CheckNames(self.checks)
        );
    }
}

fn valid_crc32(bytes: &[u8], expected: &[u8]) -> bool {
    let mut crc = flate2::Crc::new();
    crc.update(bytes);
    crc.sum().to_le_bytes() == expected
}

fn xz_vli(bytes: &mut &[u8]) -> io::Result<u64> {
    let mut value = 0_u64;
    for shift in (0..63).step_by(7) {
        let (&byte, rest) = bytes.split_first().ok_or_else(bad_xz)?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            if shift != 0 && byte == 0 {
                return Err(bad_xz());
            }
            return Ok(value);
        }
    }
    Err(bad_xz())
}

fn bad_xz() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Compressed data is corrupt")
}

struct NiceSize(u64);

fn nice_size(bytes: u64) -> NiceSize {
    NiceSize(bytes)
}

impl std::fmt::Display for NiceSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let width = f.width().unwrap_or(0);
        if self.0 < 10_000 {
            return write!(f, "{:>width$} B", self.0, width = width.saturating_sub(2));
        }
        let units = ["KiB", "MiB", "GiB", "TiB"];
        let mut value = self.0 as f64 / 1024.0;
        let mut unit = 0;
        while value > 9999.9 && unit + 1 < units.len() {
            value /= 1024.0;
            unit += 1;
        }
        write!(
            f,
            "{value:>width$.1} {}",
            units[unit],
            width = width.saturating_sub(4)
        )
    }
}

struct XzRatio(u64, u64);

fn xz_ratio(compressed: u64, plain: u64) -> XzRatio {
    XzRatio(compressed, plain)
}

impl std::fmt::Display for XzRatio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ratio = self.0 as f64 / self.1 as f64;
        let width = f.width().unwrap_or(0);
        if self.1 == 0 || ratio > 9.999 {
            write!(f, "{:>width$}", "---")
        } else {
            write!(f, "{ratio:>width$.3}")
        }
    }
}

struct CheckNames(u16);

impl std::fmt::Display for CheckNames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const CHECKS: [&str; 16] = [
            "None",
            "CRC32",
            "Unknown-2",
            "Unknown-3",
            "CRC64",
            "Unknown-5",
            "Unknown-6",
            "Unknown-7",
            "Unknown-8",
            "Unknown-9",
            "SHA-256",
            "Unknown-11",
            "Unknown-12",
            "Unknown-13",
            "Unknown-14",
            "Unknown-15",
        ];
        let mut length = 0;
        for (id, name) in CHECKS.iter().enumerate() {
            if self.0 & (1 << id) != 0 {
                if length != 0 {
                    f.write_str(",")?;
                    length += 1;
                }
                f.write_str(name)?;
                length += name.len();
            }
        }
        write!(
            f,
            "{:width$}",
            "",
            width = f.width().unwrap_or(0).saturating_sub(length)
        )
    }
}

struct Decoded {
    bytes: u64,
    overhead: u64,
    trailing_garbage: bool,
}

impl Decoded {
    fn clean(bytes: u64) -> Self {
        Self {
            bytes,
            overhead: 0,
            trailing_garbage: false,
        }
    }
}

fn read_prefix(input: &mut impl Read, prefix: &mut [u8]) -> io::Result<usize> {
    let mut length = 0;
    while length < prefix.len() {
        match input.read(&mut prefix[length..]) {
            Ok(0) => break,
            Ok(count) => length += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(length)
}

fn matches_magic(prefix: &[u8], magic: &[u8]) -> bool {
    !prefix.is_empty() && (prefix.starts_with(magic) || magic.starts_with(prefix))
}

fn zstd_magic(prefix: &[u8]) -> bool {
    if prefix.len() < 4 {
        return (0xfd2f_b522_u32..=0xfd2f_b528)
            .chain(0x184d_2a50..=0x184d_2a5f)
            .any(|magic| matches_magic(prefix, &magic.to_le_bytes()));
    }
    let magic = u32::from_le_bytes(prefix[..4].try_into().expect("four-byte magic"));
    (0xfd2f_b522..=0xfd2f_b528).contains(&magic) || magic & 0xffff_fff0 == 0x184d_2a50
}

fn decode_bzip2(
    input: impl Read,
    mut output: impl Write,
    passthrough: bool,
) -> io::Result<Decoded> {
    let mut input = io::BufReader::new(input);
    let mut decoded = Decoded::clean(0);
    let mut members = 0_u64;
    loop {
        let mut magic = [0; 4];
        let length = read_prefix(&mut input, &mut magic)?;
        let prefix = &magic[..length];
        if length == 0 && members != 0 {
            return Ok(decoded);
        }
        if matches_magic(prefix, b"BZh") {
            let mut member = BzDecoder::new(prefix.chain(&mut input));
            decoded.bytes += io::copy(&mut member, &mut output).map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidInput {
                    io::Error::new(io::ErrorKind::InvalidData, error)
                } else {
                    error
                }
            })?;
            members += 1;
        } else if members != 0 {
            decoded.trailing_garbage = true;
            return Ok(decoded);
        } else if passthrough {
            output.write_all(prefix)?;
            decoded.bytes = length as u64 + io::copy(&mut input, &mut output)?;
            return Ok(decoded);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a bzip2 file",
            ));
        }
    }
}

fn decode_gzip(input: impl Read, mut output: impl Write, passthrough: bool) -> io::Result<Decoded> {
    let mut input = io::BufReader::new(input);
    let mut decoded = Decoded::clean(0);
    let mut members = 0_u64;
    loop {
        let mut magic = [0; 2];
        let length = read_prefix(&mut input, &mut magic)?;
        if length == 0 && members != 0 {
            return Ok(decoded);
        }
        let prefix = &magic[..length];
        if prefix == [0x1f, 0x8b] {
            let mut member = GzDecoder::new(CountRead {
                inner: prefix.chain(&mut input),
                bytes: 0,
            });
            decoded.overhead += member.get_ref().bytes + 8;
            decoded.bytes += io::copy(&mut member, &mut output)?;
            members += 1;
        } else if passthrough {
            output.write_all(prefix)?;
            decoded.bytes += length as u64 + io::copy(&mut input, &mut output)?;
            return Ok(decoded);
        } else if members != 0 && prefix.first() == Some(&0) {
            let mut only_zeroes = prefix.iter().all(|byte| *byte == 0);
            while only_zeroes {
                let bytes = input.fill_buf()?;
                if bytes.is_empty() {
                    return Ok(decoded);
                }
                only_zeroes = bytes.iter().all(|byte| *byte == 0);
                let count = bytes.len();
                input.consume(count);
            }
            decoded.trailing_garbage = true;
            return Ok(decoded);
        } else if length < magic.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected end of file",
            ));
        } else if members == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not in gzip format",
            ));
        } else {
            decoded.trailing_garbage = true;
            return Ok(decoded);
        }
    }
}

fn collect_input_files(path: &Path, recursive: bool, files: &mut Vec<OsString>) -> io::Result<()> {
    if !recursive || path == Path::new("-") {
        files.push(path.as_os_str().to_owned());
        return Ok(());
    }
    if !fs::symlink_metadata(path)?.is_dir() {
        files.push(path.as_os_str().to_owned());
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        collect_input_files(&entry?.path(), true, files)?;
    }
    Ok(())
}

fn read_stdin() -> io::Result<Vec<u8>> {
    let mut compressed = Vec::new();
    io::stdin().read_to_end(&mut compressed)?;
    Ok(compressed)
}

fn parse_threads(value: &OsStr) -> io::Result<u32> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| invalid(format!("invalid thread count: {}", value.to_string_lossy())))
}

fn same_file(input: &Metadata, output: &Path) -> io::Result<bool> {
    match fs::metadata(output) {
        Ok(output) => Ok(same_identity(input, &output)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn same_identity(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_identity(_left: &Metadata, _right: &Metadata) -> bool {
    false
}

#[cfg(unix)]
const CLEANUP_SLOTS: usize = 16;
#[cfg(unix)]
const CLEANUP_PATH_MAX: usize = 4096;

#[cfg(unix)]
struct CleanupSlot {
    active: AtomicBool,
    path: UnsafeCell<[u8; CLEANUP_PATH_MAX]>,
}

#[cfg(unix)]
impl CleanupSlot {
    const fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            path: UnsafeCell::new([0; CLEANUP_PATH_MAX]),
        }
    }
}

#[cfg(unix)]
// SAFETY: the path is written before active is published and only read by the
// signal handler while active remains true.
unsafe impl Sync for CleanupSlot {}

#[cfg(unix)]
static CLEANUP_SLOTS_STATE: [CleanupSlot; CLEANUP_SLOTS] =
    [const { CleanupSlot::new() }; CLEANUP_SLOTS];
#[cfg(unix)]
static CLEANUP_INSTALLED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
unsafe extern "C" fn cleanup_signal(signal: libc::c_int) {
    for slot in &CLEANUP_SLOTS_STATE {
        if slot.active.load(Ordering::Acquire) {
            // SAFETY: active is published only after a NUL-terminated path is
            // copied into the fixed-size slot.
            unsafe {
                libc::unlink((*slot.path.get()).as_ptr().cast());
            }
        }
    }
    // SAFETY: signal disposition and process-directed signal delivery are
    // async-signal-safe.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::kill(libc::getpid(), signal);
        libc::_exit(128 + signal);
    }
}

#[cfg(unix)]
fn install_cleanup_signals() -> io::Result<()> {
    if CLEANUP_INSTALLED.load(Ordering::Acquire) {
        return Ok(());
    }
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: cleanup_signal has the C signal-handler ABI and does not
        // capture state.
        let result =
            unsafe { libc::signal(signal, cleanup_signal as *const () as libc::sighandler_t) };
        if result == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    CLEANUP_INSTALLED.store(true, Ordering::Release);
    Ok(())
}

#[cfg(unix)]
fn register_cleanup_path(path: &Path) -> io::Result<usize> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.len() >= CLEANUP_PATH_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "temporary output path is too long",
        ));
    }
    install_cleanup_signals()?;
    for (index, slot) in CLEANUP_SLOTS_STATE.iter().enumerate() {
        if !slot.active.load(Ordering::Acquire) {
            // SAFETY: no active handler can read this slot until active is
            // published below, and staging creation is single-threaded.
            unsafe {
                let path = &mut *slot.path.get();
                path[..bytes.len()].copy_from_slice(bytes);
                path[bytes.len()] = 0;
            }
            slot.active.store(true, Ordering::Release);
            return Ok(index);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "too many active temporary outputs",
    ))
}

#[cfg(unix)]
fn unregister_cleanup_path(index: usize) {
    CLEANUP_SLOTS_STATE[index]
        .active
        .store(false, Ordering::Release);
}

struct PendingOutput {
    path: Option<PathBuf>,
    file: File,
    directory: File,
    #[cfg(unix)]
    signal_slot: usize,
}

impl PendingOutput {
    fn new(destination: &Path, force: bool, has_source_metadata: bool) -> io::Result<Self> {
        if !force {
            match fs::symlink_metadata(destination) {
                Ok(_) => return Err(output_exists(destination)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = output_parent(destination);
        let directory = File::open(parent)?;
        for _ in 0..128 {
            let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
            let time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_nanos());
            let path = parent.join(format!(
                ".axe-compress-{}-{time}-{nonce}",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(if has_source_metadata { 0o600 } else { 0o666 });
            }
            match options.open(&path) {
                Ok(file) => {
                    #[cfg(unix)]
                    let signal_slot = match register_cleanup_path(&path) {
                        Ok(slot) => slot,
                        Err(error) => {
                            let _ = fs::remove_file(&path);
                            return Err(error);
                        }
                    };
                    return Ok(Self {
                        path: Some(path),
                        file,
                        directory,
                        #[cfg(unix)]
                        signal_slot,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot create a temporary output",
        ))
    }

    fn publish(
        &mut self,
        destination: &Path,
        force: bool,
        metadata: Option<&Metadata>,
    ) -> io::Result<()> {
        if let Some(metadata) = metadata {
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                use std::os::unix::fs::{MetadataExt, PermissionsExt};

                // GNU copy_stat ignores EPERM when ownership cannot be
                // restored, but always strips set-ID bits before chmod so
                // they cannot be granted to a different owner.
                let result = unsafe {
                    libc::fchown(
                        self.file.as_raw_fd(),
                        metadata.uid() as libc::uid_t,
                        metadata.gid() as libc::gid_t,
                    )
                };
                if result == -1 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EPERM) {
                        return Err(error);
                    }
                }
                self.file.set_permissions(fs::Permissions::from_mode(
                    metadata.permissions().mode() & 0o777,
                ))?;
            }
            #[cfg(not(unix))]
            self.file.set_permissions(metadata.permissions())?;
            self.file.set_times(
                fs::FileTimes::new()
                    .set_accessed(metadata.accessed()?)
                    .set_modified(metadata.modified()?),
            )?;
        }
        self.file.sync_all()?;
        let staging = self
            .path
            .as_ref()
            .expect("unpublished output has a staging path");
        if force {
            fs::rename(staging, destination)?;
            self.path = None;
        } else {
            // link(2) publishes without replacing a name created during compression.
            fs::hard_link(staging, destination).map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    output_exists(destination)
                } else {
                    error
                }
            })?;
            fs::remove_file(staging)?;
            self.path = None;
        }
        #[cfg(unix)]
        unregister_cleanup_path(self.signal_slot);
        // The caller must not remove its input until the new directory entry is durable.
        self.directory.sync_all()
    }

    fn abort(&mut self, error: io::Error) -> io::Error {
        let Some(path) = self.path.take() else {
            return error;
        };
        let result = match fs::remove_file(&path) {
            Ok(()) => error,
            Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => error,
            Err(cleanup) => io::Error::new(
                error.kind(),
                format!(
                    "{error}; cannot remove temporary output {}: {cleanup}",
                    path.display()
                ),
            ),
        };
        #[cfg(unix)]
        unregister_cleanup_path(self.signal_slot);
        result
    }
}

impl Drop for PendingOutput {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
            #[cfg(unix)]
            unregister_cleanup_path(self.signal_slot);
        }
    }
}

fn output_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn output_exists(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} already exists", path.display()),
    )
}

fn os_string_from_bytes(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        OsString::from_vec(bytes.to_vec())
    }
    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn compressed_suffix(path: &Path, format: Format, custom: Option<&OsStr>) -> Option<OsString> {
    let name = path.file_name()?.as_encoded_bytes();
    if let Some(suffix) = custom
        && name.ends_with(suffix.as_encoded_bytes())
    {
        return Some(suffix.to_os_string());
    }
    let suffixes: &[&[u8]] = match format {
        Format::Gzip => &[b".tgz", b".taz", b".gz", b"-gz", b".z", b"-z", b"_z", b".Z"],
        Format::Bzip2 => &[b".tbz2", b".tbz", b".bz2", b".bz"],
        Format::Xz => &[b".txz", b".xz"],
        Format::Zstd => &[],
    };
    suffixes
        .iter()
        .find(|suffix| name.ends_with(suffix))
        .map(|suffix| os_string_from_bytes(suffix))
}

fn append_suffix(input: &Path, suffix: &OsStr) -> PathBuf {
    let mut output = input.as_os_str().to_owned();
    output.push(suffix);
    output.into()
}

fn remove_suffix(input: &Path, suffix: &[u8]) -> Option<PathBuf> {
    let name = input.file_name()?.as_encoded_bytes();
    name.ends_with(suffix)
        .then(|| path_with_file_name(input, &name[..name.len() - suffix.len()]))
}

fn path_with_file_name(input: &Path, name: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        input.with_file_name(OsString::from_vec(name.to_vec()))
    }
    #[cfg(not(unix))]
    {
        input.with_file_name(String::from_utf8_lossy(name).into_owned())
    }
}

fn output_path(
    input: &Path,
    decompress: bool,
    format: Format,
    custom: Option<&OsStr>,
) -> io::Result<PathBuf> {
    if !decompress {
        let suffix = custom.unwrap_or_else(|| match format {
            Format::Gzip => OsStr::new(".gz"),
            Format::Bzip2 => OsStr::new(".bz2"),
            Format::Xz => OsStr::new(".xz"),
            Format::Zstd => OsStr::new(".zst"),
        });
        return Ok(append_suffix(input, suffix));
    }

    let unknown = || {
        invalid(match format {
            Format::Xz => format!(
                "{}: Filename has an unknown suffix, skipping",
                input.display()
            ),
            Format::Zstd => format!(
                "{}: unknown suffix (.zst/.tzst expected). Can't derive the output file name. Specify it with -o dstFileName. Ignoring.",
                input.display()
            ),
            _ => format!("{}: unknown suffix -- ignored", input.display()),
        })
    };
    match format {
        Format::Gzip => {
            if let Some(suffix) = custom
                && let Some(output) = remove_suffix(input, suffix.as_encoded_bytes())
            {
                return Ok(output);
            }
            for (suffix, tar) in [
                (b".tgz".as_slice(), true),
                (b".taz".as_slice(), true),
                (b".gz".as_slice(), false),
                (b"-gz".as_slice(), false),
                (b".z".as_slice(), false),
                (b"-z".as_slice(), false),
                (b"_z".as_slice(), false),
                (b".Z".as_slice(), false),
            ] {
                if let Some(output) = remove_suffix(input, suffix) {
                    return Ok(if tar {
                        append_suffix(&output, OsStr::new(".tar"))
                    } else {
                        output
                    });
                }
            }
            Err(unknown())
        }
        Format::Bzip2 | Format::Xz | Format::Zstd => {
            let defaults: &[(&[u8], bool)] = match format {
                Format::Bzip2 => &[
                    (b".tbz2", true),
                    (b".tbz", true),
                    (b".bz2", false),
                    (b".bz", false),
                ],
                Format::Xz => &[(b".txz", true), (b".xz", false)],
                Format::Zstd => &[(b".tzst", true), (b".zst", false)],
                Format::Gzip => unreachable!(),
            };
            if let Some(suffix) = custom
                && let Some(output) = remove_suffix(input, suffix.as_encoded_bytes())
            {
                return Ok(output);
            }
            for (suffix, tar) in defaults {
                if let Some(output) = remove_suffix(input, suffix) {
                    return Ok(if *tar {
                        append_suffix(&output, OsStr::new(".tar"))
                    } else {
                        output
                    });
                }
            }
            if matches!(format, Format::Bzip2) {
                return Ok(append_suffix(input, OsStr::new(".out")));
            }
            Err(unknown())
        }
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn done() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_magic_survives_one_byte_reads_and_interruption_between_members() {
        struct ShortReads<'a> {
            input: &'a [u8],
            interrupted: bool,
        }

        impl Read for ShortReads<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.interrupted = false;
                let length = buffer.len().min(1);
                self.input.read(&mut buffer[..length])
            }
        }

        let mut compressed = Vec::new();
        for payload in [b"first".as_slice(), b"second".as_slice()] {
            let mut encoder = GzEncoder::new(&mut compressed, flate2::Compression::default());
            encoder.write_all(payload).unwrap();
            encoder.finish().unwrap();
        }
        let mut output = Vec::new();
        let decoded = decode_gzip(
            ShortReads {
                input: &compressed,
                interrupted: true,
            },
            &mut output,
            true,
        )
        .unwrap();
        assert_eq!(output, b"firstsecond");
        assert_eq!(decoded.bytes, output.len() as u64);
        assert!(!decoded.trailing_garbage);
    }

    #[test]
    fn gzip_emits_complete_members_before_a_corrupt_member_fails() {
        let mut compressed = Vec::new();
        let mut encoder = GzEncoder::new(&mut compressed, flate2::Compression::default());
        encoder.write_all(b"complete member").unwrap();
        encoder.finish().unwrap();
        compressed.extend_from_slice(&[0x1f, 0x8b, 8]);
        let mut output = Vec::new();
        assert!(decode_gzip(compressed.as_slice(), &mut output, true).is_err());
        assert_eq!(output, b"complete member");
    }

    #[test]
    fn traditional_suffixes_are_mapped() {
        assert_eq!(
            output_path(Path::new("archive.tbz2"), true, Format::Bzip2, None,).unwrap(),
            PathBuf::from("archive.tar")
        );
        assert_eq!(
            output_path(Path::new("data.xz"), true, Format::Xz, None).unwrap(),
            PathBuf::from("data")
        );
        assert_eq!(
            output_path(Path::new("archive.tgz"), true, Format::Gzip, None,).unwrap(),
            PathBuf::from("archive.tar")
        );
    }

    #[test]
    fn unknown_decompression_suffix_is_rejected() {
        assert!(output_path(Path::new("data.bin"), true, Format::Xz, None).is_err());
        assert!(output_path(Path::new("data.bz2"), true, Format::Gzip, None).is_err());
    }
}
