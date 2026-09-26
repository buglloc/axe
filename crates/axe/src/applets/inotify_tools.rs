use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Local;
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use regex::Regex;

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
    recursive: bool,
    no_dereference: bool,
    timeout: Option<Duration>,
    mask: WatchMask,
    selected_events: Vec<&'static str>,
    exclude: Option<Regex>,
    include: Option<Regex>,
}

impl Common {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            recursive: false,
            no_dereference: false,
            timeout: None,
            mask: WatchMask::ALL_EVENTS,
            selected_events: event_columns().to_vec(),
            exclude: None,
            include: None,
        }
    }

    fn apply_event(&mut self, value: &str) -> Result<(), String> {
        if self.mask == WatchMask::ALL_EVENTS {
            self.mask = WatchMask::empty();
            self.selected_events.clear();
        }
        for value in value.split(',') {
            let (mask, names) = parse_event(value)?;
            self.mask.insert(mask);
            for name in names {
                if !self.selected_events.contains(&name) {
                    self.selected_events.push(name);
                }
            }
        }
        Ok(())
    }
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
        let values = utf8_args(args)?;
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
        let mut iter = values.into_iter().skip(1);
        while let Some(value) = iter.next() {
            match value.as_str() {
                "-m" | "--monitor" => options.monitor = true,
                "-r" | "--recursive" => options.common.recursive = true,
                "-P" | "--no-dereference" => options.common.no_dereference = true,
                "-q" | "--quiet" => options.quiet = options.quiet.saturating_add(1),
                "-c" | "--csv" => options.csv = true,
                "--no-newline" => options.newline = false,
                "-e" | "--event" => options
                    .common
                    .apply_event(&iter.next().ok_or("event option requires an argument")?)?,
                "-t" | "--timeout" => options.common.timeout = parse_timeout(iter.next())?,
                "--format" => {
                    options.format = Some(iter.next().ok_or("--format requires an argument")?)
                }
                "--timefmt" => {
                    options.time_format = iter.next().ok_or("--timefmt requires an argument")?
                }
                "-o" | "--outfile" => {
                    options.output = Some(PathBuf::from(
                        iter.next().ok_or("--outfile requires an argument")?,
                    ))
                }
                "--exclude" => {
                    options.common.exclude = Some(
                        Regex::new(&iter.next().ok_or("--exclude requires a pattern")?)
                            .map_err(|error| error.to_string())?,
                    )
                }
                "--excludei" => {
                    options.common.exclude = Some(
                        Regex::new(&format!(
                            "(?i:{})",
                            iter.next().ok_or("--excludei requires a pattern")?
                        ))
                        .map_err(|error| error.to_string())?,
                    )
                }
                "--include" => {
                    options.common.include = Some(
                        Regex::new(&iter.next().ok_or("--include requires a pattern")?)
                            .map_err(|error| error.to_string())?,
                    )
                }
                "--includei" => {
                    options.common.include = Some(
                        Regex::new(&format!(
                            "(?i:{})",
                            iter.next().ok_or("--includei requires a pattern")?
                        ))
                        .map_err(|error| error.to_string())?,
                    )
                }
                "-h" | "--help" => {
                    println!(
                        "Usage: inotifywait [-cmrPq] [-e EVENT] [-t SECONDS] [--format FMT] FILE ..."
                    );
                    return Err(String::new());
                }
                "-d" | "--daemon" | "-s" | "--syslog" => {
                    return Err(format!("unsupported option '{value}'"));
                }
                value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
                value if value.starts_with('@') => {}
                _ => options.common.paths.push(PathBuf::from(value)),
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
    let (mut inotify, mut watches) = establish(&options.common)?;
    if options.quiet == 0 {
        eprintln!("Setting up watches.");
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
            if !path_selected(&full, &options.common) {
                continue;
            }
            if options.common.recursive
                && event.mask.contains(EventMask::ISDIR)
                && (event.mask.contains(EventMask::CREATE)
                    || event.mask.contains(EventMask::MOVED_TO))
            {
                add_tree(
                    &mut inotify,
                    &mut watches,
                    &full,
                    options.common.mask,
                    options.common.no_dereference,
                )?;
            }
            if options.quiet < 2 {
                let names = event_names(event.mask).join(",");
                let watched = watched_name(&root);
                let filename = event
                    .name
                    .as_ref()
                    .map(|value| value.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let rendered = if let Some(format) = &options.format {
                    render_format(
                        format,
                        &watched,
                        &filename,
                        &names,
                        event.cookie,
                        &options.time_format,
                    )
                } else if options.csv {
                    format!(
                        "{},{},{}",
                        csv_field(&watched),
                        csv_field(&names),
                        csv_field(&filename)
                    )
                } else if filename.is_empty() {
                    format!("{watched} {names}")
                } else {
                    format!("{watched} {names} {filename}")
                };
                use std::io::Write;
                output
                    .write_all(rendered.as_bytes())
                    .map_err(|error| error.to_string())?;
                if options.newline && !rendered.ends_with('\n') {
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
        let values = utf8_args(args)?;
        let mut options = Self {
            common: Common::new(),
            zero: false,
            sort_event: "total".into(),
            ascending: false,
            verbose: false,
        };
        let mut iter = values.into_iter().skip(1);
        while let Some(value) = iter.next() {
            match value.as_str() {
                "-r" | "--recursive" => options.common.recursive = true,
                "-P" | "--no-dereference" => options.common.no_dereference = true,
                "-z" | "--zero" => options.zero = true,
                "-v" | "--verbose" => options.verbose = true,
                "-e" | "--event" => options
                    .common
                    .apply_event(&iter.next().ok_or("event option requires an argument")?)?,
                "-t" | "--timeout" => options.common.timeout = parse_timeout(iter.next())?,
                "-a" | "--ascending" => {
                    options.ascending = true;
                    options.sort_event = iter.next().ok_or("ascending requires an event")?;
                }
                "-d" | "--descending" => {
                    options.ascending = false;
                    options.sort_event = iter.next().ok_or("descending requires an event")?;
                }
                "--exclude" => {
                    options.common.exclude = Some(
                        Regex::new(&iter.next().ok_or("--exclude requires a pattern")?)
                            .map_err(|error| error.to_string())?,
                    )
                }
                "--include" => {
                    options.common.include = Some(
                        Regex::new(&iter.next().ok_or("--include requires a pattern")?)
                            .map_err(|error| error.to_string())?,
                    )
                }
                "-h" | "--help" => {
                    println!(
                        "Usage: inotifywatch [-vzrP] [-e EVENT] [-t SECONDS] [-a|-d EVENT] FILE ..."
                    );
                    return Err(String::new());
                }
                value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
                value if value.starts_with('@') => {}
                _ => options.common.paths.push(PathBuf::from(value)),
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
    let (mut inotify, mut watches) = establish(&options.common)?;
    if options.verbose {
        eprintln!("Watches established, collecting statistics.");
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
            if !path_selected(&full, &options.common) {
                continue;
            }
            if options.common.recursive
                && event.mask.contains(EventMask::ISDIR)
                && (event.mask.contains(EventMask::CREATE)
                    || event.mask.contains(EventMask::MOVED_TO))
            {
                add_tree(
                    &mut inotify,
                    &mut watches,
                    &full,
                    options.common.mask,
                    options.common.no_dereference,
                )?;
            }
            let row = counts.entry(root).or_default();
            for name in event_counter_names(event.mask) {
                *row.entry(name).or_default() += 1;
            }
        }
    }
    for path in watches.values() {
        counts.entry(path.clone()).or_default();
    }
    print_counts(counts, &options);
    Ok(())
}

fn print_counts(counts: BTreeMap<PathBuf, BTreeMap<&'static str, u64>>, options: &WatchOptions) {
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
    print!("total ");
    for column in &columns {
        print!("{column} ");
    }
    println!("filename");
    for (path, row) in rows {
        print!("{} ", row.values().sum::<u64>());
        for column in &columns {
            print!("{} ", row.get(column).copied().unwrap_or_default());
        }
        println!("{}", watched_name(&path));
    }
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
        return Err(std::io::Error::last_os_error().to_string());
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

fn establish(common: &Common) -> Result<(Inotify, HashMap<WatchDescriptor, PathBuf>), String> {
    let mut inotify = Inotify::init().map_err(|error| error.to_string())?;
    let mut watches = HashMap::new();
    for path in &common.paths {
        add_tree(
            &mut inotify,
            &mut watches,
            path,
            common.mask,
            common.no_dereference,
        )?;
    }
    Ok((inotify, watches))
}

fn add_tree(
    inotify: &mut Inotify,
    watches: &mut HashMap<WatchDescriptor, PathBuf>,
    root: &Path,
    mask: WatchMask,
    no_dereference: bool,
) -> Result<(), String> {
    let metadata = if no_dereference {
        std::fs::symlink_metadata(root)
    } else {
        std::fs::metadata(root)
    }
    .map_err(|error| format!("{}: {error}", root.display()))?;
    let watch_mask = if no_dereference {
        mask | WatchMask::DONT_FOLLOW
    } else {
        mask
    };
    let descriptor = inotify
        .watches()
        .add(root, watch_mask)
        .map_err(|error| format!("{}: {error}", root.display()))?;
    watches.insert(descriptor, root.to_path_buf());
    if !metadata.is_dir() {
        return Ok(());
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        if directory != root {
            let descriptor = inotify
                .watches()
                .add(&directory, watch_mask)
                .map_err(|error| format!("{}: {error}", directory.display()))?;
            watches.insert(descriptor, directory.clone());
        }
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}

fn parse_event(value: &str) -> Result<(WatchMask, Vec<&'static str>), String> {
    match value.to_ascii_lowercase().as_str() {
        "access" => Ok((WatchMask::ACCESS, vec!["access"])),
        "modify" => Ok((WatchMask::MODIFY, vec!["modify"])),
        "attrib" => Ok((WatchMask::ATTRIB, vec!["attrib"])),
        "close_write" => Ok((WatchMask::CLOSE_WRITE, vec!["close_write"])),
        "close_nowrite" => Ok((WatchMask::CLOSE_NOWRITE, vec!["close_nowrite"])),
        "close" => Ok((WatchMask::CLOSE, vec!["close_write", "close_nowrite"])),
        "open" => Ok((WatchMask::OPEN, vec!["open"])),
        "moved_to" => Ok((WatchMask::MOVED_TO, vec!["moved_to"])),
        "moved_from" => Ok((WatchMask::MOVED_FROM, vec!["moved_from"])),
        "move" => Ok((WatchMask::MOVE, vec!["moved_to", "moved_from"])),
        "move_self" => Ok((WatchMask::MOVE_SELF, vec!["move_self"])),
        "create" => Ok((WatchMask::CREATE, vec!["create"])),
        "delete" => Ok((WatchMask::DELETE, vec!["delete"])),
        "delete_self" => Ok((WatchMask::DELETE_SELF, vec!["delete_self"])),
        "unmount" => Ok((WatchMask::ALL_EVENTS, vec!["unmount"])),
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
    let value = path.to_string_lossy();
    !common
        .exclude
        .as_ref()
        .is_some_and(|regex| regex.is_match(&value))
        && common
            .include
            .as_ref()
            .is_none_or(|regex| regex.is_match(&value))
}
fn watched_name(path: &Path) -> String {
    let mut value = path.to_string_lossy().into_owned();
    if path.is_dir() && !value.ends_with('/') {
        value.push('/');
    }
    value
}
fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn parse_timeout(value: Option<String>) -> Result<Option<Duration>, String> {
    let seconds = value
        .ok_or("timeout requires an argument")?
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
fn utf8_args(args: Vec<OsString>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|value| {
            value.into_string().map_err(|value| {
                format!("argument is not valid UTF-8: {}", value.to_string_lossy())
            })
        })
        .collect()
}

fn render_format(
    format: &str,
    watched: &str,
    filename: &str,
    events: &str,
    cookie: u32,
    time_format: &str,
) -> String {
    let mut output = String::new();
    let mut chars = format.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '%' {
            output.push(character);
            continue;
        }
        match chars.next() {
            Some('w') => output.push_str(watched),
            Some('f') => output.push_str(filename),
            Some('e') => output.push_str(events),
            Some('c') => output.push_str(&cookie.to_string()),
            Some('T') => output.push_str(&Local::now().format(time_format).to_string()),
            Some('0') => output.push('\0'),
            Some('n') => output.push('\n'),
            Some('%') => output.push('%'),
            Some(separator) if chars.peek() == Some(&'e') => {
                chars.next();
                output.push_str(&events.replace(',', &separator.to_string()));
            }
            Some(other) => {
                output.push('%');
                output.push(other);
            }
            None => output.push('%'),
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
            render_format("%w%f %e %Xe%n", "/tmp/", "file", "CREATE,CLOSE", 0, "%s"),
            "/tmp/file CREATE,CLOSE CREATEXCLOSE\n"
        );
    }

    #[test]
    fn csv_doubles_quotes() {
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
    }
}
