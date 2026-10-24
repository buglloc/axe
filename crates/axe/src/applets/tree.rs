use std::ffi::OsString;
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

    let options = match cli_to_options(&cli) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("tree: {error}");
            return 2;
        }
    };

    let result = if options.from_file {
        list_directory(&cli.path, &options)
    } else if let Some(path) = &options.output_file {
        fs::File::create(path).and_then(|file| {
            list_filesystem(
                Path::new(&cli.path),
                &options,
                &mut io::BufWriter::new(file),
            )
        })
    } else {
        list_filesystem(
            Path::new(&cli.path),
            &options,
            &mut io::BufWriter::new(io::stdout().lock()),
        )
    };
    match result {
        Ok(()) => 0,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(error) => {
            eprintln!("tree: {}: {error}", cli.path);
            1
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
    icons: IconManager,
    directories: u64,
    files: u64,
}

fn list_filesystem(path: &Path, options: &TreeOptions, writer: &mut impl Write) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    let mut listing = Listing {
        options,
        icons: IconManager::new(),
        directories: u64::from(metadata.is_dir()),
        files: u64::from(!metadata.is_dir()),
    };
    listing.write_metadata(writer, &metadata)?;
    writeln!(writer, "{}", path.display())?;
    if metadata.is_dir() {
        let entries = listing.read_entries(path, 0, false)?;
        listing.write_entries(writer, entries, &mut Vec::new(), false)?;
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
    writer.flush()
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
            let is_directory = metadata.is_dir();
            if (!is_directory && self.options.dir_only)
                || (!is_directory
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
            let name = if self.options.full_path {
                entry.path.as_os_str()
            } else {
                &entry.name
            }
            .to_string_lossy();
            if self.options.color && !self.options.no_color {
                write!(writer, "{}", colorize(&entry.entry, &name))?;
            } else {
                write!(writer, "{name}")?;
            }
            if self.options.classify {
                write!(writer, "{}", classify(&entry.metadata))?;
            }
            writeln!(writer)?;
            if entry.metadata.is_dir() {
                self.directories += 1;
                let matched = matched_parent || self.directory_matches(&entry);
                let children = match entry.children.take() {
                    Some(children) => children,
                    None => self.read_entries(&entry.path, parents_last.len() + 1, matched)?,
                };
                parents_last.push(last);
                self.write_entries(writer, children, parents_last, matched)?;
                parents_last.pop();
            } else {
                self.files += 1;
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
}
