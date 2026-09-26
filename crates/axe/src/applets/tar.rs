use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use bzip2::Compression as Bzip2Compression;
use bzip2::read::MultiBzDecoder;
use bzip2::write::BzEncoder;
use flate2::Compression as GzipCompression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use lzma_rust2::{XzOptions, XzReader, XzWriter};

pub fn tar(args: Vec<OsString>) -> i32 {
    match Options::parse(args).and_then(Options::run) {
        Ok(()) => 0,
        Err(error)
            if error.kind() == io::ErrorKind::BrokenPipe
                || (error.kind() == io::ErrorKind::Interrupted && error.to_string().is_empty()) =>
        {
            0
        }
        Err(error) => {
            eprintln!("tar: {error}");
            2
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    Create,
    Extract,
    List,
}

#[derive(Clone, Copy)]
enum Compression {
    Gzip,
    Bzip2,
    Xz,
}

struct Options {
    action: Action,
    compression: Option<Compression>,
    archive: Option<OsString>,
    directory: Option<PathBuf>,
    operands: Vec<OsString>,
    verbose: bool,
    excludes: Vec<OsString>,
    preserve_permissions: Option<bool>,
    preserve_ownerships: Option<bool>,
    preserve_mtime: bool,
    overwrite: bool,
    ignore_zeros: bool,
}

impl Options {
    fn parse(args: Vec<OsString>) -> io::Result<Self> {
        let mut action = None;
        let mut compression = None;
        let mut archive = None;
        let mut directory = None;
        let mut operands = Vec::new();
        let mut verbose = false;
        let mut excludes = Vec::new();
        let mut preserve_permissions = None;
        let mut preserve_ownerships = None;
        let mut preserve_mtime = true;
        let mut overwrite = true;
        let mut ignore_zeros = false;
        let mut iter = args.into_iter();
        let _name = iter.next();

        while let Some(arg) = iter.next() {
            let Some(text) = arg.to_str() else {
                operands.push(arg);
                continue;
            };

            if text == "--" {
                operands.extend(iter);
                break;
            }
            if text == "--help" {
                println!(
                    "Usage: tar (-c|--create)|(-x|--extract)|(-t|--list) \
                     [-z|-j|-J] [-v] [-f ARCHIVE] [-C DIR] [FILE...]"
                );
                return Err(done());
            }
            if text == "--version" {
                println!("tar (axe) {}", env!("CARGO_PKG_VERSION"));
                return Err(done());
            }

            match text {
                "--create" => set_action(&mut action, Action::Create)?,
                "--extract" | "--get" => set_action(&mut action, Action::Extract)?,
                "--list" => set_action(&mut action, Action::List)?,
                "--gzip" | "--gunzip" | "--ungzip" => compression = Some(Compression::Gzip),
                "--bzip2" => compression = Some(Compression::Bzip2),
                "--xz" => compression = Some(Compression::Xz),
                "--verbose" => verbose = true,
                "--same-permissions" | "--preserve-permissions" => {
                    preserve_permissions = Some(true);
                }
                "--no-same-permissions" => preserve_permissions = Some(false),
                "--same-owner" => preserve_ownerships = Some(true),
                "--no-same-owner" => preserve_ownerships = Some(false),
                "--no-mtime" => preserve_mtime = false,
                "--keep-old-files" => overwrite = false,
                "--overwrite" => overwrite = true,
                "--ignore-zeros" => ignore_zeros = true,
                "-f" | "--file" => {
                    archive = Some(
                        iter.next()
                            .ok_or_else(|| invalid("-f requires an archive"))?,
                    );
                }
                "-C" | "--directory" => {
                    directory = Some(PathBuf::from(
                        iter.next()
                            .ok_or_else(|| invalid("-C requires a directory"))?,
                    ));
                }
                "--exclude" => {
                    excludes.push(
                        iter.next()
                            .ok_or_else(|| invalid("--exclude requires a pattern"))?,
                    );
                }
                _ if text.starts_with("--file=") => {
                    archive = Some(text["--file=".len()..].into());
                }
                _ if text.starts_with("--directory=") => {
                    directory = Some(text["--directory=".len()..].into());
                }
                _ if text.starts_with("--exclude=") => {
                    excludes.push(text["--exclude=".len()..].into());
                }
                _ if text.starts_with('-') && text != "-" => {
                    let mut chars = text[1..].chars().peekable();
                    while let Some(flag) = chars.next() {
                        match flag {
                            'c' => set_action(&mut action, Action::Create)?,
                            'x' => set_action(&mut action, Action::Extract)?,
                            't' => set_action(&mut action, Action::List)?,
                            'z' => compression = Some(Compression::Gzip),
                            'j' => compression = Some(Compression::Bzip2),
                            'J' => compression = Some(Compression::Xz),
                            'v' => verbose = true,
                            'p' => preserve_permissions = Some(true),
                            'f' => {
                                let rest: String = chars.collect();
                                archive = Some(if rest.is_empty() {
                                    iter.next()
                                        .ok_or_else(|| invalid("-f requires an archive"))?
                                } else {
                                    rest.into()
                                });
                                break;
                            }
                            _ => return Err(invalid(format!("unsupported option -{flag}"))),
                        }
                    }
                }
                _ => operands.push(arg),
            }
        }

        let action = action.ok_or_else(|| invalid("one of -c, -x, or -t is required"))?;
        Ok(Self {
            action,
            compression,
            archive,
            directory,
            operands,
            verbose,
            excludes,
            preserve_permissions,
            preserve_ownerships,
            preserve_mtime,
            overwrite,
            ignore_zeros,
        })
    }

    fn run(mut self) -> io::Result<()> {
        if let Some(archive) = self
            .archive
            .as_ref()
            .filter(|path| path.as_os_str() != OsStr::new("-"))
            .filter(|path| !Path::new(path).is_absolute())
        {
            self.archive = Some(std::env::current_dir()?.join(archive).into_os_string());
        }
        if let Some(directory) = &self.directory {
            std::env::set_current_dir(directory)?;
        }

        match self.action {
            Action::Create => self.create(),
            Action::Extract => self.extract(false),
            Action::List => self.extract(true),
        }
    }

    fn create(self) -> io::Result<()> {
        let output: Box<dyn Write> = match self.archive.as_deref() {
            Some(path) if path != OsStr::new("-") => Box::new(File::create(path)?),
            _ => Box::new(io::stdout().lock()),
        };

        match self.compression {
            Some(Compression::Gzip) => {
                let encoder = GzEncoder::new(output, GzipCompression::default());
                let mut encoder =
                    build_archive(encoder, self.operands, self.verbose, &self.excludes)?;
                encoder.try_finish()
            }
            Some(Compression::Bzip2) => {
                let encoder = BzEncoder::new(output, Bzip2Compression::default());
                let mut output =
                    build_archive(encoder, self.operands, self.verbose, &self.excludes)?
                        .finish()?;
                output.flush()
            }
            Some(Compression::Xz) => {
                let encoder =
                    XzWriter::new(output, XzOptions::with_preset(6)).map_err(io::Error::other)?;
                let mut output =
                    build_archive(encoder, self.operands, self.verbose, &self.excludes)?
                        .finish()
                        .map_err(io::Error::other)?;
                output.flush()
            }
            None => {
                let mut output =
                    build_archive(output, self.operands, self.verbose, &self.excludes)?;
                output.flush()
            }
        }
    }

    fn extract(self, list: bool) -> io::Result<()> {
        let input: Box<dyn Read> = match self.archive.as_deref() {
            Some(path) if path != OsStr::new("-") => Box::new(File::open(path)?),
            _ => Box::new(io::stdin().lock()),
        };
        let mut input = BufReader::new(input);
        let compression = self
            .compression
            .or_else(|| detect_compression(input.fill_buf().ok()?));
        let input: Box<dyn Read> = match compression {
            Some(Compression::Gzip) => Box::new(MultiGzDecoder::new(input)),
            Some(Compression::Bzip2) => Box::new(MultiBzDecoder::new(input)),
            Some(Compression::Xz) => Box::new(XzReader::new(input, true)),
            None => Box::new(input),
        };

        let mut archive = tar::Archive::new(input);
        if let Some(preserve) = self.preserve_permissions {
            archive.set_preserve_permissions(preserve);
        }
        if let Some(preserve) = self.preserve_ownerships {
            archive.set_preserve_ownerships(preserve);
        }
        archive.set_preserve_mtime(self.preserve_mtime);
        archive.set_overwrite(self.overwrite);
        archive.set_ignore_zeros(self.ignore_zeros);

        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        let mut directories = Vec::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            if is_excluded(&path, &self.excludes) {
                continue;
            }
            if list {
                if self.verbose {
                    write_verbose_entry(&mut stdout, &entry, &path)?;
                } else {
                    writeln!(stdout, "{}", path.display())?;
                }
            } else {
                if self.verbose {
                    writeln!(stdout, "{}", path.display())?;
                }
                if entry.header().entry_type().is_dir() {
                    directories.push(entry);
                } else {
                    entry.unpack_in(".")?;
                }
            }
        }

        // Apply restrictive directory modes only after their contents exist.
        directories.sort_by(|left, right| right.path_bytes().cmp(&left.path_bytes()));
        for mut directory in directories {
            directory.unpack_in(".")?;
        }

        Ok(())
    }
}

fn build_archive<W: Write>(
    output: W,
    operands: Vec<OsString>,
    verbose: bool,
    excludes: &[OsString],
) -> io::Result<W> {
    let mut archive = tar::Builder::new(output);
    archive.follow_symlinks(false);
    for operand in operands {
        let path = Path::new(&operand);
        append_tree(&mut archive, path, path, verbose, excludes)?;
    }

    archive.finish()?;
    archive.into_inner()
}

fn append_tree<W: Write>(
    archive: &mut tar::Builder<W>,
    source: &Path,
    archive_path: &Path,
    verbose: bool,
    excludes: &[OsString],
) -> io::Result<()> {
    if is_excluded(archive_path, excludes) {
        return Ok(());
    }

    let metadata = fs::symlink_metadata(source)?;
    if verbose {
        if metadata.is_dir() {
            eprintln!("{}/", archive_path.display());
        } else {
            eprintln!("{}", archive_path.display());
        }
    }

    if metadata.is_dir() {
        archive.append_dir(archive_path, source)?;
        for child in fs::read_dir(source)? {
            let child = child?;
            append_tree(
                archive,
                &child.path(),
                &archive_path.join(child.file_name()),
                verbose,
                excludes,
            )?;
        }
    } else {
        archive.append_path_with_name(source, archive_path)?;
    }
    Ok(())
}

fn detect_compression(prefix: &[u8]) -> Option<Compression> {
    if prefix.starts_with(&[0x1f, 0x8b]) {
        Some(Compression::Gzip)
    } else if prefix.starts_with(b"BZh") {
        Some(Compression::Bzip2)
    } else if prefix.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Some(Compression::Xz)
    } else {
        None
    }
}

fn is_excluded(path: &Path, patterns: &[OsString]) -> bool {
    let path = path.as_os_str().as_encoded_bytes();
    patterns
        .iter()
        .any(|pattern| wildcard_match(pattern.as_encoded_bytes(), path))
}

fn wildcard_match(pattern: &[u8], value: &[u8]) -> bool {
    let (mut pattern_index, mut value_index) = (0, 0);
    let (mut star, mut star_value) = (None, 0);
    while value_index < value.len() {
        if pattern.get(pattern_index) == Some(&value[value_index])
            || pattern.get(pattern_index) == Some(&b'?')
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern.get(pattern_index) == Some(&b'*') {
            star = Some(pattern_index);
            pattern_index += 1;
            star_value = value_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            star_value += 1;
            value_index = star_value;
        } else {
            return false;
        }
    }
    pattern[pattern_index..].iter().all(|byte| *byte == b'*')
}

fn write_verbose_entry<W: Write, R: Read>(
    output: &mut W,
    entry: &tar::Entry<'_, R>,
    path: &Path,
) -> io::Result<()> {
    let header = entry.header();
    writeln!(
        output,
        "{:06o} {}/{} {:>10} {:>10} {}",
        header.mode()?,
        header.uid()?,
        header.gid()?,
        header.size()?,
        header.mtime()?,
        path.display()
    )
}

fn set_action(slot: &mut Option<Action>, action: Action) -> io::Result<()> {
    if slot.is_some() {
        return Err(invalid("multiple operation modes specified"));
    }

    *slot = Some(action);
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn done() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "")
}
