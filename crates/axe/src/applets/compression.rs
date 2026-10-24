use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use bzip2::read::{BzEncoder, MultiBzDecoder};
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
            Self::Gzip => "[-cdfklt] [FILE ...]",
            Self::Bzip2 | Self::Xz => "[-cdfktl] [-T THREADS] [-1..-9] [FILE ...]",
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
        Err(error)
            if error.kind() == io::ErrorKind::BrokenPipe
                || (error.kind() == io::ErrorKind::Interrupted && error.to_string().is_empty()) =>
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
    level: i32,
    threads: u32,
    files: Vec<OsString>,
    output: Option<PathBuf>,
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
            "gunzip" | "zcat" | "bunzip2" | "bzcat" | "unxz" | "xzcat"
        );
        let stdout = matches!(program.as_str(), "zcat" | "bzcat" | "xzcat");
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
            level: if matches!(format, Format::Zstd) { 3 } else { 6 },
            threads: 1,
            files: Vec::new(),
            output: None,
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
                for flag in text[1..].chars() {
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

        Ok(options)
    }

    /// Applies a codec-specific word option; returns false when `text` is not one.
    fn long_option(
        &mut self,
        text: &str,
        args: &mut impl Iterator<Item = OsString>,
    ) -> io::Result<bool> {
        if matches!(self.format, Format::Zstd) {
            match text {
                "--compress" => self.decompress = false,
                "--decompress" => self.decompress = true,
                "--stdout" => self.stdout = true,
                "--force" => self.force = true,
                "--keep" => self.keep = true,
                "--rm" => self.keep = false,
                "--test" => {
                    self.test = true;
                    self.decompress = true;
                }
                "--quiet" => self.quiet = true,
                "--verbose" => self.verbose = true,
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
                "--test" => {
                    self.test = true;
                    self.decompress = true;
                }
                "--quiet" => self.quiet = true,
                "--verbose" => self.verbose = true,
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
                } else {
                    return self.thread_option(text, args);
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
            (Format::Gzip, 'n') => {}
            (Format::Gzip, 'q') => self.quiet = true,
            (Format::Gzip, 'v') => self.verbose = true,
            (Format::Gzip, 'l') => {
                self.list = true;
                self.decompress = true;
            }
            (Format::Bzip2 | Format::Xz | Format::Zstd, 'z') => self.decompress = false,
            (Format::Bzip2 | Format::Xz, 'q' | 'v') => {}
            (Format::Zstd, 'q') => self.quiet = true,
            (Format::Zstd, 'v') => self.verbose = true,
            (Format::Xz, 'l') => self.list = true,
            (Format::Bzip2 | Format::Xz, '1'..='9') => {
                self.level = flag.to_digit(10).expect("digit") as i32;
            }
            _ => return Err(invalid(format!("unsupported option -{flag}"))),
        }

        Ok(())
    }

    fn run(self) -> io::Result<i32> {
        if self.list {
            return self
                .list_contents()
                .map(|warning| if warning { 2 } else { 0 });
        }

        if self.files.is_empty() {
            return self
                .filter_stdin()
                .map(|warning| if warning { 2 } else { 0 });
        }

        let mut warning = false;
        for file in &self.files {
            if file == OsStr::new("-") {
                warning |= self.filter_stdin()?;
                continue;
            }

            let input_path = Path::new(file);
            let input = File::open(input_path)?;
            let trailing_garbage = if self.test {
                self.decode(input, io::sink(), true)?.trailing_garbage
            } else if self.stdout {
                self.transcode(input, io::stdout().lock(), true)?
            } else {
                let output_path = match &self.output {
                    Some(path) => {
                        if same_file(&input, path)? {
                            return Err(invalid("input and output must be different files"));
                        }
                        Cow::Borrowed(path.as_path())
                    }
                    None => Cow::Owned(output_path(input_path, self.decompress, self.format)?),
                };
                let trailing_garbage = self.transcode_file(input, &output_path)?;

                if self.verbose && !self.quiet {
                    eprintln!("{}: done", input_path.display());
                }

                if !self.keep {
                    fs::remove_file(input_path)?;
                }
                trailing_garbage
            };
            if trailing_garbage {
                self.report_trailing_garbage(&input_path.display().to_string(), true);
            }
            warning |= trailing_garbage;
        }

        Ok(if warning { 2 } else { 0 })
    }

    fn filter_stdin(&self) -> io::Result<bool> {
        let stdin = io::stdin().lock();
        let warning = if self.test {
            self.decode(stdin, io::sink(), true)?.trailing_garbage
        } else if !self.stdout
            && let Some(path) = &self.output
        {
            self.transcode_file(stdin, path)?
        } else {
            self.transcode(stdin, io::stdout().lock(), true)?
        };
        self.report_trailing_garbage("(stdin)", warning);
        Ok(warning)
    }

    fn report_trailing_garbage(&self, name: &str, warning: bool) {
        if warning && !self.quiet {
            eprintln!(
                "\n{}: {name}: decompression OK, trailing garbage ignored",
                self.program
            );
        }
    }

    fn transcode_file(&self, input: impl Read, path: &Path) -> io::Result<bool> {
        let mut output = DeferredOutput {
            path,
            force: self.force,
            file: None,
        };
        let result = self.transcode(input, &mut output, false);
        if result.is_ok() || output.file.is_none() {
            return result;
        }
        drop(output.file.take());
        match (result, fs::remove_file(path)) {
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(cleanup)) if cleanup.kind() == io::ErrorKind::NotFound => Err(error),
            (Err(error), Err(cleanup)) => Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}; cannot remove partial output {}: {cleanup}",
                    path.display()
                ),
            )),
            (Ok(warning), _) => Ok(warning),
        }
    }

    fn transcode(&self, input: impl Read, output: impl Write, to_stdout: bool) -> io::Result<bool> {
        if self.decompress {
            self.decode(input, output, to_stdout)
                .map(|decoded| decoded.trailing_garbage)
        } else {
            self.encode(input, output).map(|()| false)
        }
    }

    fn encode(&self, mut input: impl Read, mut output: impl Write) -> io::Result<()> {
        match self.format {
            Format::Gzip => {
                let mut encoder = GzEncoder::new(output, flate2::Compression::default());
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
        let decoded = match self.format {
            Format::Gzip => decode_gzip(input, &mut output, self.force && to_stdout && !self.list)?,
            Format::Bzip2 => {
                Decoded::clean(io::copy(&mut MultiBzDecoder::new(input), &mut output)?)
            }
            Format::Xz => Decoded::clean(io::copy(&mut XzReader::new(input, true), &mut output)?),
            Format::Zstd => Decoded::clean(io::copy(
                &mut zstd::stream::read::Decoder::new(input)?,
                &mut output,
            )?),
        };
        output.flush()?;
        Ok(decoded)
    }

    fn list_contents(&self) -> io::Result<bool> {
        let mut header_printed = false;
        if self.files.is_empty() {
            return self.print_listing("-", &read_stdin()?, &mut header_printed);
        }

        let mut warning = false;
        for file in &self.files {
            if file == OsStr::new("-") {
                warning |= self.print_listing("-", &read_stdin()?, &mut header_printed)?;
                continue;
            }

            let compressed = fs::read(file)?;
            let name = match self.format {
                Format::Gzip => output_path(Path::new(file), true, self.format)
                    .unwrap_or_else(|_| PathBuf::from(file))
                    .display()
                    .to_string(),
                Format::Bzip2 | Format::Xz => file.to_string_lossy().into_owned(),
                Format::Zstd => unreachable!("zstd does not accept --list"),
            };
            warning |= self.print_listing(&name, &compressed, &mut header_printed)?;
        }

        Ok(warning)
    }

    fn print_listing(
        &self,
        name: &str,
        compressed: &[u8],
        header_printed: &mut bool,
    ) -> io::Result<bool> {
        let decoded = self.decode(compressed, io::sink(), false)?;
        let uncompressed = decoded.bytes;
        let ratio = |value: f64| if uncompressed == 0 { 0.0 } else { value };
        if !*header_printed {
            println!(
                "{}",
                match self.format {
                    Format::Gzip =>
                        "         compressed        uncompressed  ratio uncompressed_name",
                    Format::Bzip2 | Format::Xz => {
                        "Strms  Blocks   Compressed Uncompressed  Ratio  Check   Filename"
                    }
                    Format::Zstd => unreachable!("zstd does not accept --list"),
                }
            );
            *header_printed = true;
        }
        match self.format {
            Format::Gzip => println!(
                "{:>19} {:>19} {:>5.1}% {name}",
                compressed.len(),
                uncompressed,
                ratio(100.0 - compressed.len() as f64 * 100.0 / uncompressed as f64)
            ),
            Format::Bzip2 | Format::Xz => println!(
                "{:>5} {:>7} {:>12} {:>12} {:>6.3}  CRC64   {name}",
                1,
                1,
                compressed.len(),
                uncompressed,
                ratio(compressed.len() as f64 / uncompressed as f64)
            ),
            Format::Zstd => unreachable!("zstd does not accept --list"),
        }

        self.report_trailing_garbage(name, decoded.trailing_garbage);
        Ok(decoded.trailing_garbage)
    }
}

struct Decoded {
    bytes: u64,
    trailing_garbage: bool,
}

impl Decoded {
    fn clean(bytes: u64) -> Self {
        Self {
            bytes,
            trailing_garbage: false,
        }
    }
}

fn decode_gzip(input: impl Read, mut output: impl Write, passthrough: bool) -> io::Result<Decoded> {
    let mut input = io::BufReader::new(input);
    let mut decoded = Decoded::clean(0);
    let mut members = 0_u64;
    loop {
        // A pipe read may stop between the two magic bytes.
        let mut magic = [0; 2];
        let mut length = 0;
        while length < magic.len() {
            match input.read(&mut magic[length..]) {
                Ok(0) => break,
                Ok(count) => length += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        if length == 0 && members != 0 {
            return Ok(decoded);
        }
        let prefix = &magic[..length];
        if prefix == [0x1f, 0x8b] {
            let mut member = GzDecoder::new(prefix.chain(&mut input));
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

fn same_file(input: &File, output: &Path) -> io::Result<bool> {
    let output = match fs::metadata(output) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let input = input.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(input.dev() == output.dev() && input.ino() == output.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (input, output);
        Err(invalid(
            "explicit output files require Unix file identity checks",
        ))
    }
}

// Invalid input must not truncate an existing destination before decoding starts.
struct DeferredOutput<'a> {
    path: &'a Path,
    force: bool,
    file: Option<File>,
}

impl DeferredOutput<'_> {
    fn file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            self.file = Some(create_output(self.path, self.force)?);
        }
        Ok(self.file.as_mut().expect("file was opened above"))
    }
}

impl Write for DeferredOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.file()?.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file()?.flush()
    }
}

fn create_output(path: &Path, force: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    options.open(path).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", path.display()),
            )
        } else {
            error
        }
    })
}

fn output_path(input: &Path, decompress: bool, format: Format) -> io::Result<PathBuf> {
    if !decompress {
        let mut output = input.as_os_str().to_owned();
        output.push(match format {
            Format::Gzip => ".gz",
            Format::Bzip2 => ".bz2",
            Format::Xz => ".xz",
            Format::Zstd => ".zst",
        });
        return Ok(output.into());
    }

    let unknown = || invalid(format!("{}: unknown suffix -- ignored", input.display()));
    match format {
        Format::Gzip => {
            let name = input
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or_default();
            let output_name = if let Some(stem) = name
                .strip_suffix(".tgz")
                .or_else(|| name.strip_suffix(".taz"))
            {
                format!("{stem}.tar")
            } else if let Some(stem) = [".gz", "-gz", ".z", "-z", "_z"]
                .into_iter()
                .find_map(|suffix| name.strip_suffix(suffix))
            {
                stem.to_owned()
            } else {
                return Err(unknown());
            };
            Ok(input.with_file_name(output_name))
        }
        Format::Bzip2 | Format::Xz | Format::Zstd => {
            let suffix = input
                .extension()
                .and_then(OsStr::to_str)
                .unwrap_or_default();
            let accepted = match format {
                Format::Bzip2 => matches!(suffix, "bz2" | "bz" | "tbz" | "tbz2"),
                Format::Gzip | Format::Xz => suffix == "xz",
                Format::Zstd => matches!(suffix, "zst" | "tzst"),
            };
            if !accepted {
                return Err(unknown());
            }
            let mut output = input.to_path_buf();
            output.set_extension(if matches!(suffix, "tbz" | "tbz2" | "tzst") {
                "tar"
            } else {
                ""
            });
            Ok(output)
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
            output_path(Path::new("archive.tbz2"), true, Format::Bzip2).unwrap(),
            PathBuf::from("archive.tar")
        );
        assert_eq!(
            output_path(Path::new("data.xz"), true, Format::Xz).unwrap(),
            PathBuf::from("data")
        );
        assert_eq!(
            output_path(Path::new("archive.tgz"), true, Format::Gzip).unwrap(),
            PathBuf::from("archive.tar")
        );
    }

    #[test]
    fn unknown_decompression_suffix_is_rejected() {
        assert!(output_path(Path::new("data.bin"), true, Format::Xz).is_err());
        assert!(output_path(Path::new("data.bz2"), true, Format::Gzip).is_err());
    }
}
