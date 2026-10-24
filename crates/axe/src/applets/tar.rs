use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

use bzip2::Compression as Bzip2Compression;
use bzip2::read::MultiBzDecoder;
use bzip2::write::BzEncoder;
use chrono::{Local, TimeZone};
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

#[derive(Clone, Copy, Eq, PartialEq)]
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
        let mut first = true;

        while let Some(arg) = iter.next() {
            let Some(text) = arg.to_str() else {
                first = false;
                operands.push(arg);
                continue;
            };
            let old_style = first && !text.starts_with('-');
            first = false;

            if text == "--" {
                operands.extend(iter);
                break;
            }
            if text.starts_with("--") {
                let (option, value) = text
                    .split_once('=')
                    .map_or((text, None), |(option, value)| (option, Some(value)));
                match option {
                    "--file" => {
                        archive = Some(long_argument(option, value, &mut iter)?);
                        continue;
                    }
                    "--directory" => {
                        directory = Some(PathBuf::from(long_argument(option, value, &mut iter)?));
                        continue;
                    }
                    "--strip-components" => {
                        strip_components =
                            parse_strip_components(&long_argument(option, value, &mut iter)?)?;
                        continue;
                    }
                    "--exclude" => {
                        excludes.push(long_argument(option, value, &mut iter)?);
                        continue;
                    }
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
                    "--help" | "--version" => {
                        reject_option_value(option, value)?;
                        if option == "--help" {
                            println!(
                                "Usage: tar (-c|--create)|(-x|--extract)|(-t|--list) \
                                 [-z|-j|-J] [-v] [-O] [--strip-components N] \
                                 [-f ARCHIVE] [-C DIR] [FILE...]"
                            );
                        } else {
                            println!("tar (axe) {}", env!("CARGO_PKG_VERSION"));
                        }
                        return Err(done());
                    }
                    _ => return Err(invalid(format!("unrecognized option '{text}'"))),
                }
                reject_option_value(option, value)?;
            } else if old_style || (text.starts_with('-') && text != "-") {
                let mut chars = if old_style { text } else { &text[1..] }.chars();
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
                        'f' | 'C' => {
                            // Old-style argument-taking options consume the next word,
                            // not the remaining letters in the option cluster.
                            let value = if !old_style && !chars.as_str().is_empty() {
                                OsString::from(chars.as_str())
                            } else {
                                iter.next().ok_or_else(|| {
                                    invalid(if old_style {
                                        format!("Old option '{flag}' requires an argument.")
                                    } else {
                                        format!("option requires an argument -- '{flag}'")
                                    })
                                })?
                            };
                            if flag == 'f' {
                                archive = Some(value);
                            } else {
                                directory = Some(PathBuf::from(value));
                            }
                            if !old_style {
                                break;
                            }
                        }
                        _ => return Err(invalid(format!("invalid option -- '{flag}'"))),
                    }
                }
            } else {
                operands.push(arg);
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

        let operands: Vec<PathBuf> = self
            .operands
            .iter()
            .map(|operand| {
                Path::new(operand)
                    .components()
                    .filter(|part| !matches!(part, Component::Prefix(_) | Component::RootDir))
                    .collect()
            })
            .collect();
        let mut matched = vec![false; operands.len()];
        let root = if list || self.to_stdout {
            None
        } else {
            Some(fs::canonicalize(".")?)
        };
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        let mut ugswidth = 19;
        let mut directories = Vec::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;
            if !member_selected(&path, &operands, &mut matched)
                || is_excluded(&path, &self.excludes)
            {
                continue;
            }
            if list {
                if self.verbose {
                    write_verbose_entry(&mut stdout, &entry, &path, &mut ugswidth)?;
                } else {
                    write_escaped(&mut stdout, entry.path_bytes().as_ref())?;
                    writeln!(stdout)?;
                }
                continue;
            }

            let Some(path) = extraction_path(&path, self.strip_components)? else {
                continue;
            };
            if self.to_stdout {
                if !entry_is_directory(&entry) {
                    io::copy(&mut entry, &mut stdout)?;
                }
            } else {
                if self.verbose {
                    write_escaped(&mut stdout, path.as_os_str().as_encoded_bytes())?;
                    writeln!(stdout)?;
                }
                if entry_is_directory(&entry) {
                    if let Some(root) = &root {
                        let destination = extraction_destination(root, &path)?;
                        if let Err(error) = fs::create_dir(&destination)
                            && (error.kind() != io::ErrorKind::AlreadyExists
                                || !fs::symlink_metadata(&destination)?.is_dir())
                        {
                            return Err(error);
                        }
                    }
                    directories.push((entry, path));
                } else if let Some(root) = &root {
                    unpack_entry(&mut entry, root, &path, self.strip_components)?;
                }
            }
        }

        // Apply restrictive directory modes only after their contents exist.
        // Revalidate parents here: later archive entries can introduce symlinks.
        directories.sort_by(|left, right| right.1.cmp(&left.1));
        for (mut directory, path) in directories {
            if let Some(root) = &root {
                unpack_entry(&mut directory, root, &path, self.strip_components)?;
            }
        }

        let mut missing = false;
        for (operand, matched) in operands.iter().zip(matched) {
            if !matched {
                eprintln!("tar: {}: Not found in archive", operand.display());
                missing = true;
            }
        }
        if missing {
            Err(invalid(
                "Exiting with failure status due to previous errors",
            ))
        } else {
            Ok(())
        }
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
    ugswidth: &mut usize,
) -> io::Result<()> {
    let header = entry.header();
    let kind = header.entry_type();
    let user = stored_name(header.username_bytes().unwrap_or_default(), header.uid()?);
    let group = stored_name(header.groupname_bytes().unwrap_or_default(), header.gid()?);
    let size = if kind.is_character_special() || kind.is_block_special() {
        format!(
            "{},{}",
            header.device_major()?.unwrap_or(0),
            header.device_minor()?.unwrap_or(0)
        )
    } else {
        entry.size().to_string()
    };
    let pad = user.len() + group.len() + 2 + size.len();
    *ugswidth = (*ugswidth).max(pad);
    write!(
        output,
        "{} {user}/{group} {size:>width$} {} ",
        mode_string(header)?,
        local_datetime(header.mtime()?),
        width = *ugswidth - pad + size.len()
    )?;
    write_escaped(output, path.as_os_str().as_encoded_bytes())?;
    if kind.is_symlink() || kind.is_hard_link() {
        output.write_all(if kind.is_symlink() {
            b" -> "
        } else {
            b" link to "
        })?;
        if let Some(target) = entry.link_name_bytes() {
            write_escaped(output, &target)?;
        }
    }
    writeln!(output)
}

/// GNU tar lists the owner name stored in the archive, falling back to the
/// numeric id when the header carries none.
fn stored_name(stored: &[u8], id: u64) -> Cow<'_, str> {
    if stored.is_empty() {
        Cow::Owned(id.to_string())
    } else {
        String::from_utf8_lossy(stored)
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
        tar::EntryType::Link => 'h',
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
        let special = mode & (1 << (shift / 3 + 9)) != 0;
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

fn member_selected(path: &Path, operands: &[PathBuf], matched: &mut [bool]) -> bool {
    let mut selected = operands.is_empty();
    for (operand, matched) in operands.iter().zip(matched) {
        if path.starts_with(operand) {
            *matched = true;
            selected = true;
        }
    }
    selected
}

fn local_datetime(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| Local.timestamp_opt(seconds, 0).single())
        .map_or_else(
            || seconds.to_string(),
            |time| time.format("%Y-%m-%d %H:%M").to_string(),
        )
}

fn long_argument(
    option: &str,
    value: Option<&str>,
    args: &mut impl Iterator<Item = OsString>,
) -> io::Result<OsString> {
    value
        .map(OsString::from)
        .or_else(|| args.next())
        .ok_or_else(|| invalid(format!("option '{option}' requires an argument")))
}

fn reject_option_value(option: &str, value: Option<&str>) -> io::Result<()> {
    if value.is_some() {
        Err(invalid(format!(
            "option '{option}' doesn't allow an argument"
        )))
    } else {
        Ok(())
    }
}

fn parse_strip_components(value: &OsStr) -> io::Result<usize> {
    let value = value
        .to_str()
        .ok_or_else(|| invalid("strip-components count must be valid UTF-8"))?;
    value
        .parse()
        .map_err(|_| invalid(format!("invalid strip-components count '{value}'")))
}

fn extraction_path(path: &Path, count: usize) -> io::Result<Option<PathBuf>> {
    let mut stripped = PathBuf::new();
    let mut remaining = count;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                return Err(invalid(format!(
                    "{}: Member name contains '..'",
                    path.display()
                )));
            }
            Component::Normal(part) if remaining == 0 => stripped.push(part),
            Component::Normal(_) => remaining -= 1,
        }
    }
    Ok((!stripped.as_os_str().is_empty()).then_some(stripped))
}

fn entry_is_directory<R: Read>(entry: &tar::Entry<'_, R>) -> bool {
    entry.header().entry_type().is_dir()
        || (entry.header().as_ustar().is_none() && entry.path_bytes().ends_with(b"/"))
}

fn unpack_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    root: &Path,
    path: &Path,
    strip: usize,
) -> io::Result<()> {
    if strip == 0 {
        entry.unpack_in(root)?;
        return Ok(());
    }

    let destination = extraction_destination(root, path)?;
    if entry.header().entry_type().is_hard_link() {
        let target = entry
            .link_name()?
            .ok_or_else(|| invalid("hard link has no target"))?;
        let target = extraction_path(&target, strip)?
            .ok_or_else(|| invalid("hard link target has no remaining components"))?;
        let target = validate_inside(root, &root.join(target))?;
        fs::hard_link(target, destination)?;
    } else {
        entry.unpack(destination)?;
    }
    Ok(())
}

fn extraction_destination(root: &Path, path: &Path) -> io::Result<PathBuf> {
    // Resolve each parent before creating its child, including existing symlinks.
    // This mirrors Entry::unpack_in's filesystem containment checks.
    let mut parent = root.to_path_buf();
    if let Some(components) = path.parent() {
        for component in components.components() {
            parent.push(component);
            match fs::create_dir(&parent) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            parent = validate_inside(root, &parent)?;
        }
    }
    let name = path
        .file_name()
        .ok_or_else(|| invalid("empty member name"))?;
    Ok(parent.join(name))
}

fn validate_inside(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let resolved = fs::canonicalize(path)?;
    if !resolved.starts_with(root) {
        return Err(invalid(format!(
            "trying to unpack outside of destination path: {}",
            path.display()
        )));
    }
    Ok(resolved)
}

fn write_escaped<W: Write>(output: &mut W, bytes: &[u8]) -> io::Result<()> {
    let mut start = 0;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if (b' '..=b'~').contains(&byte) && byte != b'\\' {
            continue;
        }
        output.write_all(&bytes[start..index])?;
        match byte {
            b'\x07' => output.write_all(b"\\a")?,
            b'\x08' => output.write_all(b"\\b")?,
            b'\t' => output.write_all(b"\\t")?,
            b'\n' => output.write_all(b"\\n")?,
            b'\x0b' => output.write_all(b"\\v")?,
            b'\x0c' => output.write_all(b"\\f")?,
            b'\r' => output.write_all(b"\\r")?,
            b'\\' => output.write_all(b"\\\\")?,
            _ => write!(output, "\\{byte:03o}")?,
        }
        start = index + 1;
    }
    output.write_all(&bytes[start..])
}

fn set_action(slot: &mut Option<Action>, action: Action) -> io::Result<()> {
    if slot.is_some_and(|current| current != action) {
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
