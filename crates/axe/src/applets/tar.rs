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
    strip_components: usize,
    to_stdout: bool,
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
        let mut strip_components = 0usize;
        let mut to_stdout = false;
        let mut iter = args.into_iter();
        let _name = iter.next();
        let mut old_style_allowed = true;

        while let Some(arg) = iter.next() {
            let Some(argument) = arg.to_str() else {
                old_style_allowed = false;
                operands.push(arg);
                continue;
            };
            // GNU and busybox tar accept a dashless leading argument such as
            // `tar czf archive.tar.gz` as a flag cluster.
            let converted;
            let text: &str = if old_style_allowed
                && !argument.starts_with('-')
                && is_old_style_cluster(argument)
            {
                converted = format!("-{argument}");
                &converted
            } else {
                argument
            };
            old_style_allowed = false;
            let arg = OsString::from(text);

            if text == "--" {
                operands.extend(iter);
                break;
            }
            if text == "--help" {
                println!(
                    "Usage: tar (-c|--create)|(-x|--extract)|(-t|--list) \
                     [-z|-j|-J] [-v] [-O] [--strip-components N] [-f ARCHIVE] [-C DIR] [FILE...]"
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
                "--to-stdout" => to_stdout = true,
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
                "-O" => to_stdout = true,
                "-C" | "--directory" => {
                    directory = Some(PathBuf::from(
                        iter.next()
                            .ok_or_else(|| invalid("-C requires a directory"))?,
                    ));
                }
                "--strip-components" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| invalid("--strip-components requires a count"))?;
                    strip_components = parse_strip_components(&value)?;
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
                _ if text.starts_with("--strip-components=") => {
                    strip_components =
                        parse_strip_components(OsStr::new(&text["--strip-components=".len()..]))?;
                }
                _ if text.starts_with("--exclude=") => {
                    excludes.push(text["--exclude=".len()..].into());
                }
                _ if text.starts_with("--") => {
                    return Err(invalid(format!("unsupported option '{text}'")));
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
                            'O' => to_stdout = true,
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
            strip_components,
            to_stdout,
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

        if list {
            // GNU tar sizes the size column to the largest entry, with a
            // fixed minimum; the width needs all entries up front.
            let mut listed: Vec<(tar::Entry<'_, Box<dyn Read>>, PathBuf)> = Vec::new();
            for entry in archive.entries()? {
                let entry = entry?;
                let path = entry.path()?.into_owned();
                if is_excluded(&path, &self.excludes) || !member_selected(&path, &self.operands) {
                    continue;
                }
                if let Some(path) = strip_components(&path, self.strip_components) {
                    listed.push((entry, path));
                }
            }
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            let size_width = listed
                .iter()
                .map(|(entry, _)| entry.header().size().map(|size| size.to_string().len()))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .max()
                .unwrap_or(1)
                .max(15);
            for (entry, path) in listed {
                if self.verbose {
                    write_verbose_entry(&mut stdout, &entry, &path, size_width)?;
                } else {
                    writeln!(stdout, "{}", path.display())?;
                }
            }
            return Ok(());
        }

        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        let mut directories: Vec<(tar::Entry<'_, Box<dyn Read>>, PathBuf)> = Vec::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            if is_excluded(&path, &self.excludes) || !member_selected(&path, &self.operands) {
                continue;
            }
            let Some(path) = strip_components(&path, self.strip_components) else {
                continue;
            };
            if self.to_stdout {
                if !entry.header().entry_type().is_dir() {
                    io::copy(&mut entry, &mut stdout)?;
                }
            } else {
                if self.verbose {
                    writeln!(stdout, "{}", path.display())?;
                }
                if entry.header().entry_type().is_dir() {
                    directories.push((entry, path));
                } else if !path.as_os_str().is_empty() {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    entry.unpack(&path)?;
                }
            }
        }

        // Apply restrictive directory modes only after their contents exist.
        directories.sort_by(|left, right| right.1.cmp(&left.1));
        for (mut directory, path) in directories {
            if !path.as_os_str().is_empty() {
                directory.unpack(&path)?;
            }
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
    size_width: usize,
) -> io::Result<()> {
    let header = entry.header();
    let mut name = path.display().to_string();
    if header.entry_type().is_symlink()
        && let Ok(Some(target)) = entry.link_name()
    {
        name.push_str(" -> ");
        name.push_str(&target.display().to_string());
    }
    writeln!(
        output,
        "{} {}/{} {:>width$} {} {}",
        mode_string(header)?,
        stored_name(header.username_bytes().unwrap_or_default(), header.uid()?),
        stored_name(header.groupname_bytes().unwrap_or_default(), header.gid()?),
        header.size()?,
        local_datetime(header.mtime()?),
        name,
        width = size_width
    )
}

/// GNU tar lists the owner name stored in the archive, falling back to the
/// numeric id when the header carries none.
fn stored_name(stored: &[u8], id: u64) -> String {
    let stored = String::from_utf8_lossy(stored);
    if stored.is_empty() {
        id.to_string()
    } else {
        stored.into_owned()
    }
}

/// GNU-style permission string with the entry-type character, for example
/// `drwxr-xr-x` or `-rw-r--r--`.
fn mode_string(header: &tar::Header) -> io::Result<String> {
    // GNU tar stores the file type bits in the mode field and lists only the
    // permission bits.
    let mode = header.mode()? & 0o7777;
    let type_char = match header.entry_type() {
        tar::EntryType::Directory => 'd',
        tar::EntryType::Symlink => 'l',
        tar::EntryType::Char => 'c',
        tar::EntryType::Block => 'b',
        tar::EntryType::Fifo => 'p',
        _ => '-',
    };
    let mut text = String::with_capacity(10);
    text.push(type_char);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        let special = (mode >> (shift + 9)) & 1 == 1;
        let (read, write) = (bits & 4 != 0, bits & 2 != 0);
        let execute = bits & 1 != 0;
        text.push(if read { 'r' } else { '-' });
        text.push(if write { 'w' } else { '-' });
        match (shift, special, execute) {
            (0, true, true) => text.push('t'),
            (0, true, false) => text.push('T'),
            (3 | 6, true, true) => text.push('s'),
            (3 | 6, true, false) => text.push('S'),
            (_, _, execute) => text.push(if execute { 'x' } else { '-' }),
        }
    }
    Ok(text)
}

/// Reports whether an archive member is selected by the name operands, using
/// GNU tar's rule: an exact match or a matching parent directory.
fn member_selected(path: &Path, operands: &[OsString]) -> bool {
    operands.is_empty()
        || operands
            .iter()
            .any(|operand| path == Path::new(operand) || path.strip_prefix(operand).is_ok())
}

/// Local-time `YYYY-MM-DD HH:MM` for the archive timestamp.
fn local_datetime(seconds: u64) -> String {
    let time = seconds as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `tm` is a valid out-pointer for localtime_r.
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return seconds.to_string();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

/// Reports whether a dashless argument looks like an old-style flag cluster
/// such as `czf` in `tar czf archive.tar.gz`.
fn is_old_style_cluster(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|flag| {
            matches!(
                flag,
                'c' | 'x' | 't' | 'z' | 'j' | 'J' | 'v' | 'p' | 'O' | 'f'
            )
        })
}

fn parse_strip_components(value: &OsStr) -> io::Result<usize> {
    let value = value
        .to_str()
        .ok_or_else(|| invalid("strip-components count must be valid UTF-8"))?;
    value
        .parse()
        .map_err(|_| invalid(format!("invalid strip-components count '{value}'")))
}

/// Drops the leading `count` path components; returns `None` when nothing
/// would remain, matching GNU tar's `--strip-components` behaviour.
fn strip_components(path: &Path, count: usize) -> Option<PathBuf> {
    if count == 0 {
        return Some(path.to_path_buf());
    }
    let mut components = path.components().peekable();
    for _ in 0..count {
        components.next()?;
    }
    let mut stripped = PathBuf::new();
    for component in components {
        stripped.push(component);
    }
    (stripped.as_os_str() != ".").then_some(stripped)
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
