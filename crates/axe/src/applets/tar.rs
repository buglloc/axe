use std::borrow::Cow;
#[cfg(unix)]
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
#[cfg(not(unix))]
use std::time::{Duration, UNIX_EPOCH};

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
    Zstd,
}

struct Operand {
    name: PathBuf,
    directory: usize,
    wildcards: bool,
}

struct Exclusion {
    name: OsString,
    wildcards: bool,
}

struct Options {
    action: Action,
    compression: Option<Compression>,
    archive: Option<OsString>,
    directories: Vec<PathBuf>,
    operands: Vec<Operand>,
    verbose: bool,
    excludes: Vec<Exclusion>,
    preserve_permissions: Option<bool>,
    preserve_ownerships: Option<bool>,
    preserve_mtime: bool,
    overwrite: bool,
    ignore_zeros: bool,
    strip_components: usize,
    to_stdout: bool,
    dereference: bool,
}

fn operand(name: OsString, directory: usize, wildcards: Option<bool>) -> Operand {
    Operand {
        name: PathBuf::from(name),
        directory,
        wildcards: wildcards.unwrap_or(false),
    }
}

impl Options {
    fn parse(args: Vec<OsString>) -> io::Result<Self> {
        let mut action = None;
        let mut compression = None;
        let mut archive = None;
        let mut directories = vec![PathBuf::from(".")];
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
        let mut auto_compress = false;
        let mut dereference = false;
        let mut wildcards = None;
        let mut iter = args.into_iter();
        let _name = iter.next();
        let mut first = true;

        while let Some(arg) = iter.next() {
            let Some(text) = arg.to_str() else {
                first = false;
                operands.push(operand(arg, directories.len() - 1, wildcards));
                continue;
            };
            let old_style = first && !text.starts_with('-');
            first = false;

            if text == "--" {
                operands.extend(iter.map(|name| operand(name, directories.len() - 1, wildcards)));
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
                        directories.push(PathBuf::from(long_argument(option, value, &mut iter)?));
                        continue;
                    }
                    "--strip-components" => {
                        strip_components =
                            parse_strip_components(&long_argument(option, value, &mut iter)?)?;
                        continue;
                    }
                    "--exclude" => {
                        excludes.push(Exclusion {
                            name: long_argument(option, value, &mut iter)?,
                            wildcards: wildcards.unwrap_or(true),
                        });
                        continue;
                    }
                    "--create" => set_action(&mut action, Action::Create)?,
                    "--extract" | "--get" => set_action(&mut action, Action::Extract)?,
                    "--list" => set_action(&mut action, Action::List)?,
                    "--gzip" | "--gunzip" | "--ungzip" => compression = Some(Compression::Gzip),
                    "--bzip2" => compression = Some(Compression::Bzip2),
                    "--xz" => compression = Some(Compression::Xz),
                    "--zstd" => compression = Some(Compression::Zstd),
                    "--auto-compress" => auto_compress = true,
                    "--no-auto-compress" => auto_compress = false,
                    "--dereference" => dereference = true,
                    "--wildcards" => wildcards = Some(true),
                    "--no-wildcards" => wildcards = Some(false),
                    "--verbose" => verbose = true,
                    "--to-stdout" => to_stdout = true,
                    "--same-permissions" | "--preserve-permissions" => {
                        preserve_permissions = Some(true);
                    }
                    "--no-same-permissions" => preserve_permissions = Some(false),
                    "--same-owner" => preserve_ownerships = Some(true),
                    "--no-same-owner" => preserve_ownerships = Some(false),
                    "--touch" | "--no-mtime" => preserve_mtime = false,
                    "--keep-old-files" => overwrite = false,
                    "--overwrite" => overwrite = true,
                    "--ignore-zeros" => ignore_zeros = true,
                    "--help" | "--version" => {
                        reject_option_value(option, value)?;
                        if option == "--help" {
                            println!(
                                "Usage: tar (-c|--create)|(-x|--extract)|(-t|--list) \
                                 [-z|-j|-J|--zstd|-a] [-h] [-v] [-m] [-O] \
                                 [--strip-components N] [-f ARCHIVE] [-C DIR] [FILE...]"
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
                        'a' => auto_compress = true,
                        'h' => dereference = true,
                        'v' => verbose = true,
                        'p' => preserve_permissions = Some(true),
                        'm' => preserve_mtime = false,
                        'O' => to_stdout = true,
                        'k' => overwrite = false,
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
                                directories.push(PathBuf::from(value));
                            }
                            if !old_style {
                                break;
                            }
                        }
                        _ => return Err(invalid(format!("invalid option -- '{flag}'"))),
                    }
                }
            } else {
                operands.push(operand(arg, directories.len() - 1, wildcards));
            }
        }

        let action = action.ok_or_else(|| invalid("one of -c, -x, or -t is required"))?;
        if action == Action::Create && operands.is_empty() {
            return Err(invalid("Cowardly refusing to create an empty archive"));
        }
        if action == Action::Create && auto_compress {
            compression = archive.as_deref().and_then(compression_from_suffix);
        }
        Ok(Self {
            action,
            compression,
            archive,
            directories,
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
            dereference,
        })
    }

    fn run(mut self) -> io::Result<()> {
        let mut current = std::env::current_dir()?;
        for directory in &mut self.directories {
            current = fs::canonicalize(current.join(&*directory))?;
            if !current.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("{}: Not a directory", current.display()),
                ));
            }
            directory.clone_from(&current);
        }
        if self.action != Action::Create {
            for operand in &mut self.operands {
                operand.name = operand
                    .name
                    .components()
                    .filter(|part| !matches!(part, Component::Prefix(_) | Component::RootDir))
                    .collect();
            }
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
                let mut encoder = build_archive(encoder, &self)?;
                encoder.try_finish()
            }
            Some(Compression::Bzip2) => {
                let encoder = BzEncoder::new(output, Bzip2Compression::default());
                let mut output = build_archive(encoder, &self)?.finish()?;
                output.flush()
            }
            Some(Compression::Xz) => {
                let encoder =
                    XzWriter::new(output, XzOptions::with_preset(6)).map_err(io::Error::other)?;
                let mut output = build_archive(encoder, &self)?
                    .finish()
                    .map_err(io::Error::other)?;
                output.flush()
            }
            Some(Compression::Zstd) => {
                let encoder = zstd::stream::write::Encoder::new(output, 3)?;
                let mut output = build_archive(encoder, &self)?.finish()?;
                output.flush()
            }
            None => {
                let mut output = build_archive(output, &self)?;
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
            Some(Compression::Zstd) => Box::new(zstd::stream::read::Decoder::new(input)?),
            None => Box::new(input),
        };

        let mut archive = tar::Archive::new(input);
        if !list && !self.to_stdout {
            let preserve = self
                .preserve_permissions
                .unwrap_or_else(root_permissions_default);
            archive.set_preserve_permissions(preserve);
            if !preserve {
                archive.set_mask(current_umask());
            }
        }
        if let Some(preserve) = self.preserve_ownerships {
            archive.set_preserve_ownerships(preserve);
        }
        archive.set_preserve_mtime(self.preserve_mtime);
        archive.set_overwrite(self.overwrite);
        archive.set_ignore_zeros(self.ignore_zeros);

        let mut matched = vec![false; self.operands.len()];
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        let mut ugswidth = 19;
        let mut directories = Vec::new();
        let mut failed = false;
        for entry in archive.entries()? {
            let mut entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    eprintln!("tar: {error}");
                    failed = true;
                    break;
                }
            };
            let path = match entry.path() {
                Ok(path) => path,
                Err(error) => {
                    eprintln!("tar: {error}");
                    failed = true;
                    continue;
                }
            };
            let Some(directory) = member_selected(
                &path,
                &self.operands,
                &mut matched,
                self.directories.len() - 1,
            ) else {
                continue;
            };
            if is_excluded(&path, &self.excludes) {
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

            let path = match extraction_path(&path, self.strip_components) {
                Ok(Some(path)) => path,
                Ok(None) => continue,
                Err(error) => {
                    eprintln!("tar: {error}");
                    failed = true;
                    continue;
                }
            };
            if self.to_stdout {
                if self.verbose {
                    let mut stderr = io::stderr().lock();
                    write_escaped(&mut stderr, entry.path_bytes().as_ref())?;
                    writeln!(stderr)?;
                }
                if !entry_is_directory(&entry) {
                    io::copy(&mut entry, &mut stdout)?;
                }
                continue;
            }
            if self.verbose {
                write_escaped(&mut stdout, entry.path_bytes().as_ref())?;
                writeln!(stdout)?;
            }
            let root = &self.directories[directory];
            if entry_is_directory(&entry) {
                match prepare_directory(root, &path) {
                    Ok(created) => {
                        if created || self.overwrite {
                            directories.push((entry, path, directory));
                        }
                    }
                    Err(error) => {
                        report_extraction_error(&path, &entry, &error);
                        failed = true;
                    }
                }
            } else if let Err(error) = unpack_entry(
                &mut entry,
                root,
                &path,
                self.strip_components,
                self.overwrite,
            ) {
                report_extraction_error(&path, &entry, &error);
                failed = true;
            }
        }

        // Resolve aliases before restrictive modes make their parent paths inaccessible.
        directories.retain_mut(|(entry, path, directory)| {
            match directory_destination(&self.directories[*directory], path) {
                Ok(destination) => {
                    *path = destination;
                    true
                }
                Err(error) => {
                    report_extraction_error(path, entry, &error);
                    failed = true;
                    false
                }
            }
        });
        directories.sort_by_key(|(_, path, _)| std::cmp::Reverse(path.components().count()));
        for (mut directory, path, root) in directories {
            if let Err(error) = finalize_directory(
                &mut directory,
                &self.directories[root],
                &path,
                self.preserve_mtime,
            ) {
                report_extraction_error(&path, &directory, &error);
                failed = true;
            }
        }

        for (operand, matched) in self.operands.iter().zip(matched) {
            if !matched {
                eprintln!("tar: {}: Not found in archive", operand.name.display());
                failed = true;
            }
        }
        if failed {
            Err(invalid(
                "Exiting with failure status due to previous errors",
            ))
        } else {
            Ok(())
        }
    }
}

fn build_archive<W: Write>(output: W, options: &Options) -> io::Result<W> {
    let mut archive = tar::Builder::new(output);
    archive.follow_symlinks(options.dereference);
    let verbose: Option<Box<dyn Write>> = options.verbose.then(|| {
        if options
            .archive
            .as_deref()
            .is_some_and(|path| path != OsStr::new("-"))
        {
            Box::new(io::stdout()) as Box<dyn Write>
        } else {
            Box::new(io::stderr()) as Box<dyn Write>
        }
    });
    let mut context = CreateContext {
        options,
        verbose,
        #[cfg(unix)]
        hard_links: HashMap::new(),
        #[cfg(unix)]
        hard_link_names: HashMap::new(),
        active_directories: Vec::new(),
    };
    for operand in &options.operands {
        let source = options.directories[operand.directory].join(&operand.name);
        context.append_tree(&mut archive, &source, &operand.name)?;
    }

    archive.finish()?;
    archive.into_inner()
}

#[cfg(unix)]
type FileIdentity = (u64, u64);
#[cfg(not(unix))]
type FileIdentity = PathBuf;

struct CreateContext<'a> {
    options: &'a Options,
    verbose: Option<Box<dyn Write>>,
    #[cfg(unix)]
    hard_links: HashMap<FileIdentity, PathBuf>,
    #[cfg(unix)]
    hard_link_names: HashMap<PathBuf, FileIdentity>,
    active_directories: Vec<FileIdentity>,
}

impl CreateContext<'_> {
    fn append_tree<W: Write>(
        &mut self,
        archive: &mut tar::Builder<W>,
        source: &Path,
        archive_path: &Path,
    ) -> io::Result<()> {
        if is_excluded(archive_path, &self.options.excludes) {
            return Ok(());
        }

        let metadata = if self.options.dereference {
            fs::metadata(source)?
        } else {
            fs::symlink_metadata(source)?
        };
        #[cfg(unix)]
        if let Some(identity) = self
            .hard_link_names
            .remove(archive_relative_name(archive_path))
        {
            self.hard_links.remove(&identity);
        }
        if let Some(output) = &mut self.verbose {
            write_escaped(output, archive_path.as_os_str().as_encoded_bytes())?;
            if metadata.is_dir() && !archive_path.as_os_str().as_encoded_bytes().ends_with(b"/") {
                output.write_all(b"/")?;
            }
            writeln!(output)?;
        }

        if metadata.is_dir() {
            if self.options.dereference {
                let identity = file_identity(source, &metadata)?;
                if self.active_directories.contains(&identity) {
                    return Err(invalid(format!(
                        "{}: Directory cycle detected",
                        archive_path.display()
                    )));
                }
                self.active_directories.push(identity);
            }
            archive.append_dir(archive_path, source)?;
            for child in fs::read_dir(source)? {
                let child = child?;
                self.append_tree(
                    archive,
                    &child.path(),
                    &archive_path.join(child.file_name()),
                )?;
            }
            if self.options.dereference {
                self.active_directories.pop();
            }
        } else {
            #[cfg(unix)]
            if metadata.is_file() && metadata.nlink() > 1 {
                let identity = (metadata.dev(), metadata.ino());
                if let Some(target) = self.hard_links.get(&identity) {
                    let mut header = tar::Header::new_gnu();
                    header.set_metadata(&metadata);
                    header.set_entry_type(tar::EntryType::Link);
                    header.set_size(0);
                    archive.append_link(&mut header, archive_path, target)?;
                    return Ok(());
                }
                archive.append_path_with_name(source, archive_path)?;
                let target = archive_relative_name(archive_path).to_path_buf();
                self.hard_link_names.insert(target.clone(), identity);
                self.hard_links.insert(identity, target);
                return Ok(());
            }
            #[cfg(unix)]
            if append_special(archive, archive_path, &metadata)? {
                return Ok(());
            }
            archive.append_path_with_name(source, archive_path)?;
        }
        Ok(())
    }
}

fn archive_relative_name(path: &Path) -> &Path {
    let mut components = path.components();
    while matches!(
        components.clone().next(),
        Some(Component::Prefix(_) | Component::RootDir | Component::CurDir)
    ) {
        components.next();
    }
    components.as_path()
}

#[cfg(unix)]
fn append_special<W: Write>(
    archive: &mut tar::Builder<W>,
    path: &Path,
    metadata: &fs::Metadata,
) -> io::Result<bool> {
    let kind = metadata.file_type();
    let kind = if kind.is_fifo() {
        tar::EntryType::Fifo
    } else if kind.is_char_device() {
        tar::EntryType::Char
    } else if kind.is_block_device() {
        tar::EntryType::Block
    } else {
        return Ok(false);
    };
    let mut header = tar::Header::new_gnu();
    header.set_metadata(metadata);
    header.set_entry_type(kind);
    header.set_size(0);
    if kind.is_character_special() || kind.is_block_special() {
        let device = metadata.rdev() as libc::dev_t;
        #[cfg(any(target_os = "solaris", target_os = "illumos"))]
        // SAFETY: these scalar device-number accessors have no memory preconditions.
        let (major, minor) = unsafe { (libc::major(device), libc::minor(device)) };
        #[cfg(not(any(target_os = "solaris", target_os = "illumos")))]
        let (major, minor) = (libc::major(device), libc::minor(device));
        #[cfg(not(target_os = "linux"))]
        let (major, minor) = (major as u32, minor as u32);
        header.set_device_major(major)?;
        header.set_device_minor(minor)?;
    }
    archive.append_data(&mut header, path, io::empty())?;
    Ok(true)
}

#[cfg(unix)]
fn file_identity(_path: &Path, metadata: &fs::Metadata) -> io::Result<FileIdentity> {
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(path: &Path, _metadata: &fs::Metadata) -> io::Result<FileIdentity> {
    fs::canonicalize(path)
}

fn compression_from_suffix(path: &OsStr) -> Option<Compression> {
    let path = path.as_encoded_bytes();
    if [b".gz".as_slice(), b".tgz"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
    {
        Some(Compression::Gzip)
    } else if [b".bz2".as_slice(), b".tbz", b".tbz2"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
    {
        Some(Compression::Bzip2)
    } else if [b".xz".as_slice(), b".txz"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
    {
        Some(Compression::Xz)
    } else if [b".zst".as_slice(), b".zstd", b".tzst"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
    {
        Some(Compression::Zstd)
    } else {
        None
    }
}

fn detect_compression(prefix: &[u8]) -> Option<Compression> {
    if prefix.starts_with(&[0x1f, 0x8b]) {
        Some(Compression::Gzip)
    } else if prefix.starts_with(b"BZh") {
        Some(Compression::Bzip2)
    } else if prefix.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Some(Compression::Xz)
    } else if prefix.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || (prefix
            .first()
            .is_some_and(|byte| (0x50..=0x5f).contains(byte))
            && prefix.get(1..4) == Some(&[0x2a, 0x4d, 0x18]))
    {
        Some(Compression::Zstd)
    } else {
        None
    }
}

fn pattern_path_bytes(path: &Path) -> &[u8] {
    let bytes = path.as_os_str().as_encoded_bytes();
    let end = bytes
        .iter()
        .rposition(|byte| *byte != b'/')
        .map_or(0, |index| index + 1);
    &bytes[..end]
}

fn is_excluded(path: &Path, patterns: &[Exclusion]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    path.ancestors().any(|ancestor| {
        let bytes = pattern_path_bytes(ancestor);
        !bytes.is_empty()
            && patterns.iter().any(|pattern| {
                let name = pattern.name.as_encoded_bytes();
                let matches = |value: &[u8]| {
                    if pattern.wildcards {
                        wildcard_match(name, value)
                    } else {
                        name == value
                    }
                };
                matches(bytes)
                    || bytes
                        .iter()
                        .enumerate()
                        .any(|(index, byte)| *byte == b'/' && matches(&bytes[index + 1..]))
            })
    })
}

fn wildcard_match(pattern: &[u8], value: &[u8]) -> bool {
    let (mut pattern_index, mut value_index) = (0, 0);
    let (mut star, mut star_value) = (None, 0);
    while value_index < value.len() {
        if pattern.get(pattern_index) == Some(&b'*') {
            star = Some(pattern_index);
            pattern_index += 1;
            star_value = value_index;
        } else if let Some(length) = wildcard_byte(&pattern[pattern_index..], value[value_index]) {
            pattern_index += length;
            value_index += 1;
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

fn wildcard_byte(pattern: &[u8], value: u8) -> Option<usize> {
    let first = *pattern.first()?;
    if first == b'?' {
        return Some(1);
    }
    if first == b'['
        && let Some((matches, length)) = wildcard_class(pattern, value)
    {
        return matches.then_some(length);
    }
    let (literal, length) = pattern_literal(pattern, 0);
    (literal == value).then_some(length)
}

fn pattern_literal(pattern: &[u8], index: usize) -> (u8, usize) {
    if pattern[index] == b'\\' && index + 1 < pattern.len() {
        (pattern[index + 1], 2)
    } else {
        (pattern[index], 1)
    }
}

fn wildcard_class(pattern: &[u8], value: u8) -> Option<(bool, usize)> {
    let negate = matches!(pattern.get(1), Some(b'!' | b'^'));
    let start = if negate { 2 } else { 1 };
    let mut index = start;
    let mut matches = false;
    while index < pattern.len() {
        if pattern[index] == b']' && index > start {
            return Some((matches != negate, index + 1));
        }
        if pattern.get(index..index + 2) == Some(b"[:") {
            let end = pattern[index + 2..]
                .windows(2)
                .position(|bytes| bytes == b":]")?
                + index
                + 2;
            matches |= named_wildcard_class(&pattern[index + 2..end], value)?;
            index = end + 2;
            continue;
        }
        let (first, length) = pattern_literal(pattern, index);
        index += length;
        if pattern.get(index) == Some(&b'-')
            && pattern.get(index + 1).is_some_and(|byte| *byte != b']')
        {
            let (last, length) = pattern_literal(pattern, index + 1);
            matches |= (first..=last).contains(&value);
            index += length + 1;
        } else {
            matches |= first == value;
        }
    }
    None
}

fn named_wildcard_class(name: &[u8], value: u8) -> Option<bool> {
    Some(match name {
        b"alnum" => value.is_ascii_alphanumeric(),
        b"alpha" => value.is_ascii_alphabetic(),
        b"ascii" => value.is_ascii(),
        b"blank" => matches!(value, b' ' | b'\t'),
        b"cntrl" => value.is_ascii_control(),
        b"digit" => value.is_ascii_digit(),
        b"graph" => value.is_ascii_graphic(),
        b"lower" => value.is_ascii_lowercase(),
        b"print" => value.is_ascii_graphic() || value == b' ',
        b"punct" => value.is_ascii_punctuation(),
        b"space" => matches!(value, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r'),
        b"upper" => value.is_ascii_uppercase(),
        b"word" => value.is_ascii_alphanumeric() || value == b'_',
        b"xdigit" => value.is_ascii_hexdigit(),
        _ => return None,
    })
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

fn member_selected(
    path: &Path,
    operands: &[Operand],
    matched: &mut [bool],
    default_directory: usize,
) -> Option<usize> {
    let mut selected = operands.is_empty().then_some(default_directory);
    for (operand, matched) in operands.iter().zip(matched) {
        let matches = if operand.wildcards {
            path.ancestors().any(|ancestor| {
                wildcard_match(
                    operand.name.as_os_str().as_encoded_bytes(),
                    pattern_path_bytes(ancestor),
                )
            })
        } else {
            path.starts_with(&operand.name)
        };
        if matches {
            *matched = true;
            selected.get_or_insert(operand.directory);
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

fn root_permissions_default() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no arguments or memory preconditions.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn current_umask() -> u32 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Ok(status) = fs::read_to_string("/proc/self/status")
        && let Some(mask) = status
            .lines()
            .find_map(|line| line.strip_prefix("Umask:"))
            .and_then(|value| u32::from_str_radix(value.trim(), 8).ok())
    {
        return mask;
    }
    #[cfg(unix)]
    {
        use nix::sys::stat::{Mode, umask};
        // Match Brush's fallback on systems without a readable process umask.
        let mask = umask(Mode::empty());
        umask(mask);
        mask.bits() as u32
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn prepare_directory(root: &Path, path: &Path) -> io::Result<bool> {
    let destination = extraction_destination(root, path)?;
    match fs::create_dir(&destination) {
        Ok(()) => Ok(true),
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                && fs::symlink_metadata(&destination)?.is_dir() =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn directory_destination(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let destination = extraction_destination(root, path)?;
    if !fs::symlink_metadata(&destination)?.is_dir() {
        return Err(invalid(format!("{}: Not a directory", path.display())));
    }
    validate_inside(root, &destination)
}

fn finalize_directory<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    root: &Path,
    destination: &Path,
    preserve_mtime: bool,
) -> io::Result<()> {
    if !fs::symlink_metadata(destination)?.is_dir() {
        return Err(invalid(format!(
            "{}: Not a directory",
            destination.display()
        )));
    }
    let destination = validate_inside(root, destination)?;
    let timestamp = preserve_mtime.then(|| entry.header().mtime()).transpose()?;
    entry.unpack(&destination)?;
    if let Some(timestamp) = timestamp {
        set_directory_mtime(&destination, timestamp)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_directory_mtime(path: &Path, seconds: u64) -> io::Result<()> {
    use nix::fcntl::AT_FDCWD;
    use nix::sys::stat::{UtimensatFlags, utimensat};
    use nix::sys::time::TimeSpec;

    let seconds = seconds
        .try_into()
        .map_err(|_| invalid("directory modification time is out of range"))?;
    utimensat(
        AT_FDCWD,
        path,
        &TimeSpec::UTIME_OMIT,
        &TimeSpec::new(seconds, 0),
        UtimensatFlags::NoFollowSymlink,
    )
    .map_err(io::Error::from)
}

#[cfg(not(unix))]
fn set_directory_mtime(path: &Path, seconds: u64) -> io::Result<()> {
    let time = UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .ok_or_else(|| invalid("directory modification time is out of range"))?;
    File::open(path)?.set_times(fs::FileTimes::new().set_modified(time))
}

fn report_extraction_error<R: Read>(path: &Path, entry: &tar::Entry<'_, R>, error: &io::Error) {
    let mut stderr = io::stderr().lock();
    let _ = write!(stderr, "tar: ");
    let _ = write_escaped(&mut stderr, path.as_os_str().as_encoded_bytes());
    if error.kind() == io::ErrorKind::AlreadyExists {
        if entry.header().entry_type().is_hard_link() {
            let _ = write!(stderr, ": Cannot hard link to '");
            if let Some(target) = entry.link_name_bytes() {
                let _ = write_escaped(&mut stderr, &target);
            }
            let _ = writeln!(stderr, "': File exists");
        } else if entry_is_directory(entry) {
            let _ = writeln!(stderr, ": Cannot mkdir: File exists");
        } else {
            let _ = writeln!(stderr, ": Cannot open: File exists");
        }
    } else {
        let _ = writeln!(stderr, ": {error}");
    }
}

fn unpack_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    root: &Path,
    path: &Path,
    strip: usize,
    overwrite: bool,
) -> io::Result<()> {
    if entry.header().entry_type().is_hard_link() {
        let destination = extraction_destination(root, path)?;
        let target = entry
            .link_name()?
            .ok_or_else(|| invalid("hard link has no target"))?;
        let target = extraction_path(&target, strip)?
            .ok_or_else(|| invalid("hard link target has no remaining components"))?;
        let target = root.join(target);
        validate_inside(root, &target)?;
        let parent = target
            .parent()
            .ok_or_else(|| invalid("hard link target has no parent"))?;
        let name = target
            .file_name()
            .ok_or_else(|| invalid("hard link target has no file name"))?;
        let target = validate_inside(root, parent)?.join(name);
        if overwrite && target == destination {
            return Ok(());
        }
        fs::hard_link(&target, &destination).or_else(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists && overwrite {
                fs::remove_file(&destination)?;
                fs::hard_link(&target, &destination)
            } else {
                Err(error)
            }
        })?;
    } else if strip == 0 {
        entry.unpack_in(root)?;
    } else {
        let destination = extraction_destination(root, path)?;
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
