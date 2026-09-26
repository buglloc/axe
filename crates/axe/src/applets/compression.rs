use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use bzip2::Compression;
use bzip2::read::{BzEncoder, MultiBzDecoder};
use lzma_rust2::{XzOptions, XzReader, XzWriter, XzWriterMt};

#[derive(Clone, Copy)]
enum Format {
    Bzip2,
    Xz,
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
        .unwrap_or(match format {
            Format::Bzip2 => "bzip2",
            Format::Xz => "xz",
        })
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
    level: u32,
    threads: u32,
    list: bool,
    files: Vec<OsString>,
}

impl Options {
    fn parse(
        program: String,
        format: Format,
        args: impl Iterator<Item = OsString>,
    ) -> io::Result<Self> {
        let decompress = matches!(program.as_str(), "bunzip2" | "bzcat" | "unxz" | "xzcat");
        let stdout = matches!(program.as_str(), "bzcat" | "xzcat");
        let mut options = Self {
            program,
            format,
            decompress,
            stdout,
            keep: false,
            force: false,
            test: false,
            level: 6,
            threads: 1,
            list: false,
            files: Vec::new(),
        };
        let mut positional = false;
        let mut args = args.peekable();
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
                println!(
                    "Usage: {} [-cdfktl] [-T THREADS] [-1..-9] [FILE ...]",
                    options.program
                );
                return Err(done());
            } else if text == "--version" {
                println!("{} (axe) {}", options.program, env!("CARGO_PKG_VERSION"));
                return Err(done());
            } else if text == "-T" || text == "--threads" {
                let value = args
                    .next()
                    .ok_or_else(|| invalid("-T requires a thread count"))?;
                options.threads = parse_threads(&value)?;
            } else if let Some(value) = text.strip_prefix("--threads=") {
                options.threads = parse_threads(OsStr::new(value))?;
            } else if let Some(value) = text.strip_prefix("-T").filter(|value| !value.is_empty()) {
                options.threads = parse_threads(OsStr::new(value))?;
            } else if text == "--list" || text == "-l" {
                if !matches!(options.format, Format::Xz) {
                    return Err(invalid("--list is only supported by xz"));
                }
                options.list = true;
            } else if text.starts_with('-') && text != "-" {
                for flag in text[1..].chars() {
                    match flag {
                        'c' => options.stdout = true,
                        'd' => options.decompress = true,
                        'z' => options.decompress = false,
                        'f' => options.force = true,
                        'k' => options.keep = true,
                        't' => {
                            options.test = true;
                            options.decompress = true;
                        }
                        'q' | 'v' => {}
                        'l' if matches!(options.format, Format::Xz) => options.list = true,
                        '1'..='9' => options.level = flag.to_digit(10).expect("digit"),
                        _ => return Err(invalid(format!("unsupported option -{flag}"))),
                    }
                }
            } else {
                options.files.push(argument);
            }
        }
        Ok(options)
    }

    fn run(self) -> io::Result<()> {
        if self.list {
            return self.list_contents();
        }

        if self.files.is_empty() {
            let stdin = io::stdin();
            if self.test {
                return self.copy_decode(stdin.lock(), io::sink());
            }
            let stdout = io::stdout();
            return if self.decompress {
                self.copy_decode(stdin.lock(), stdout.lock())
            } else {
                self.copy_encode(stdin.lock(), stdout.lock())
            };
        }

        for argument in &self.files {
            if argument == OsStr::new("-") {
                let stdin = io::stdin();
                if self.test {
                    self.copy_decode(stdin.lock(), io::sink())?;
                } else {
                    let stdout = io::stdout();
                    if self.decompress {
                        self.copy_decode(stdin.lock(), stdout.lock())?;
                    } else {
                        self.copy_encode(stdin.lock(), stdout.lock())?;
                    }
                }
                continue;
            }

            let input_path = Path::new(argument);
            let input = File::open(input_path)?;
            if self.test {
                self.copy_decode(input, io::sink())?;
                continue;
            }
            if self.stdout {
                let stdout = io::stdout();
                if self.decompress {
                    self.copy_decode(input, stdout.lock())?;
                } else {
                    self.copy_encode(input, stdout.lock())?;
                }
                continue;
            }

            let output_path = output_path(input_path, self.decompress, self.format)?;
            let output = create_output(&output_path, self.force)?;
            let result = if self.decompress {
                self.copy_decode(input, output)
            } else {
                self.copy_encode(input, output)
            };
            if let Err(error) = result {
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
            if !self.keep {
                fs::remove_file(input_path)?;
            }
        }
        Ok(())
    }

    fn copy_encode(&self, input: impl Read, mut output: impl Write) -> io::Result<()> {
        match self.format {
            Format::Bzip2 => {
                let mut encoder = BzEncoder::new(input, Compression::new(self.level));
                io::copy(&mut encoder, &mut output)?;
                output.flush()
            }
            Format::Xz => {
                let options = XzOptions::with_preset(self.level);
                if self.threads == 1 {
                    let mut encoder = XzWriter::new(output, options).map_err(io::Error::other)?;
                    let mut input = input;
                    io::copy(&mut input, &mut encoder)?;
                    let mut output = encoder.finish().map_err(io::Error::other)?;
                    output.flush()
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
                    let mut input = input;
                    io::copy(&mut input, &mut encoder)?;
                    let mut output = encoder.finish().map_err(io::Error::other)?;
                    output.flush()
                }
            }
        }
    }

    fn copy_decode(&self, input: impl Read, mut output: impl Write) -> io::Result<()> {
        match self.format {
            Format::Bzip2 => {
                let mut decoder = MultiBzDecoder::new(input);
                io::copy(&mut decoder, &mut output)?;
            }
            Format::Xz => {
                let mut decoder = XzReader::new(input, true);
                io::copy(&mut decoder, &mut output)?;
            }
        }
        output.flush()
    }

    fn list_contents(&self) -> io::Result<()> {
        println!("Strms  Blocks   Compressed Uncompressed  Ratio  Check   Filename");
        if self.files.is_empty() {
            let mut compressed = Vec::new();
            io::stdin().read_to_end(&mut compressed)?;
            print_xz_listing("-", &compressed)?;
            return Ok(());
        }
        for file in &self.files {
            let compressed = if file == OsStr::new("-") {
                let mut compressed = Vec::new();
                io::stdin().read_to_end(&mut compressed)?;
                compressed
            } else {
                fs::read(file)?
            };
            print_xz_listing(&file.to_string_lossy(), &compressed)?;
        }
        Ok(())
    }
}

fn parse_threads(value: &OsStr) -> io::Result<u32> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| invalid(format!("invalid thread count: {}", value.to_string_lossy())))
}

fn print_xz_listing(name: &str, compressed: &[u8]) -> io::Result<()> {
    let mut decoder = XzReader::new(compressed, true);
    let uncompressed = io::copy(&mut decoder, &mut io::sink())?;
    let ratio = if uncompressed == 0 {
        0.0
    } else {
        compressed.len() as f64 / uncompressed as f64
    };
    println!(
        "{:>5} {:>7} {:>12} {:>12} {:>6.3}  CRC64   {name}",
        1,
        1,
        compressed.len(),
        uncompressed,
        ratio
    );
    Ok(())
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
            Format::Bzip2 => ".bz2",
            Format::Xz => ".xz",
        });
        return Ok(output.into());
    }
    let suffix = input
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    let accepted = match format {
        Format::Bzip2 => matches!(suffix, "bz2" | "bz" | "tbz" | "tbz2"),
        Format::Xz => suffix == "xz",
    };
    if !accepted {
        return Err(invalid(format!(
            "{}: unknown suffix -- ignored",
            input.display()
        )));
    }
    let mut output = input.to_path_buf();
    if matches!(suffix, "tbz" | "tbz2") {
        output.set_extension("tar");
    } else {
        output.set_extension("");
    }
    Ok(output)
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
    }

    #[test]
    fn unknown_decompression_suffix_is_rejected() {
        assert!(output_path(Path::new("data.bin"), true, Format::Xz).is_err());
    }
}
