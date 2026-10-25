use std::borrow::Cow;
use std::ffi::{CStr, OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{Arg, ArgAction, CommandFactory, FromArgMatches};
use rust_tree::rust_tree::cli::{Cli, cli_to_options};
use rust_tree::rust_tree::display::colorize;
use rust_tree::rust_tree::icons::IconManager;
use rust_tree::rust_tree::options::TreeOptions;
use rust_tree::rust_tree::traversal::list_directory;

fn command() -> clap::Command {
    Cli::command()
        .disable_help_flag(true)
        .args_override_self(true)
        .after_help(
            "Supports filesystem and --fromfile text listings. -N prints names as-is; \
             -q prints non-printable characters as '?'. JSON (-J), following directory \
             symlinks (-l), and --charset are not supported.",
        )
        .mut_arg("human_readable", |arg| arg.short('h'))
        .mut_arg("pattern", |arg| arg.allow_hyphen_values(true))
        .mut_arg("exclude", |arg| arg.allow_hyphen_values(true))
        .arg(
            Arg::new("help")
                .long("help")
                .help("Print help")
                .action(ArgAction::Help),
        )
}

pub fn tree(args: Vec<OsString>) -> i32 {
    let parsed = command()
        .try_get_matches_from(args)
        .and_then(|matches| Cli::from_arg_matches(&matches));
    let cli = match parsed {
        Ok(cli) => cli,
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

    let mut options = match cli_to_options(&cli) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("tree: {error}");
            return 2;
        }
    };
    let utf8_names = utf8_locale();
    options.ascii |= !utf8_names;

    let result = if options.from_file {
        list_directory(&cli.path, &options).map(|()| false)
    } else if let Some(path) = &options.output_file {
        fs::File::create(path).and_then(|file| {
            list_filesystem(
                Path::new(&cli.path),
                &options,
                utf8_names,
                &mut io::BufWriter::new(file),
            )
        })
    } else {
        list_filesystem(
            Path::new(&cli.path),
            &options,
            utf8_names,
            &mut io::BufWriter::new(io::stdout().lock()),
        )
    };
    match result {
        Ok(had_errors) => {
            if had_errors {
                2
            } else {
                0
            }
        }
        Err(error) => {
            eprintln!("tree: {}: {error}", cli.path.display());
            2
        }
    }
}

struct Entry {
    entry: fs::DirEntry,
    path: PathBuf,
    name: OsString,
    metadata: fs::Metadata,
    children: Option<Vec<Entry>>,
}

struct Listing<'a> {
    options: &'a TreeOptions,
    utf8_names: bool,
    icons: IconManager,
    directories: u64,
    files: u64,
    had_errors: bool,
}

fn list_filesystem(
    path: &Path,
    options: &TreeOptions,
    utf8_names: bool,
    writer: &mut impl Write,
) -> io::Result<bool> {
    let mut listing = Listing {
        options,
        utf8_names,
        icons: IconManager::new(),
        directories: 0,
        files: 0,
        had_errors: false,
    };
    match fs::metadata(path) {
        Ok(metadata) => {
            listing.directories = u64::from(metadata.is_dir());
            listing.files = u64::from(!metadata.is_dir());
            listing.write_metadata(writer, &metadata)?;
            write_name(
                writer,
                escaped_name(
                    path.as_os_str(),
                    utf8_names,
                    options.no_quote,
                    options.quote_nonprintable,
                ),
            )?;
            if metadata.is_dir() {
                match listing.read_entries(path, 0, false) {
                    Ok(entries) => {
                        writeln!(writer)?;
                        listing.write_entries(writer, entries, &mut Vec::new(), false)?;
                    }
                    Err(_) => {
                        listing.had_errors = true;
                        writeln!(writer, "  [error opening dir]")?;
                    }
                }
            } else {
                writeln!(writer)?;
            }
        }
        Err(_) => {
            listing.had_errors = true;
            write_name(
                writer,
                escaped_name(
                    path.as_os_str(),
                    utf8_names,
                    options.no_quote,
                    options.quote_nonprintable,
                ),
            )?;
            writeln!(writer, "  [error opening dir]")?;
        }
    }
    if !options.no_report {
        write!(
            writer,
            "\n{} {}",
            listing.directories,
            if listing.directories == 1 {
                "directory"
            } else {
                "directories"
            }
        )?;
        if !options.dir_only {
            write!(
                writer,
                ", {} {}",
                listing.files,
                if listing.files == 1 { "file" } else { "files" }
            )?;
        }
        writeln!(writer)?;
    }
    writer.flush()?;
    Ok(listing.had_errors)
}

impl Listing<'_> {
    fn read_entries(
        &self,
        path: &Path,
        depth: usize,
        matched_parent: bool,
    ) -> io::Result<Vec<Entry>> {
        if self
            .options
            .level
            .is_some_and(|level| depth >= level as usize)
        {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        let mut count = 0_u64;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            count += 1;
            let name = entry.file_name();
            let text = name.to_string_lossy();
            if (!self.options.all_files && text.starts_with('.'))
                || self
                    .options
                    .exclude_patterns
                    .iter()
                    .any(|pattern| pattern.matches(&text))
            {
                continue;
            }
            let metadata = entry.metadata()?;
            let is_directory =
                metadata.is_dir() || (metadata.is_symlink() && entry.path().is_dir());
            if (!is_directory && self.options.dir_only)
                || (!metadata.is_dir()
                    && !matched_parent
                    && self
                        .options
                        .pattern_glob
                        .as_ref()
                        .is_some_and(|pattern| !pattern.matches(&text)))
            {
                continue;
            }
            entries.push(Entry {
                path: entry.path(),
                entry,
                name,
                metadata,
                children: None,
            });
        }
        if depth != 0 && self.options.file_limit.is_some_and(|limit| count > limit) {
            return Ok(Vec::new());
        }
        entries.sort_by(|left, right| {
            let directories = if self.options.dirs_first {
                right.metadata.is_dir().cmp(&left.metadata.is_dir())
            } else {
                std::cmp::Ordering::Equal
            };
            directories.then_with(|| {
                let order = if self.options.sort_by_time {
                    left.metadata
                        .modified()
                        .ok()
                        .cmp(&right.metadata.modified().ok())
                } else {
                    std::cmp::Ordering::Equal
                }
                .then_with(|| left.name.cmp(&right.name));
                if self.options.reverse {
                    order.reverse()
                } else {
                    order
                }
            })
        });
        if self.options.prune
            && (self.options.pattern_glob.is_some() || !self.options.exclude_patterns.is_empty())
        {
            for entry in &mut entries {
                if entry.metadata.is_dir() {
                    let matched = matched_parent || self.directory_matches(entry);
                    entry.children = Some(self.read_entries(&entry.path, depth + 1, matched)?);
                }
            }
            entries.retain(|entry| {
                !entry.metadata.is_dir()
                    || entry
                        .children
                        .as_ref()
                        .is_some_and(|children| !children.is_empty())
                    || self.directory_matches(entry)
            });
        }
        Ok(entries)
    }

    fn directory_matches(&self, entry: &Entry) -> bool {
        self.options.match_dirs
            && self
                .options
                .pattern_glob
                .as_ref()
                .is_some_and(|pattern| pattern.matches(&entry.name.to_string_lossy()))
    }

    fn write_entries(
        &mut self,
        writer: &mut impl Write,
        entries: Vec<Entry>,
        parents_last: &mut Vec<bool>,
        matched_parent: bool,
    ) -> io::Result<()> {
        let count = entries.len();
        for (index, mut entry) in entries.into_iter().enumerate() {
            let last = index + 1 == count;
            if !self.options.no_indent {
                for is_last in parents_last.iter() {
                    write!(
                        writer,
                        "{}",
                        if *is_last {
                            "    "
                        } else if self.options.ascii {
                            "|   "
                        } else {
                            "│   "
                        }
                    )?;
                }
                write!(
                    writer,
                    "{}",
                    match (last, self.options.ascii) {
                        (true, true) => "`-- ",
                        (false, true) => "|-- ",
                        (true, false) => "└── ",
                        (false, false) => "├── ",
                    }
                )?;
            }
            self.write_metadata(writer, &entry.metadata)?;
            if self.options.icons {
                write!(writer, "{} ", self.icons.get_icon_for_path(&entry.path))?;
            }
            let name = escaped_name(
                if self.options.full_path {
                    entry.path.as_os_str()
                } else {
                    &entry.name
                },
                self.utf8_names,
                self.options.no_quote,
                self.options.quote_nonprintable,
            );
            if self.options.color && !self.options.no_color {
                if let Ok(text) = std::str::from_utf8(name.as_ref()) {
                    write!(writer, "{}", colorize(&entry.entry, text))?;
                } else {
                    writer.write_all(name.as_ref())?;
                }
            } else {
                writer.write_all(name.as_ref())?;
            }
            if self.options.classify {
                write!(writer, "{}", classify(&entry.metadata))?;
            }
            if entry.metadata.is_symlink() {
                write!(writer, " -> ")?;
                write_name(
                    writer,
                    escaped_name(
                        fs::read_link(&entry.path)?.as_os_str(),
                        self.utf8_names,
                        self.options.no_quote,
                        self.options.quote_nonprintable,
                    ),
                )?;
            }
            let children = if entry.metadata.is_dir() {
                self.directories += 1;
                let matched = matched_parent || self.directory_matches(&entry);
                let result = match entry.children.take() {
                    Some(children) => Ok(children),
                    None => self.read_entries(&entry.path, parents_last.len() + 1, matched),
                };
                match result {
                    Ok(children) => Some((children, matched)),
                    Err(_) => {
                        self.had_errors = true;
                        write!(writer, "  [error opening dir]")?;
                        None
                    }
                }
            } else {
                if entry.metadata.is_symlink() && entry.path.is_dir() {
                    self.directories += 1;
                } else {
                    self.files += 1;
                }
                None
            };
            writeln!(writer)?;
            if let Some((children, matched)) = children {
                parents_last.push(last);
                self.write_entries(writer, children, parents_last, matched)?;
                parents_last.pop();
            }
        }
        Ok(())
    }

    fn write_metadata(&self, writer: &mut impl Write, metadata: &fs::Metadata) -> io::Result<()> {
        if !(self.options.print_permissions
            || self.options.print_size
            || self.options.human_readable
            || self.options.print_mod_date)
        {
            return Ok(());
        }
        write!(writer, "[")?;
        let mut separated = false;
        if self.options.print_permissions {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let permissions = rust_tree::rust_tree::display::format_permissions_unix(
                    metadata.permissions().mode(),
                    metadata.is_dir(),
                );
                write!(writer, "{}", &permissions[1..permissions.len() - 1])?;
                separated = true;
            }
        }
        if self.options.human_readable || self.options.print_size {
            if separated {
                write!(writer, " ")?;
            }
            if self.options.human_readable {
                write!(writer, "{}", human_size(metadata.len()))?;
            } else {
                write!(writer, "{:>11}", metadata.len())?;
            }
            separated = true;
        }
        if self.options.print_mod_date {
            if separated {
                write!(writer, " ")?;
            }
            write!(writer, "{}", modification_date(metadata.modified()?))?;
        }
        write!(writer, "]  ")
    }
}

fn utf8_locale() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: setlocale reads the NUL-terminated locale name.
        let locale = unsafe { libc::setlocale(libc::LC_CTYPE, c"".as_ptr()) };
        if locale.is_null() {
            return false;
        }
        // This is the resolved locale returned by libc, not an environment
        // variable. An invalid requested locale returns null above.
        let locale = unsafe { CStr::from_ptr(locale) }.to_bytes();
        locale
            .windows(5)
            .any(|part| part.eq_ignore_ascii_case(b"utf-8"))
            || locale
                .windows(4)
                .any(|part| part.eq_ignore_ascii_case(b"utf8"))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn write_name(writer: &mut impl Write, name: Cow<'_, [u8]>) -> io::Result<()> {
    writer.write_all(name.as_ref())
}

fn escaped_name(
    name: &OsStr,
    utf8: bool,
    no_quote: bool,
    quote_nonprintable: bool,
) -> Cow<'_, [u8]> {
    let bytes = name.as_encoded_bytes();
    if no_quote {
        return Cow::Borrowed(bytes);
    }
    let mut escaped = Vec::new();
    let mut changed = false;
    for chunk in bytes.utf8_chunks() {
        for character in chunk.valid().chars() {
            let mut append_octal = |byte: u8| {
                escaped.extend_from_slice(format!("\\{byte:03o}").as_bytes());
            };
            if utf8 {
                if character.is_control() {
                    changed = true;
                    if quote_nonprintable {
                        escaped.push(b'?');
                    } else {
                        for byte in character.encode_utf8(&mut [0; 4]).bytes() {
                            append_octal(byte);
                        }
                    }
                } else {
                    escaped.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
                }
            } else if !character.is_ascii() {
                changed = true;
                for byte in character.encode_utf8(&mut [0; 4]).bytes() {
                    append_octal(byte);
                }
            } else {
                let byte = character as u8;
                match byte {
                    b'\\' => {
                        changed = true;
                        escaped.extend_from_slice(b"\\\\");
                    }
                    b' ' => {
                        changed = true;
                        escaped.extend_from_slice(b"\\ ");
                    }
                    0x07 => {
                        changed = true;
                        escaped.extend_from_slice(b"\\a");
                    }
                    0x08 => {
                        changed = true;
                        escaped.extend_from_slice(b"\\b");
                    }
                    b'\t' => {
                        changed = true;
                        escaped.extend_from_slice(if utf8 { b"\\011" } else { b"\\t" });
                    }
                    b'\n' => {
                        changed = true;
                        escaped.extend_from_slice(if utf8 { b"\\012" } else { b"\\n" });
                    }
                    0x0b => {
                        changed = true;
                        escaped.extend_from_slice(if utf8 { b"\\013" } else { b"\\v" });
                    }
                    0x0c => {
                        changed = true;
                        escaped.extend_from_slice(if utf8 { b"\\014" } else { b"\\f" });
                    }
                    b'\r' => {
                        changed = true;
                        escaped.extend_from_slice(if utf8 { b"\\015" } else { b"\\r" });
                    }
                    0..=0x1f | 0x7f => {
                        changed = true;
                        append_octal(byte);
                    }
                    _ => escaped.push(byte),
                }
            }
        }
        for byte in chunk.invalid() {
            if quote_nonprintable && utf8 {
                changed = true;
                escaped.push(b'?');
            } else {
                changed = true;
                escaped.extend_from_slice(format!("\\{byte:03o}").as_bytes());
            }
        }
    }
    if changed {
        Cow::Owned(escaped)
    } else {
        Cow::Borrowed(bytes)
    }
}

fn human_size(mut size: u64) -> String {
    if size < 1024 {
        return format!("{size:>4}");
    }
    let mut unit = 1;
    while size >= 1024 * 1024 {
        size /= 1024;
        unit += 1;
    }
    let suffix = b"BKMGTPE"[unit] as char;
    let value = size as f64 / 1024.0;
    if (size + 52) / 1024 >= 10 {
        format!("{value:>3.0}{suffix}")
    } else {
        format!("{value:>3.1}{suffix}")
    }
}

fn classify(metadata: &fs::Metadata) -> &'static str {
    let kind = metadata.file_type();
    if kind.is_dir() {
        "/"
    } else if kind.is_symlink() {
        "@"
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{FileTypeExt, PermissionsExt};
            if kind.is_socket() {
                return "=";
            }
            if kind.is_fifo() {
                return "|";
            }
            if kind.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return "*";
            }
        }
        ""
    }
}

#[cfg(unix)]
fn modification_date(time: std::time::SystemTime) -> String {
    let seconds = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as libc::c_long,
        Err(error) => -(error.duration().as_secs() as libc::c_long),
    };
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: seconds is initialized, and local is writable storage for one tm.
    if unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) }.is_null() {
        return "Unknown date".into();
    }
    let format = if time > std::time::SystemTime::now()
        || std::time::SystemTime::now()
            .duration_since(time)
            .is_ok_and(|age| age.as_secs() > 6 * 31 * 24 * 60 * 60)
    {
        c"%b %e  %Y"
    } else {
        c"%b %e %H:%M"
    };
    let mut buffer = [0_u8; 128];
    // SAFETY: localtime_r initialized local; buffer is writable and its length
    // is passed to strftime, and format is NUL-terminated.
    let length = unsafe {
        libc::strftime(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            format.as_ptr(),
            local.as_ptr(),
        )
    };
    String::from_utf8_lossy(&buffer[..length]).into_owned()
}

#[cfg(not(unix))]
fn modification_date(time: std::time::SystemTime) -> String {
    time.duration_since(std::time::UNIX_EPOCH).map_or_else(
        |_| "Unknown date".into(),
        |duration| duration.as_secs().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes_use_compact_binary_units_and_tree_column_width() {
        for (size, expected) in [
            (0, "   0"),
            (1023, "1023"),
            (1024, "1.0K"),
            (5000, "4.9K"),
            (10 * 1024, " 10K"),
            (1024 * 1024, "1.0M"),
            (u64::MAX, " 16E"),
        ] {
            assert_eq!(human_size(size), expected, "{size}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn escaped_names_follow_c_and_utf8_locale_rules() {
        use std::os::unix::ffi::OsStringExt;

        let name = OsString::from_vec(b"back\\slash\n\x01space \xc3\xa9\xff".to_vec());
        assert_eq!(
            escaped_name(&name, false, false, false).as_ref(),
            b"back\\\\slash\\n\\001space\\ \\303\\251\\377"
        );
        assert_eq!(
            escaped_name(&name, true, false, false).as_ref(),
            b"back\\slash\\012\\001space \xc3\xa9\\377"
        );
        assert_eq!(
            escaped_name(&name, true, false, true).as_ref(),
            b"back\\slash??space \xc3\xa9?"
        );
        assert_eq!(
            escaped_name(&name, true, true, true).as_ref(),
            b"back\\slash\n\x01space \xc3\xa9\xff"
        );
    }
}
