use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;

pub fn gzip(args: Vec<OsString>) -> i32 {
    let name = args
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .and_then(OsStr::to_str)
        .unwrap_or("gzip")
        .to_owned();

    match Options::parse(&name, args.into_iter().skip(1)).and_then(Options::run) {
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
    decompress: bool,
    test: bool,
    stdout: bool,
    keep: bool,
    force: bool,
    files: Vec<OsString>,
    quiet: bool,
    verbose: bool,
    list: bool,
}

impl Options {
    fn parse(name: &str, args: impl Iterator<Item = OsString>) -> io::Result<Self> {
        let mut options = Self {
            program: name.into(),
            decompress: matches!(name, "gunzip" | "zcat"),
            test: false,
            stdout: name == "zcat",
            keep: false,
            force: false,
            quiet: false,
            verbose: false,
            list: false,
            files: Vec::new(),
        };

        let mut positional = false;
        for arg in args {
            let Some(text) = arg.to_str() else {
                options.files.push(arg);
                continue;
            };
            if positional {
                options.files.push(arg);
            } else if text == "--" {
                positional = true;
            } else if text == "--help" {
                println!("Usage: {} [-cdfkt] [FILE ...]", options.program);
                return Err(done());
            } else if text == "--version" {
                println!("{} (axe) {}", options.program, env!("CARGO_PKG_VERSION"));
                return Err(done());
            } else if text == "--test" {
                options.test = true;
                options.decompress = true;
            } else if text == "--quiet" {
                options.quiet = true;
            } else if text == "--verbose" {
                options.verbose = true;
            } else if text == "--list" {
                options.list = true;
                options.decompress = true;
            } else if text.starts_with('-') && text != "-" {
                for flag in text[1..].chars() {
                    match flag {
                        'c' => options.stdout = true,
                        'd' => options.decompress = true,
                        'f' => options.force = true,
                        'k' => options.keep = true,
                        't' => {
                            options.test = true;
                            options.decompress = true;
                        }
                        'n' => {}
                        'q' => options.quiet = true,
                        'v' => options.verbose = true,
                        'l' => {
                            options.list = true;
                            options.decompress = true;
                        }
                        _ => return Err(invalid(format!("unsupported option -{flag}"))),
                    }
                }
            } else {
                options.files.push(arg);
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
            let stdout = io::stdout();
            return if self.test {
                copy_decode(stdin.lock(), io::sink())
            } else if self.decompress {
                copy_decode(stdin.lock(), stdout.lock())
            } else {
                copy_encode(stdin.lock(), stdout.lock())
            };
        }

        for file in &self.files {
            if file == OsStr::new("-") {
                let stdin = io::stdin();
                let stdout = io::stdout();
                if self.test {
                    copy_decode(stdin.lock(), io::sink())?;
                } else if self.decompress {
                    copy_decode(stdin.lock(), stdout.lock())?;
                } else {
                    copy_encode(stdin.lock(), stdout.lock())?;
                }
                continue;
            }

            let input_path = Path::new(file);
            let input = File::open(input_path)?;
            if self.test {
                copy_decode(input, io::sink())?;
                continue;
            }

            if self.stdout {
                let stdout = io::stdout();
                if self.decompress {
                    copy_decode(input, stdout.lock())?;
                } else {
                    copy_encode(input, stdout.lock())?;
                }
                continue;
            }

            let output_path = output_path(input_path, self.decompress)?;

            let output = create_output(&output_path, self.force)?;
            let write_result = if self.decompress {
                copy_decode(input, output)
            } else {
                copy_encode(input, output)
            };

            if let Err(error) = write_result {
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

    fn list_contents(&self) -> io::Result<()> {
        println!("         compressed        uncompressed  ratio uncompressed_name");

        if self.files.is_empty() {
            let mut compressed = Vec::new();
            io::stdin().read_to_end(&mut compressed)?;
            print_gzip_listing("-", &compressed)?;
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
            let name = if file == OsStr::new("-") {
                "-".to_owned()
            } else {
                output_path(Path::new(file), true)?.display().to_string()
            };
            print_gzip_listing(&name, &compressed)?;
        }

        Ok(())
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

fn output_path(input: &Path, decompress: bool) -> io::Result<PathBuf> {
    if decompress {
        let name = input
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        let output_name = if let Some(stem) = name.strip_suffix(".tgz") {
            format!("{stem}.tar")
        } else if let Some(stem) = name.strip_suffix(".taz") {
            format!("{stem}.tar")
        } else if let Some(stem) = [".gz", "-gz", ".z", "-z", "_z"]
            .into_iter()
            .find_map(|suffix| name.strip_suffix(suffix))
        {
            stem.to_owned()
        } else {
            return Err(invalid(format!(
                "{}: unknown suffix -- ignored",
                input.display()
            )));
        };

        Ok(input.with_file_name(output_name))
    } else {
        let mut output = input.as_os_str().to_owned();
        output.push(".gz");
        Ok(output.into())
    }
}

fn copy_encode(input: impl Read, output: impl Write) -> io::Result<()> {
    let mut encoder = GzEncoder::new(output, Compression::default());
    let mut input = input;

    io::copy(&mut input, &mut encoder)?;
    encoder.try_finish()
}

fn copy_decode(input: impl Read, output: impl Write) -> io::Result<()> {
    let mut decoder = MultiGzDecoder::new(input);
    let mut output = output;

    io::copy(&mut decoder, &mut output)?;
    output.flush()
}

fn print_gzip_listing(name: &str, compressed: &[u8]) -> io::Result<()> {
    let mut decoder = MultiGzDecoder::new(compressed);
    let mut uncompressed = Vec::new();
    decoder.read_to_end(&mut uncompressed)?;

    let ratio = if uncompressed.is_empty() {
        0.0
    } else {
        100.0 - compressed.len() as f64 * 100.0 / uncompressed.len() as f64
    };
    println!(
        "{:>19} {:>19} {:>5.1}% {name}",
        compressed.len(),
        uncompressed.len(),
        ratio
    );
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn done() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "")
}
