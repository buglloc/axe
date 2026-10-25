use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Local;
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use regex::bytes::{Regex, RegexBuilder};

pub fn inotifywait(args: Vec<OsString>) -> i32 {
    match WaitOptions::parse(args).and_then(run_wait) {
        Ok(WaitOutcome::Event) => 0,
        Ok(WaitOutcome::Timeout) => 2,
        Err(error) if error.is_empty() => 0,
        Err(error) => {
            eprintln!("inotifywait: {error}");
            1
        }
    }
}

pub fn inotifywatch(args: Vec<OsString>) -> i32 {
    match WatchOptions::parse(args).and_then(run_watch) {
        Ok(()) => 0,
        Err(error) if error.is_empty() => 0,
        Err(error) => {
            eprintln!("inotifywatch: {error}");
            1
        }
    }
}

struct Common {
    paths: Vec<PathBuf>,
    excluded_paths: Vec<PathBuf>,
    recursive: bool,
    no_dereference: bool,
    timeout: Option<Duration>,
    mask: WatchMask,
    output_mask: EventMask,
    events_specified: bool,
    selected_events: Vec<&'static str>,
    exclude: Option<Regex>,
    include: Option<Regex>,
}

impl Common {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            excluded_paths: Vec::new(),
            recursive: false,
            no_dereference: false,
            timeout: None,
            mask: WatchMask::ALL_EVENTS,
            output_mask: EventMask::from_bits_retain(WatchMask::ALL_EVENTS.bits())
                | EventMask::UNMOUNT,
            events_specified: false,
            selected_events: event_columns().to_vec(),
            exclude: None,
            include: None,
        }
    }

    fn apply_event(&mut self, value: &str) -> Result<(), String> {
        if !self.events_specified {
            self.events_specified = true;
            self.output_mask = EventMask::empty();
            self.selected_events.clear();
        }
        for value in value.split(',') {
            let (mask, names) = parse_event(value)?;
            self.output_mask.insert(mask);
            for name in names {
                if !self.selected_events.contains(&name) {
                    self.selected_events.push(name);
                }
            }
        }
        self.mask = WatchMask::from_bits_truncate(self.output_mask.bits());
        if self.mask.is_empty() {
            // UNMOUNT is delivered automatically but is not a WatchMask flag.
            self.mask = WatchMask::ALL_EVENTS;
        }
        Ok(())
    }

    fn add_operand(&mut self, value: OsString) -> Result<(), String> {
        if let Some(path) = value.as_bytes().strip_prefix(b"@") {
            self.excluded_paths
                .push(absolute_path(Path::new(OsStr::from_bytes(path)))?);
        } else {
            self.paths.push(PathBuf::from(value));
        }
        Ok(())
    }

    fn path_excluded(&self, path: &Path) -> bool {
        !self.excluded_paths.is_empty()
            && absolute_path(path).is_ok_and(|path| {
                self.excluded_paths
                    .iter()
                    .any(|excluded| path.starts_with(excluded))
            })
    }
}

fn path_regex(pattern: &str, case_insensitive: bool) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .unicode(false)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|error| error.to_string())
}

struct WaitOptions {
    common: Common,
    monitor: bool,
    quiet: u8,
    csv: bool,
    format: Option<String>,
    time_format: String,
    newline: bool,
    output: Option<PathBuf>,
}

enum WaitOutcome {
    Event,
    Timeout,
}

impl WaitOptions {
    fn parse(args: Vec<OsString>) -> Result<Self, String> {
        let mut iter = args.into_iter().skip(1);
        let mut options = Self {
            common: Common::new(),
            monitor: false,
            quiet: 0,
            csv: false,
            format: None,
            time_format: "%Y-%m-%d %H:%M:%S".into(),
            newline: true,
            output: None,
        };
        while let Some(value) = iter.next() {
            match value.to_str().unwrap_or_default() {
                "-m" | "--monitor" => options.monitor = true,
                "-r" | "--recursive" => options.common.recursive = true,
                "-P" | "--no-dereference" => options.common.no_dereference = true,
                "-q" | "--quiet" => options.quiet = options.quiet.saturating_add(1),
                "-c" | "--csv" => options.csv = true,
                "--no-newline" => options.newline = false,
                "-e" | "--event" => options.common.apply_event(&text_argument(
                    iter.next(),
                    "event option requires an argument",
                )?)?,
                "-t" | "--timeout" => options.common.timeout = parse_timeout(iter.next())?,
                "--format" => {
                    options.format =
                        Some(text_argument(iter.next(), "--format requires an argument")?)
                }
                "--timefmt" => {
                    options.time_format =
                        text_argument(iter.next(), "--timefmt requires an argument")?
                }
                "-o" | "--outfile" => {
                    options.output = Some(PathBuf::from(
                        iter.next().ok_or("--outfile requires an argument")?,
                    ))
                }
                "--exclude" => {
                    options.common.exclude = Some(path_regex(
                        &text_argument(iter.next(), "--exclude requires a pattern")?,
                        false,
                    )?)
                }
                "--excludei" => {
                    options.common.exclude = Some(path_regex(
                        &text_argument(iter.next(), "--excludei requires a pattern")?,
                        true,
                    )?)
                }
                "--include" => {
                    options.common.include = Some(path_regex(
                        &text_argument(iter.next(), "--include requires a pattern")?,
                        false,
                    )?)
                }
                "--includei" => {
                    options.common.include = Some(path_regex(
                        &text_argument(iter.next(), "--includei requires a pattern")?,
                        true,
                    )?)
                }
                "-h" | "--help" => {
                    println!(
                        "Usage: inotifywait [-cmrPq] [-e EVENT] [-t SECONDS] [--format FMT] FILE ..."
                    );
                    return Err(String::new());
                }
                "-d" | "--daemon" | "-s" | "--syslog" => {
                    return Err(format!("unsupported option '{}'", value.to_string_lossy()));
                }
                "--" => {
                    for path in iter {
                        options.common.add_operand(path)?;
                    }
                    break;
                }
                value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
                _ => options.common.add_operand(value)?,
            }
        }
        if options.common.paths.is_empty() {
            return Err("no files specified to watch".into());
        }
        if options.csv && options.format.is_some() {
            return Err("--csv and --format cannot be combined".into());
        }
        Ok(options)
    }
}

fn run_wait(options: WaitOptions) -> Result<WaitOutcome, String> {
    if options.quiet == 0 {
        eprintln!(
            "Setting up watches.{}",
            if options.common.recursive {
                "  Beware: since -r was given, this may take a while!"
            } else {
                ""
            }
        );
    }
    let (mut inotify, mut watches) = establish(&options.common, false)?;
    if options.quiet == 0 {
        eprintln!("Watches established.");
    }
    let stopped = signal_flag()?;
    let started = Instant::now();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut output: Box<dyn std::io::Write> = match &options.output {
        Some(path) => Box::new(std::fs::File::create(path).map_err(|error| error.to_string())?),
        None => Box::new(std::io::stdout().lock()),
    };
    loop {
        if stopped.load(Ordering::Relaxed) {
            return Ok(WaitOutcome::Event);
        }
        let Some(events) = read_batch(&mut inotify, &mut buffer, options.common.timeout, started)?
        else {
            return Ok(WaitOutcome::Timeout);
        };
        if events.is_empty() {
            continue;
        }
        for event in events {
            let Some(root) = watches.get(&event.wd).cloned() else {
                continue;
            };
            let full = event
                .name
                .as_ref()
                .map_or_else(|| root.clone(), |name| root.join(name));
            if options.common.path_excluded(&full) {
                continue;
            }
            if options.common.recursive
                && event.mask.contains(EventMask::ISDIR)
                && (event.mask.contains(EventMask::CREATE)
                    || event.mask.contains(EventMask::MOVED_TO))
            {
                add_tree(&mut inotify, &mut watches, &full, &options.common, true)?;
            }
            let selected = event.mask & options.common.output_mask;
            if selected.is_empty() || !path_selected(&full, &options.common) {
                continue;
            }
            if options.quiet < 2 {
                let names = event_names(selected).join(",");
                let watched = watched_name(&root);
                let filename = event
                    .name
                    .as_ref()
                    .map(|value| value.as_bytes())
                    .unwrap_or_default();
                let rendered = if let Some(format) = &options.format {
                    render_format(
                        format,
                        &watched,
                        filename,
                        &names,
                        event.cookie,
                        &options.time_format,
                    )
                } else if options.csv {
                    let mut rendered = csv_field(&watched);
                    rendered.push(b',');
                    rendered.extend(csv_field(names.as_bytes()));
                    rendered.push(b',');
                    rendered.extend(csv_field(filename));
                    rendered
                } else if filename.is_empty() {
                    [watched.as_slice(), b" ", names.as_bytes()].concat()
                } else {
                    [watched.as_slice(), b" ", names.as_bytes(), b" ", filename].concat()
                };
                output
                    .write_all(&rendered)
                    .map_err(|error| error.to_string())?;
                if options.newline && !rendered.ends_with(b"\n") {
                    output.write_all(b"\n").map_err(|error| error.to_string())?;
                }
                output.flush().map_err(|error| error.to_string())?;
            }
            if !options.monitor {
                return Ok(WaitOutcome::Event);
            }
        }
    }
}

struct WatchOptions {
    common: Common,
    zero: bool,
    sort_event: String,
    ascending: bool,
    verbose: bool,
}

impl WatchOptions {
    fn parse(args: Vec<OsString>) -> Result<Self, String> {
        let mut iter = args.into_iter().skip(1);
        let mut options = Self {
            common: Common::new(),
            zero: false,
            sort_event: "total".into(),
            ascending: false,
            verbose: false,
        };
        while let Some(value) = iter.next() {
            match value.to_str().unwrap_or_default() {
                "-r" | "--recursive" => options.common.recursive = true,
                "-P" | "--no-dereference" => options.common.no_dereference = true,
                "-z" | "--zero" => options.zero = true,
                "-v" | "--verbose" => options.verbose = true,
                "-e" | "--event" => options.common.apply_event(&text_argument(
                    iter.next(),
                    "event option requires an argument",
                )?)?,
                "-t" | "--timeout" => options.common.timeout = parse_timeout(iter.next())?,
                "-a" | "--ascending" => {
                    options.ascending = true;
                    options.sort_event = text_argument(iter.next(), "ascending requires an event")?;
                }
                "-d" | "--descending" => {
                    options.ascending = false;
                    options.sort_event =
                        text_argument(iter.next(), "descending requires an event")?;
                }
                "--exclude" => {
                    options.common.exclude = Some(path_regex(
                        &text_argument(iter.next(), "--exclude requires a pattern")?,
                        false,
                    )?)
                }
                "--include" => {
                    options.common.include = Some(path_regex(
                        &text_argument(iter.next(), "--include requires a pattern")?,
                        false,
                    )?)
                }
                "-h" | "--help" => {
                    println!(
                        "Usage: inotifywatch [-vzrP] [-e EVENT] [-t SECONDS] [-a|-d EVENT] FILE ..."
                    );
                    return Err(String::new());
                }
                "--" => {
                    for path in iter {
                        options.common.add_operand(path)?;
                    }
                    break;
                }
                value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
                _ => options.common.add_operand(value)?,
            }
        }
        if options.common.paths.is_empty() {
            return Err("no files specified to watch".into());
        }
        if options.sort_event != "total" && !event_columns().contains(&options.sort_event.as_str())
        {
            return Err(format!("unknown sort event '{}'", options.sort_event));
        }
        Ok(options)
    }
}

fn run_watch(options: WatchOptions) -> Result<(), String> {
    eprintln!("Establishing watches...");
    let (mut inotify, mut watches) = establish(&options.common, options.verbose)?;
    if options.verbose {
        eprintln!("Total of {} watches.", watches.len());
    }
    eprintln!("Finished establishing watches, now collecting statistics.");
    if let Some(timeout) = options.common.timeout {
        eprintln!("Will listen for events for {} seconds.", timeout.as_secs());
    }
    let stopped = signal_flag()?;
    let started = Instant::now();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut counts = BTreeMap::<PathBuf, BTreeMap<&'static str, u64>>::new();
    loop {
        if stopped.load(Ordering::Relaxed) {
            break;
        }
        let Some(events) = read_batch(&mut inotify, &mut buffer, options.common.timeout, started)?
        else {
            break;
        };
        for event in events {
            let Some(root) = watches.get(&event.wd).cloned() else {
                continue;
            };
            let full = event
                .name
                .as_ref()
                .map_or_else(|| root.clone(), |name| root.join(name));
            if options.common.path_excluded(&full) {
                continue;
            }
            if options.common.recursive
                && event.mask.contains(EventMask::ISDIR)
                && (event.mask.contains(EventMask::CREATE)
                    || event.mask.contains(EventMask::MOVED_TO))
            {
                add_tree(&mut inotify, &mut watches, &full, &options.common, true)?;
            }
            let selected = event.mask & options.common.output_mask;
            if selected.is_empty() || !path_selected(&full, &options.common) {
                continue;
            }
            let row = counts.entry(root).or_default();
            for name in event_counter_names(selected) {
                *row.entry(name).or_default() += 1;
            }
        }
    }
    for path in watches.values() {
        counts.entry(path.clone()).or_default();
    }
    print_counts(counts, &options).map_err(|error| error.to_string())
}

fn print_counts(
    counts: BTreeMap<PathBuf, BTreeMap<&'static str, u64>>,
    options: &WatchOptions,
) -> std::io::Result<()> {
    if !counts
        .values()
        .any(|row| row.values().any(|count| *count != 0))
    {
        eprintln!("No events occurred.");
        return Ok(());
    }
    let columns = options
        .common
        .selected_events
        .iter()
        .copied()
        .filter(|column| {
            options.zero
                || counts
                    .values()
                    .any(|row| row.get(column).copied().unwrap_or_default() > 0)
        })
        .collect::<Vec<_>>();
    let mut rows = counts
        .into_iter()
        .filter(|(_, row)| options.zero || row.values().sum::<u64>() > 0)
        .collect::<Vec<_>>();
    let value = |row: &BTreeMap<&'static str, u64>| {
        if options.sort_event == "total" {
            row.values().sum()
        } else {
            row.get(options.sort_event.as_str())
                .copied()
                .unwrap_or_default()
        }
    };
    rows.sort_by_key(|(_, row)| value(row));
    if !options.ascending {
        rows.reverse();
    }
    let widths = std::iter::once("total")
        .chain(columns.iter().copied())
        .map(|column| {
            let maximum = rows
                .iter()
                .map(|(_, row)| {
                    if column == "total" {
                        row.values().sum::<u64>()
                    } else {
                        row.get(column).copied().unwrap_or_default()
                    }
                })
                .max()
                .unwrap_or_default();
            column.len().max(maximum.to_string().len()) + 2
        })
        .collect::<Vec<_>>();
    let mut output = std::io::stdout().lock();
    write!(output, "{:<width$}", "total", width = widths[0])?;
    for (index, column) in columns.iter().enumerate() {
        write!(output, "{column:<width$}", width = widths[index + 1])?;
    }
    writeln!(output, "filename")?;
    for (path, row) in rows {
        write!(
            output,
            "{:<width$}",
            row.values().sum::<u64>(),
            width = widths[0]
        )?;
        for (index, column) in columns.iter().enumerate() {
            write!(
                output,
                "{:<width$}",
                row.get(column).copied().unwrap_or_default(),
                width = widths[index + 1]
            )?;
        }
        output.write_all(&watched_name(&path))?;
        output.write_all(b"\n")?;
    }
    output.flush()
}

#[derive(Clone)]
struct OwnedEvent {
    wd: WatchDescriptor,
    mask: EventMask,
    cookie: u32,
    name: Option<OsString>,
}

fn read_batch(
    inotify: &mut Inotify,
    buffer: &mut [u8],
    timeout: Option<Duration>,
    started: Instant,
) -> Result<Option<Vec<OwnedEvent>>, String> {
    let timeout_ms = timeout
        .map(|limit| limit.saturating_sub(started.elapsed()))
        .map(|remaining| remaining.as_millis().min(i32::MAX as u128) as i32)
        .unwrap_or(250);
    if timeout.is_some_and(|limit| started.elapsed() >= limit) {
        return Ok(None);
    }
    let mut descriptor = libc::pollfd {
        fd: inotify.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: descriptor points to one initialized pollfd for the duration of poll.
    let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if ready < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(Some(Vec::new()));
        }
        return Err(error.to_string());
    }
    if ready == 0 {
        return if timeout.is_some() {
            Ok(None)
        } else {
            Ok(Some(Vec::new()))
        };
    }
    let events = inotify
        .read_events(buffer)
        .map_err(|error| error.to_string())?
        .map(|event| OwnedEvent {
            wd: event.wd.clone(),
            mask: event.mask,
            cookie: event.cookie,
            name: event.name.map(OsStr::to_owned),
        })
        .collect();
    Ok(Some(events))
}

fn establish(
    common: &Common,
    verbose: bool,
) -> Result<(Inotify, HashMap<WatchDescriptor, PathBuf>), String> {
    let mut inotify = Inotify::init().map_err(|error| error.to_string())?;
    let mut watches = HashMap::new();
    for path in &common.paths {
        if verbose && common.recursive {
            eprintln!("Setting up watch(es) on {}", path.display());
        }
        add_tree(&mut inotify, &mut watches, path, common, false)?;
        if verbose && common.recursive {
            eprintln!("OK, {} is now being watched.", path.display());
        }
    }
    Ok((inotify, watches))
}

fn add_tree(
    inotify: &mut Inotify,
    watches: &mut HashMap<WatchDescriptor, PathBuf>,
    root: &Path,
    common: &Common,
    missing_ok: bool,
) -> Result<(), String> {
    if common.path_excluded(root) {
        return Ok(());
    }
    let metadata = match if common.no_dereference {
        std::fs::symlink_metadata(root)
    } else {
        std::fs::metadata(root)
    } {
        Ok(metadata) => metadata,
        Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    };
    let mut watch_mask = common.mask;
    if common.recursive {
        watch_mask |= WatchMask::CREATE | WatchMask::MOVED_TO;
    }
    if common.no_dereference {
        watch_mask |= WatchMask::DONT_FOLLOW;
    }
    let descriptor = match inotify.watches().add(root, watch_mask) {
        Ok(descriptor) => descriptor,
        Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    };
    watches.insert(descriptor, root.to_path_buf());
    if !common.recursive || !metadata.is_dir() {
        return Ok(());
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        if common.path_excluded(&directory) {
            continue;
        }
        if directory != root {
            let descriptor = match inotify.watches().add(&directory, watch_mask) {
                Ok(descriptor) => descriptor,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("{}: {error}", directory.display())),
            };
            watches.insert(descriptor, directory.clone());
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error)
                if (missing_ok || directory != root)
                    && error.kind() == std::io::ErrorKind::NotFound =>
            {
                continue;
            }
            Err(error) => return Err(format!("{}: {error}", directory.display())),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("{}: {error}", directory.display())),
            };
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{}: {error}", entry.path().display())),
            }
        }
    }
    Ok(())
}

fn parse_event(value: &str) -> Result<(EventMask, Vec<&'static str>), String> {
    match value.to_ascii_lowercase().as_str() {
        "access" => Ok((EventMask::ACCESS, vec!["access"])),
        "modify" => Ok((EventMask::MODIFY, vec!["modify"])),
        "attrib" => Ok((EventMask::ATTRIB, vec!["attrib"])),
        "close_write" => Ok((EventMask::CLOSE_WRITE, vec!["close_write"])),
        "close_nowrite" => Ok((EventMask::CLOSE_NOWRITE, vec!["close_nowrite"])),
        "close" => Ok((
            EventMask::CLOSE_WRITE | EventMask::CLOSE_NOWRITE,
            vec!["close_write", "close_nowrite"],
        )),
        "open" => Ok((EventMask::OPEN, vec!["open"])),
        "moved_to" => Ok((EventMask::MOVED_TO, vec!["moved_to"])),
        "moved_from" => Ok((EventMask::MOVED_FROM, vec!["moved_from"])),
        "move" => Ok((
            EventMask::MOVED_FROM | EventMask::MOVED_TO,
            vec!["moved_to", "moved_from"],
        )),
        "move_self" => Ok((EventMask::MOVE_SELF, vec!["move_self"])),
        "create" => Ok((EventMask::CREATE, vec!["create"])),
        "delete" => Ok((EventMask::DELETE, vec!["delete"])),
        "delete_self" => Ok((EventMask::DELETE_SELF, vec!["delete_self"])),
        "unmount" => Ok((EventMask::UNMOUNT, vec!["unmount"])),
        _ => Err(format!("unknown event '{value}'")),
    }
}

fn event_names(mask: EventMask) -> Vec<&'static str> {
    [
        (EventMask::ACCESS, "ACCESS"),
        (EventMask::MODIFY, "MODIFY"),
        (EventMask::ATTRIB, "ATTRIB"),
        (EventMask::CLOSE_WRITE, "CLOSE_WRITE,CLOSE"),
        (EventMask::CLOSE_NOWRITE, "CLOSE_NOWRITE,CLOSE"),
        (EventMask::OPEN, "OPEN"),
        (EventMask::MOVED_FROM, "MOVED_FROM,MOVE"),
        (EventMask::MOVED_TO, "MOVED_TO,MOVE"),
        (EventMask::MOVE_SELF, "MOVE_SELF"),
        (EventMask::CREATE, "CREATE"),
        (EventMask::DELETE, "DELETE"),
        (EventMask::DELETE_SELF, "DELETE_SELF"),
        (EventMask::UNMOUNT, "UNMOUNT"),
    ]
    .into_iter()
    .filter_map(|(event, name)| mask.contains(event).then_some(name))
    .collect()
}

fn event_counter_names(mask: EventMask) -> Vec<&'static str> {
    [
        (EventMask::ACCESS, "access"),
        (EventMask::MODIFY, "modify"),
        (EventMask::ATTRIB, "attrib"),
        (EventMask::CLOSE_WRITE, "close_write"),
        (EventMask::CLOSE_NOWRITE, "close_nowrite"),
        (EventMask::OPEN, "open"),
        (EventMask::MOVED_FROM, "moved_from"),
        (EventMask::MOVED_TO, "moved_to"),
        (EventMask::MOVE_SELF, "move_self"),
        (EventMask::CREATE, "create"),
        (EventMask::DELETE, "delete"),
        (EventMask::DELETE_SELF, "delete_self"),
        (EventMask::UNMOUNT, "unmount"),
    ]
    .into_iter()
    .filter_map(|(event, name)| mask.contains(event).then_some(name))
    .collect()
}

fn event_columns() -> &'static [&'static str] {
    &[
        "access",
        "modify",
        "attrib",
        "close_write",
        "close_nowrite",
        "open",
        "moved_from",
        "moved_to",
        "move_self",
        "create",
        "delete",
        "delete_self",
        "unmount",
    ]
}

fn path_selected(path: &Path, common: &Common) -> bool {
    let value = path.as_os_str().as_bytes();
    !common
        .exclude
        .as_ref()
        .is_some_and(|regex| regex.is_match(value))
        && common
            .include
            .as_ref()
            .is_none_or(|regex| regex.is_match(value))
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        if component == std::path::Component::ParentDir {
            normalized.pop();
        } else {
            normalized.push(component.as_os_str());
        }
    }
    Ok(normalized)
}

fn watched_name(path: &Path) -> Vec<u8> {
    let mut value = path.as_os_str().as_bytes().to_vec();
    if path.is_dir() && !value.ends_with(b"/") {
        value.push(b'/');
    }
    value
}

fn csv_field(value: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(value.len() + 2);
    output.push(b'"');
    for byte in value {
        output.push(*byte);
        if *byte == b'"' {
            output.push(b'"');
        }
    }
    output.push(b'"');
    output
}
fn parse_timeout(value: Option<OsString>) -> Result<Option<Duration>, String> {
    let seconds = text_argument(value, "timeout requires an argument")?
        .parse::<u64>()
        .map_err(|_| "invalid timeout")?;
    Ok((seconds > 0).then(|| Duration::from_secs(seconds)))
}

fn signal_flag() -> Result<Arc<AtomicBool>, String> {
    let flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, flag.clone())
        .map_err(|error| error.to_string())?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, flag.clone())
        .map_err(|error| error.to_string())?;
    Ok(flag)
}

fn text_argument(value: Option<OsString>, missing: &str) -> Result<String, String> {
    value
        .ok_or_else(|| missing.to_owned())?
        .into_string()
        .map_err(|_| "option value is not valid UTF-8".to_owned())
}

fn render_format(
    format: &str,
    watched: &[u8],
    filename: &[u8],
    events: &str,
    cookie: u32,
    time_format: &str,
) -> Vec<u8> {
    let mut output = Vec::new();
    let mut chars = format.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '%' {
            let mut encoded = [0; 4];
            output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            continue;
        }
        match chars.next() {
            Some('w') => output.extend_from_slice(watched),
            Some('f') => output.extend_from_slice(filename),
            Some('e') => output.extend_from_slice(events.as_bytes()),
            Some('c') => output.extend_from_slice(cookie.to_string().as_bytes()),
            Some('T') => {
                output.extend_from_slice(Local::now().format(time_format).to_string().as_bytes())
            }
            Some('0') => output.push(0),
            Some('n') => output.push(b'\n'),
            Some('%') => output.push(b'%'),
            Some(separator) if chars.peek() == Some(&'e') => {
                chars.next();
                output.extend_from_slice(events.replace(',', &separator.to_string()).as_bytes());
            }
            Some(other) => {
                output.push(b'%');
                let mut encoded = [0; 4];
                output.extend_from_slice(other.encode_utf8(&mut encoded).as_bytes());
            }
            None => output.push(b'%'),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_conversions_match_inotifywait() {
        assert_eq!(
            render_format("%w%f %e %Xe%n", b"/tmp/", b"file", "CREATE,CLOSE", 0, "%s"),
            b"/tmp/file CREATE,CLOSE CREATEXCLOSE\n"
        );
    }

    #[test]
    fn csv_doubles_quotes() {
        assert_eq!(csv_field(b"a\"b"), b"\"a\"\"b\"");
    }
}
