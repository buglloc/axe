use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use bzip2::read::{BzEncoder, MultiBzDecoder};
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use lzma_rust2::{XzOptions, XzReader, XzWriter, XzWriterMt};

#[derive(Clone, Copy)]
enum Format {
    Gzip,
    Bzip2,
    Xz,
}

impl Format {
    fn default_name(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Bzip2 => "bzip2",
            Self::Xz => "xz",
        }
    }

    fn usage(self) -> &'static str {
        match self {
            Self::Gzip => "[-cdfkt] [FILE ...]",
            Self::Bzip2 | Self::Xz => "[-cdfktl] [-T THREADS] [-1..-9] [FILE ...]",
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

fn entry(args: Vec<OsString>, format: Format) -> i32 {
    let name = args
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .and_then(OsStr::to_str)
        .unwrap_or(format.default_name())
        .to_owned();

    match Options::parse(name.clone(), format, args.into_iter().skip(1)).and_then(Options::run) {
        Ok(()) => 0,
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
    level: u32,
    threads: u32,
    files: Vec<OsString>,
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
            keep: false,
            force: false,
            test: false,
            list: false,
            quiet: false,
            verbose: false,
            level: 6,
            threads: 1,
            files: Vec::new(),
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

        Ok(options)
    }

    /// Applies a codec-specific word option; returns false when `text` is not one.
    fn long_option(
        &mut self,
        text: &str,
        args: &mut impl Iterator<Item = OsString>,
    ) -> io::Result<bool> {
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
                if text == "-T" || text == "--threads" {
                    let value = args
                        .next()
                        .ok_or_else(|| invalid("-T requires a thread count"))?;
                    self.threads = parse_threads(&value)?;
                } else if let Some(value) = text.strip_prefix("--threads=") {
                    self.threads = parse_threads(OsStr::new(value))?;
                } else if let Some(value) =
                    text.strip_prefix("-T").filter(|value| !value.is_empty())
                {
                    self.threads = parse_threads(OsStr::new(value))?;
                } else if text == "--list" || text == "-l" {
                    if !matches!(self.format, Format::Xz) {
                        return Err(invalid("--list is only supported by xz"));
                    }
                    self.list = true;
                } else {
                    return Ok(false);
                }
            }
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
            (Format::Bzip2 | Format::Xz, 'z') => self.decompress = false,
            (Format::Bzip2 | Format::Xz, 'q' | 'v') => {}
            (Format::Xz, 'l') => self.list = true,
            (Format::Bzip2 | Format::Xz, '1'..='9') => {
                self.level = flag.to_digit(10).expect("digit");
            }
            _ => return Err(invalid(format!("unsupported option -{flag}"))),
        }

        Ok(())
    }

    fn run(self) -> io::Result<()> {
        if self.list {
            return self.list_contents();
        }

        if self.files.is_empty() {
            return self.filter_stdin();
        }

        for file in &self.files {
            if file == OsStr::new("-") {
                self.filter_stdin()?;
                continue;
            }

            let input_path = Path::new(file);
            let input = File::open(input_path)?;
            if self.test {
                self.decode(input, io::sink())?;
                continue;
            }

            if self.stdout {
                self.transcode(input, io::stdout().lock())?;
                continue;
            }

            let output_path = output_path(input_path, self.decompress, self.format)?;
            let output = create_output(&output_path, self.force)?;
            if let Err(error) = self.transcode(input, output) {
                return match fs::remove_file(&output_path) {
                    Ok(()) => Err(error),
                    Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => Err(error),
                    Err(cleanup) => Err(io::Error::new(
                        error.kind(),
                        format!(
                            "{error}; cannot remove partial output {}: {cleanup}",
                            output_path.display()
                        ),
                    )),
                };
            }

            if self.verbose && !self.quiet {
                eprintln!("{}: done", input_path.display());
            }

            if !self.keep {
                fs::remove_file(input_path)?;
            }
        }

        Ok(())
    }

    fn filter_stdin(&self) -> io::Result<()> {
        let stdin = io::stdin().lock();
        if self.test {
            self.decode(stdin, io::sink()).map(drop)
        } else {
            self.transcode(stdin, io::stdout().lock())
        }
    }

    fn transcode(&self, input: impl Read, output: impl Write) -> io::Result<()> {
        if self.decompress {
            self.decode(input, output).map(drop)
        } else {
            self.encode(input, output)
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
                let mut encoder = BzEncoder::new(input, bzip2::Compression::new(self.level));
                io::copy(&mut encoder, &mut output)?;
                output.flush()
            }
            Format::Xz => {
                let options = XzOptions::with_preset(self.level);
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
        }
    }

    /// Decodes every concatenated stream and returns the decoded byte count.
    fn decode(&self, input: impl Read, mut output: impl Write) -> io::Result<u64> {
        let decoded = match self.format {
            Format::Gzip => io::copy(&mut MultiGzDecoder::new(input), &mut output)?,
            Format::Bzip2 => io::copy(&mut MultiBzDecoder::new(input), &mut output)?,
            Format::Xz => io::copy(&mut XzReader::new(input, true), &mut output)?,
        };
        output.flush()?;
        Ok(decoded)
    }

    fn list_contents(&self) -> io::Result<()> {
        println!(
            "{}",
            match self.format {
                Format::Gzip => "         compressed        uncompressed  ratio uncompressed_name",
                Format::Bzip2 | Format::Xz => {
                    "Strms  Blocks   Compressed Uncompressed  Ratio  Check   Filename"
                }
            }
        );

        if self.files.is_empty() {
            return self.print_listing("-", &read_stdin()?);
        }

        for file in &self.files {
            if file == OsStr::new("-") {
                self.print_listing("-", &read_stdin()?)?;
                continue;
            }

            let compressed = fs::read(file)?;
            let name = match self.format {
                Format::Gzip => output_path(Path::new(file), true, self.format)?
                    .display()
                    .to_string(),
                Format::Bzip2 | Format::Xz => file.to_string_lossy().into_owned(),
            };
            self.print_listing(&name, &compressed)?;
        }

        Ok(())
    }

    fn print_listing(&self, name: &str, compressed: &[u8]) -> io::Result<()> {
        let uncompressed = self.decode(compressed, io::sink())?;
        let ratio = |value: f64| if uncompressed == 0 { 0.0 } else { value };
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
        }

        Ok(())
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
        Format::Bzip2 | Format::Xz => {
            let suffix = input
                .extension()
                .and_then(OsStr::to_str)
                .unwrap_or_default();
            let accepted = match format {
                Format::Bzip2 => matches!(suffix, "bz2" | "bz" | "tbz" | "tbz2"),
                Format::Gzip | Format::Xz => suffix == "xz",
            };
            if !accepted {
                return Err(unknown());
            }
            let mut output = input.to_path_buf();
            output.set_extension(if matches!(suffix, "tbz" | "tbz2") {
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
