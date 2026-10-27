use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::RawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use regex::bytes::{Regex, RegexBuilder};

const FAN_CLOEXEC: u64 = 0x0000_0001;
const FAN_NONBLOCK: u64 = 0x0000_0002;
const FAN_MARK_ADD: u64 = 0x0000_0001;
const FAN_MARK_DONT_FOLLOW: u64 = 0x0000_0004;
const FAN_MARK_MOUNT: u64 = 0x0000_0010;
const FAN_MARK_FILESYSTEM: u64 = 0x0000_0100;
const FAN_EVENT_ON_CHILD: u64 = 0x0800_0000;
const FAN_ONDIR: u64 = 0x4000_0000;
const FAN_EVENT_METADATA_LEN: usize = 24;
const FAN_EVENT_VERSION: u8 = 3;
const IN_ACCESS: u64 = 0x0000_0001;
const IN_MODIFY: u64 = 0x0000_0002;
const IN_ATTRIB: u64 = 0x0000_0004;
const IN_CLOSE_WRITE: u64 = 0x0000_0008;
const IN_CLOSE_NOWRITE: u64 = 0x0000_0010;
const IN_OPEN: u64 = 0x0000_0020;
const IN_MOVED_FROM: u64 = 0x0000_0040;
const IN_MOVED_TO: u64 = 0x0000_0080;
const IN_CREATE: u64 = 0x0000_0100;
const IN_DELETE: u64 = 0x0000_0200;
const IN_DELETE_SELF: u64 = 0x0000_0400;
const IN_MOVE_SELF: u64 = 0x0000_0800;
const IN_UNMOUNT: u64 = 0x0000_2000;
const IN_ISDIR: u64 = 0x4000_0000;
const IN_ALL_EVENTS: u64 = IN_ACCESS
    | IN_MODIFY
    | IN_ATTRIB
    | IN_CLOSE_WRITE
    | IN_CLOSE_NOWRITE
    | IN_OPEN
    | IN_MOVED_FROM
    | IN_MOVED_TO
    | IN_CREATE
    | IN_DELETE
    | IN_DELETE_SELF
    | IN_MOVE_SELF;

pub(crate) fn uses_fanotify(args: &[OsString]) -> bool {
    args.iter().skip(1).any(|value| {
        let bytes = value.as_bytes();
        bytes == b"--fanotify"
            || bytes == b"--filesystem"
            || bytes == b"--mount"
            || bytes == b"-F"
            || bytes == b"-S"
            || bytes == b"-M"
            || (bytes.starts_with(b"-") && !bytes.starts_with(b"--") && bytes[1..].contains(&b'F'))
            || (bytes.starts_with(b"-") && !bytes.starts_with(b"--") && bytes[1..].contains(&b'S'))
            || (bytes.starts_with(b"-") && !bytes.starts_with(b"--") && bytes[1..].contains(&b'M'))
    })
}

pub fn fsnotifywait(args: Vec<OsString>) -> i32 {
    run_wait(args, true, "fsnotifywait")
}

pub fn fsnotifywatch(args: Vec<OsString>) -> i32 {
    run_watch(args, true, "fsnotifywatch")
}

pub(crate) fn fanotifywait(args: Vec<OsString>) -> i32 {
    run_wait(args, true, "inotifywait")
}

pub(crate) fn fanotifywatch(args: Vec<OsString>) -> i32 {
    run_watch(args, true, "inotifywatch")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Inode,
    Mount,
    Filesystem,
}

impl Scope {
    fn name(self) -> &'static str {
        match self {
            Self::Inode => "inode",
            Self::Mount => "mount",
            Self::Filesystem => "filesystem",
        }
    }
}

struct Options {
    paths: Vec<PathBuf>,
    excluded_paths: Vec<PathBuf>,
    recursive: bool,
    no_dereference: bool,
    timeout: Option<Duration>,
    mask: u64,
    output_mask: u64,
    selected_events: Vec<&'static str>,
    events_specified: bool,
    exclude: Option<Regex>,
    include: Option<Regex>,
    scope: Scope,
    monitor: bool,
    quiet: u8,
    csv: bool,
    format: Option<String>,
    time_format: String,
    newline: bool,
    output: Option<PathBuf>,
    fromfile: Option<PathBuf>,
    verbose: bool,
    zero: bool,
    sort_event: String,
    ascending: bool,
}

impl Options {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            excluded_paths: Vec::new(),
            recursive: false,
            no_dereference: false,
            timeout: None,
            mask: IN_ALL_EVENTS,
            output_mask: IN_ALL_EVENTS,
            selected_events: event_columns().to_vec(),
            events_specified: false,
            exclude: None,
            include: None,
            scope: Scope::Inode,
            monitor: false,
            quiet: 0,
            csv: false,
            format: None,
            time_format: "%Y-%m-%d %H:%M:%S".into(),
            newline: true,
            output: None,
            fromfile: None,
            verbose: false,
            zero: false,
            sort_event: "total".into(),
            ascending: false,
        }
    }

    fn apply_event(&mut self, value: &str) -> Result<(), String> {
        if !self.events_specified {
            self.events_specified = true;
            self.output_mask = 0;
            self.selected_events.clear();
        }
        for value in value.split(',') {
            let (mask, names) = parse_event(value)?;
            self.output_mask |= mask;
            for name in names {
                if !self.selected_events.contains(&name) {
                    self.selected_events.push(name);
                }
            }
        }
        self.mask = self.output_mask;
        Ok(())
    }

    fn add_operand(&mut self, value: OsString) -> Result<(), String> {
        if let Some(path) = value.as_bytes().strip_prefix(b"@") {
            if path.is_empty() {
                return Ok(());
            }
            self.excluded_paths
                .push(absolute_path(Path::new(OsStr::from_bytes(path)))?);
        } else {
            self.paths.push(PathBuf::from(value));
        }
        Ok(())
    }

    fn path_excluded(&self, path: &Path) -> bool {
        absolute_path(path).is_ok_and(|absolute| {
            self.excluded_paths
                .iter()
                .any(|excluded| absolute.starts_with(excluded))
        })
    }
}

fn run_wait(args: Vec<OsString>, force_fanotify: bool, name: &str) -> i32 {
    match parse_options(args, false, force_fanotify, name).and_then(wait_inner) {
        Ok(()) => 0,
        Err(error) if error.is_empty() => 0,
        Err(error) if error == "timeout" => 2,
        Err(error) => {
            eprintln!("{name}: {error}");
            1
        }
    }
}

fn run_watch(args: Vec<OsString>, force_fanotify: bool, name: &str) -> i32 {
    match parse_options(args, true, force_fanotify, name).and_then(watch_inner) {
        Ok(()) => 0,
        Err(error) if error.is_empty() => 0,
        Err(error) => {
            eprintln!("{name}: {error}");
            1
        }
    }
}

fn parse_options(
    args: Vec<OsString>,
    watch: bool,
    force_fanotify: bool,
    name: &str,
) -> Result<Options, String> {
    let mut options = Options::new();
    let mut iter = args.into_iter().skip(1);
    while let Some(value) = iter.next() {
        let bytes = value.as_bytes();
        match bytes {
            b"-m" | b"--monitor" => options.monitor = true,
            b"-r" | b"--recursive" => options.recursive = true,
            b"-P" | b"--no-dereference" => options.no_dereference = true,
            b"-q" | b"--quiet" => options.quiet = options.quiet.saturating_add(1),
            b"-c" | b"--csv" if !watch => options.csv = true,
            b"-v" | b"--verbose" if watch => options.verbose = true,
            b"-z" | b"--zero" if watch => options.zero = true,
            b"-F" | b"--fanotify" => {}
            b"-I" | b"--inotify" => {
                if force_fanotify {
                    return Err("--inotify cannot be used with fsnotify".into());
                }
            }
            b"-S" | b"--filesystem" => options.scope = Scope::Filesystem,
            b"-M" | b"--mount" => options.scope = Scope::Mount,
            b"-e" | b"--event" => options.apply_event(&text_argument(
                iter.next(),
                "event option requires an argument",
            )?)?,
            b"-t" | b"--timeout" => options.timeout = parse_timeout(iter.next())?,
            b"--format" | b"-n" if !watch => {
                options.format = Some(text_argument(iter.next(), "--format requires an argument")?)
            }
            b"--timefmt" | b"-i" if !watch => {
                options.time_format = text_argument(iter.next(), "--timefmt requires an argument")?
            }
            b"--no-newline" | b"-0" if !watch => options.newline = false,
            b"-o" | b"--outfile" if !watch => {
                options.output = Some(PathBuf::from(
                    iter.next().ok_or("--outfile requires an argument")?,
                ))
            }
            b"--fromfile" | b"-z" if !watch => {
                options.fromfile = Some(PathBuf::from(
                    iter.next().ok_or("--fromfile requires an argument")?,
                ))
            }
            b"--exclude" | b"-a" if !watch => {
                options.exclude = Some(path_regex(
                    &text_argument(iter.next(), "--exclude requires a pattern")?,
                    false,
                )?)
            }
            b"--excludei" | b"-b" if !watch => {
                options.exclude = Some(path_regex(
                    &text_argument(iter.next(), "--excludei requires a pattern")?,
                    true,
                )?)
            }
            b"--include" | b"-j" if !watch => {
                options.include = Some(path_regex(
                    &text_argument(iter.next(), "--include requires a pattern")?,
                    false,
                )?)
            }
            b"--includei" | b"-k" if !watch => {
                options.include = Some(path_regex(
                    &text_argument(iter.next(), "--includei requires a pattern")?,
                    true,
                )?)
            }
            b"--exclude" if watch => {
                options.exclude = Some(path_regex(
                    &text_argument(iter.next(), "--exclude requires a pattern")?,
                    false,
                )?)
            }
            b"--include" if watch => {
                options.include = Some(path_regex(
                    &text_argument(iter.next(), "--include requires a pattern")?,
                    false,
                )?)
            }
            b"-a" if watch => {
                options.ascending = true;
                options.sort_event = text_argument(iter.next(), "ascending requires an event")?;
            }
            b"-d" if watch => {
                options.ascending = false;
                options.sort_event = text_argument(iter.next(), "descending requires an event")?;
            }
            b"-h" | b"--help" => {
                print_help(watch, name);
                return Err(String::new());
            }
            b"--" => {
                for path in iter {
                    options.add_operand(path)?;
                }
                break;
            }
            value if value.starts_with(b"-") => {
                return Err(format!(
                    "unknown option '{}'",
                    String::from_utf8_lossy(value)
                ));
            }
            _ => options.add_operand(value)?,
        }
    }
    if options.paths.is_empty() {
        return Err("no files specified to watch".into());
    }
    if !watch && options.csv && options.format.is_some() {
        return Err("--csv and --format cannot be combined".into());
    }
    if !watch && options.format.is_none() && !options.newline {
        return Err("--no-newline cannot be specified without --format".into());
    }
    if !watch && options.format.is_none() && options.time_format != "%Y-%m-%d %H:%M:%S" {
        return Err("--timefmt cannot be specified without --format".into());
    }
    if watch
        && options.sort_event != "total"
        && !event_columns().contains(&options.sort_event.as_str())
    {
        return Err(format!("unknown sort event '{}'", options.sort_event));
    }
    if let Some(format) = &options.format
        && format.contains("%T")
        && options.time_format == "%Y-%m-%d %H:%M:%S"
    {
        return Err("%T is in --format string, but --timefmt was not specified".into());
    }
    if !options.events_specified && options.scope != Scope::Inode {
        options.mask = IN_ALL_EVENTS & !(IN_DELETE_SELF | IN_MOVE_SELF);
        options.output_mask = options.mask;
    }
    if force_fanotify {
        options.mask |= IN_ISDIR;
        options.output_mask &= !IN_ISDIR;
    }
    load_from_file(&mut options)?;
    Ok(options)
}

fn load_from_file(options: &mut Options) -> Result<(), String> {
    let Some(path) = options.fromfile.take() else {
        return Ok(());
    };
    let mut data = Vec::new();
    if path.as_os_str().as_bytes() == b"-" {
        std::io::stdin()
            .read_to_end(&mut data)
            .map_err(|error| error.to_string())?;
    } else {
        data = std::fs::read(path).map_err(|error| error.to_string())?;
    }
    for line in data.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if !line.is_empty() {
            options.add_operand(OsString::from_vec(line.to_vec()))?;
        }
    }
    if options.paths.is_empty() {
        return Err("no files specified to watch".into());
    }
    Ok(())
}

fn wait_inner(options: Options) -> Result<(), String> {
    if options.quiet == 0 {
        if options.scope == Scope::Inode && options.recursive {
            eprintln!("Setting up watches.  Beware: since -r was given, this may take a while!");
        } else {
            eprintln!("Setting up {} watches.", options.scope.name());
        }
    }
    let (mut fanotify, mut watched) = establish(&options)?;
    if options.quiet == 0 {
        eprintln!("Watches established.");
    }
    let stopped = signal_flag()?;
    let mut output: Box<dyn Write> = match &options.output {
        Some(path) => Box::new(std::fs::File::create(path).map_err(|error| error.to_string())?),
        None => Box::new(std::io::stdout().lock()),
    };
    let started = Instant::now();
    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        if stopped.load(Ordering::Relaxed) {
            return Ok(());
        }
        let events = read_events(&mut fanotify, &mut buffer, options.timeout, started)?;
        if events.is_empty() {
            if options
                .timeout
                .is_some_and(|limit| started.elapsed() >= limit)
            {
                return Err(String::from("timeout"));
            }
            continue;
        }
        for event in events {
            let Some(path) = event.path else {
                continue;
            };
            if options.scope == Scope::Inode
                && options.recursive
                && event.mask & FAN_ONDIR != 0
                && event.mask & (IN_CREATE | IN_MOVED_TO) != 0
            {
                add_tree(&fanotify, &mut watched, &path, &options, true)?;
            }
            let selected = event.mask & options.output_mask;
            if selected == 0 || !path_selected(&path, &options) {
                continue;
            }
            if options.quiet < 2 {
                write_event(&mut output, &path, event.mask, &options)?;
            }
            if !options.monitor {
                return Ok(());
            }
        }
    }
}

fn watch_inner(options: Options) -> Result<(), String> {
    eprintln!("Establishing watches...");
    let (mut fanotify, mut watched) = establish(&options)?;
    if options.verbose {
        eprintln!("Total of {} watches.", watched.len());
    }
    eprintln!("Finished establishing watches, now collecting statistics.");
    if let Some(timeout) = options.timeout {
        eprintln!("Will listen for events for {} seconds.", timeout.as_secs());
    }
    let stopped = signal_flag()?;
    let started = Instant::now();
    let mut buffer = vec![0u8; 128 * 1024];
    let mut counts = BTreeMap::<PathBuf, BTreeMap<&'static str, u64>>::new();
    for path in &watched {
        counts.entry(path.clone()).or_default();
    }
    loop {
        if stopped.load(Ordering::Relaxed) {
            break;
        }
        let events = read_events(&mut fanotify, &mut buffer, options.timeout, started)?;
        if events.is_empty() {
            if options
                .timeout
                .is_some_and(|limit| started.elapsed() >= limit)
            {
                break;
            }
            continue;
        }
        for event in events {
            let Some(path) = event.path else {
                continue;
            };
            if options.scope == Scope::Inode
                && options.recursive
                && event.mask & FAN_ONDIR != 0
                && event.mask & (IN_CREATE | IN_MOVED_TO) != 0
            {
                let before = watched.len();
                add_tree(&fanotify, &mut watched, &path, &options, true)?;
                for path in &watched[before..] {
                    counts.entry(path.clone()).or_default();
                }
            }
            let selected = event.mask & options.output_mask;
            if selected == 0 || !path_selected(&path, &options) {
                continue;
            }
            let row = counts.entry(path).or_default();
            for name in event_counter_names(selected) {
                *row.entry(name).or_default() += 1;
            }
        }
    }
    print_counts(counts, &options).map_err(|error| error.to_string())
}

struct Fanotify {
    fd: RawFd,
}

impl Drop for Fanotify {
    fn drop(&mut self) {
        // SAFETY: fd is owned by this object and is closed exactly once.
        unsafe { libc::close(self.fd) };
    }
}

fn establish(options: &Options) -> Result<(Fanotify, Vec<PathBuf>), String> {
    let fd = unsafe {
        libc::syscall(
            libc::SYS_fanotify_init,
            (FAN_CLOEXEC | FAN_NONBLOCK) as libc::c_long,
            (libc::O_RDONLY | libc::O_LARGEFILE) as libc::c_long,
        )
    };
    if fd < 0 {
        return Err(format!("fanotify: {}", std::io::Error::last_os_error()));
    }
    let fanotify = Fanotify { fd: fd as RawFd };
    let mut watched = Vec::new();
    if options.scope != Scope::Inode {
        for path in &options.paths {
            mark_path(&fanotify, path, options, false)?;
            watched.push(path.clone());
        }
    } else {
        for path in &options.paths {
            add_tree(&fanotify, &mut watched, path, options, false)?;
        }
    }
    Ok((fanotify, watched))
}

fn add_tree(
    fanotify: &Fanotify,
    watched: &mut Vec<PathBuf>,
    root: &Path,
    options: &Options,
    missing_ok: bool,
) -> Result<(), String> {
    let metadata = match if options.no_dereference {
        std::fs::symlink_metadata(root)
    } else {
        std::fs::metadata(root)
    } {
        Ok(metadata) => metadata,
        Err(error)
            if missing_ok
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(format!("{}: {error}", root.display())),
    };
    if options.path_excluded(root) {
        return Ok(());
    }
    mark_path(fanotify, root, options, metadata.is_dir())?;
    watched.push(root.to_path_buf());
    if !options.recursive || !metadata.is_dir() {
        return Ok(());
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
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
            if options.path_excluded(&entry.path()) {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => {
                    let child = entry.path();
                    match mark_path(fanotify, &child, options, true) {
                        Ok(()) => {
                            watched.push(child.clone());
                            stack.push(child);
                        }
                        Err(error) if is_transient_error(&error) => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(_) => {}
                Err(error) if is_transient_io_error(&error) => {}
                Err(error) => return Err(format!("{}: {error}", entry.path().display())),
            }
        }
    }
    Ok(())
}

fn mark_path(
    fanotify: &Fanotify,
    path: &Path,
    options: &Options,
    is_directory: bool,
) -> Result<(), String> {
    let mut flags = FAN_MARK_ADD;
    flags |= match options.scope {
        Scope::Inode => 0,
        Scope::Mount => FAN_MARK_MOUNT,
        Scope::Filesystem => FAN_MARK_FILESYSTEM,
    };
    if options.no_dereference {
        flags |= FAN_MARK_DONT_FOLLOW;
    }
    let mut mask = options.mask;
    if options.scope == Scope::Inode && is_directory {
        mask |= FAN_EVENT_ON_CHILD;
    }
    let path_bytes = path.as_os_str().as_bytes();
    let cpath = std::ffi::CString::new(path_bytes)
        .map_err(|_| format!("{}: path contains NUL", path.display()))?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_fanotify_mark,
            fanotify.fd as libc::c_long,
            flags as libc::c_long,
            mask as libc::c_long,
            libc::AT_FDCWD as libc::c_long,
            cpath.as_ptr(),
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        return Err(format!("{}: {error}", path.display()));
    }
    Ok(())
}

struct Event {
    mask: u64,
    path: Option<PathBuf>,
}

fn read_events(
    fanotify: &mut Fanotify,
    buffer: &mut [u8],
    timeout: Option<Duration>,
    started: Instant,
) -> Result<Vec<Event>, String> {
    let timeout_ms = timeout
        .map(|limit| limit.saturating_sub(started.elapsed()))
        .map(|remaining| remaining.as_millis().min(i32::MAX as u128) as i32)
        .unwrap_or(250);
    if timeout.is_some_and(|limit| started.elapsed() >= limit) {
        return Ok(Vec::new());
    }
    let mut pollfd = libc::pollfd {
        fd: fanotify.fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pollfd is initialized and valid for the duration of poll.
    let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if ready < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(Vec::new());
        }
        return Err(error.to_string());
    }
    if ready == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: buffer is a valid writable byte slice and fd is owned by fanotify.
    let count = unsafe { libc::read(fanotify.fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if count < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) {
            return Ok(Vec::new());
        }
        return Err(error.to_string());
    }
    let mut offset = 0usize;
    let mut events = Vec::new();
    let count = count as usize;
    while offset + FAN_EVENT_METADATA_LEN <= count {
        let event_len = read_u32(buffer, offset) as usize;
        if event_len < FAN_EVENT_METADATA_LEN || offset + event_len > count {
            return Err("fanotify: malformed event length".into());
        }
        if buffer[offset + 4] != FAN_EVENT_VERSION {
            return Err("fanotify: unsupported event version".into());
        }
        let mask = read_u64(buffer, offset + 8);
        let fd = read_i32(buffer, offset + 16);
        let path = if fd >= 0 {
            let path = read_event_path(fd);
            // SAFETY: fanotify owns this event descriptor and it is closed once after parsing.
            unsafe { libc::close(fd) };
            Some(path?)
        } else {
            None
        };
        events.push(Event { mask, path });
        offset += event_len;
    }
    Ok(events)
}

fn read_event_path(fd: RawFd) -> Result<PathBuf, String> {
    let link = std::fs::read_link(format!("/proc/self/fd/{fd}"))
        .map_err(|error| format!("fanotify event fd {fd}: {error}"))?;
    let bytes = link.as_os_str().as_bytes();
    let path = bytes.strip_suffix(b" (deleted)").unwrap_or(bytes);
    Ok(PathBuf::from(OsString::from_vec(path.to_vec())))
}

fn write_event(
    output: &mut dyn Write,
    path: &Path,
    mask: u64,
    options: &Options,
) -> Result<(), String> {
    let (watched, filename) = split_event_path(path, mask & FAN_ONDIR != 0);
    let names = event_names(mask & options.output_mask).join(",");
    let rendered = if let Some(format) = &options.format {
        render_format(format, &watched, &filename, &names, 0, &options.time_format)
    } else if options.csv {
        let mut rendered = csv_field(&watched);
        rendered.push(b',');
        rendered.extend(csv_field(names.as_bytes()));
        rendered.push(b',');
        rendered.extend(csv_field(&filename));
        rendered
    } else if filename.is_empty() {
        [watched.as_slice(), b" ", names.as_bytes()].concat()
    } else {
        [
            watched.as_slice(),
            b" ",
            names.as_bytes(),
            b" ",
            filename.as_slice(),
        ]
        .concat()
    };
    output
        .write_all(&rendered)
        .map_err(|error| error.to_string())?;
    if options.newline && !rendered.ends_with(b"\n") {
        output.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    output.flush().map_err(|error| error.to_string())
}

fn split_event_path(path: &Path, is_directory: bool) -> (Vec<u8>, Vec<u8>) {
    let bytes = path.as_os_str().as_bytes();
    if is_directory {
        let mut watched = bytes.to_vec();
        if !watched.ends_with(b"/") {
            watched.push(b'/');
        }
        return (watched, Vec::new());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut watched = parent.as_os_str().as_bytes().to_vec();
    if !watched.ends_with(b"/") {
        watched.push(b'/');
    }
    let filename = path
        .file_name()
        .map(|name| name.as_bytes().to_vec())
        .unwrap_or_default();
    (watched, filename)
}

fn print_counts(
    counts: BTreeMap<PathBuf, BTreeMap<&'static str, u64>>,
    options: &Options,
) -> std::io::Result<()> {
    if !counts
        .values()
        .any(|row| row.values().any(|count| *count != 0))
    {
        eprintln!("No events occurred.");
        return Ok(());
    }
    let columns = options
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
        output.write_all(path.as_os_str().as_bytes())?;
        output.write_all(b"\n")?;
    }
    output.flush()
}

fn signal_flag() -> Result<Arc<AtomicBool>, String> {
    let flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, flag.clone())
        .map_err(|error| error.to_string())?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, flag.clone())
        .map_err(|error| error.to_string())?;
    Ok(flag)
}

fn parse_event(value: &str) -> Result<(u64, Vec<&'static str>), String> {
    match value.to_ascii_lowercase().as_str() {
        "access" => Ok((IN_ACCESS, vec!["access"])),
        "modify" => Ok((IN_MODIFY, vec!["modify"])),
        "attrib" => Ok((IN_ATTRIB, vec!["attrib"])),
        "close_write" => Ok((IN_CLOSE_WRITE, vec!["close_write"])),
        "close_nowrite" => Ok((IN_CLOSE_NOWRITE, vec!["close_nowrite"])),
        "close" => Ok((
            IN_CLOSE_WRITE | IN_CLOSE_NOWRITE,
            vec!["close_write", "close_nowrite"],
        )),
        "open" => Ok((IN_OPEN, vec!["open"])),
        "moved_to" => Ok((IN_MOVED_TO, vec!["moved_to"])),
        "moved_from" => Ok((IN_MOVED_FROM, vec!["moved_from"])),
        "move" => Ok((IN_MOVED_FROM | IN_MOVED_TO, vec!["moved_to", "moved_from"])),
        "move_self" => Ok((IN_MOVE_SELF, vec!["move_self"])),
        "create" => Ok((IN_CREATE, vec!["create"])),
        "delete" => Ok((IN_DELETE, vec!["delete"])),
        "delete_self" => Ok((IN_DELETE_SELF, vec!["delete_self"])),
        "unmount" => Ok((IN_UNMOUNT, vec!["unmount"])),
        _ => Err(format!("unknown event '{value}'")),
    }
}

fn event_names(mask: u64) -> Vec<&'static str> {
    [
        (IN_ACCESS, "ACCESS"),
        (IN_MODIFY, "MODIFY"),
        (IN_ATTRIB, "ATTRIB"),
        (IN_CLOSE_WRITE, "CLOSE_WRITE,CLOSE"),
        (IN_CLOSE_NOWRITE, "CLOSE_NOWRITE,CLOSE"),
        (IN_OPEN, "OPEN"),
        (IN_MOVED_FROM, "MOVED_FROM,MOVE"),
        (IN_MOVED_TO, "MOVED_TO,MOVE"),
        (IN_MOVE_SELF, "MOVE_SELF"),
        (IN_CREATE, "CREATE"),
        (IN_DELETE, "DELETE"),
        (IN_DELETE_SELF, "DELETE_SELF"),
    ]
    .into_iter()
    .filter_map(|(event, name)| (mask & event != 0).then_some(name))
    .collect()
}

fn event_counter_names(mask: u64) -> Vec<&'static str> {
    [
        (IN_ACCESS, "access"),
        (IN_MODIFY, "modify"),
        (IN_ATTRIB, "attrib"),
        (IN_CLOSE_WRITE, "close_write"),
        (IN_CLOSE_NOWRITE, "close_nowrite"),
        (IN_OPEN, "open"),
        (IN_MOVED_FROM, "moved_from"),
        (IN_MOVED_TO, "moved_to"),
        (IN_MOVE_SELF, "move_self"),
        (IN_CREATE, "create"),
        (IN_DELETE, "delete"),
        (IN_DELETE_SELF, "delete_self"),
    ]
    .into_iter()
    .filter_map(|(event, name)| (mask & event != 0).then_some(name))
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
    ]
}

fn path_selected(path: &Path, options: &Options) -> bool {
    let value = path.as_os_str().as_bytes();
    !options
        .exclude
        .as_ref()
        .is_some_and(|regex| regex.is_match(value))
        && options
            .include
            .as_ref()
            .is_none_or(|regex| regex.is_match(value))
}

fn path_regex(pattern: &str, case_insensitive: bool) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .unicode(false)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|error| error.to_string())
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

fn parse_timeout(value: Option<OsString>) -> Result<Option<Duration>, String> {
    let value = value.ok_or("timeout requires an argument")?;
    let seconds = value
        .to_str()
        .ok_or("timeout is not valid UTF-8")?
        .parse::<u64>()
        .map_err(|_| "invalid timeout")?;
    Ok((seconds > 0).then(|| Duration::from_secs(seconds)))
}

fn text_argument(value: Option<OsString>, missing: &str) -> Result<String, String> {
    value
        .ok_or_else(|| missing.to_owned())?
        .into_string()
        .map_err(|_| "option value is not valid UTF-8".to_owned())
}

fn read_u32(buffer: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap())
}

fn read_i32(buffer: &[u8], offset: usize) -> i32 {
    i32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap())
}

fn read_u64(buffer: &[u8], offset: usize) -> u64 {
    u64::from_ne_bytes(buffer[offset..offset + 8].try_into().unwrap())
}

fn is_transient_error(error: &str) -> bool {
    error.contains("No such file or directory") || error.contains("Permission denied")
}

fn is_transient_io_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    )
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
            Some('T') => output.extend_from_slice(
                chrono::Local::now()
                    .format(time_format)
                    .to_string()
                    .as_bytes(),
            ),
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

fn print_help(watch: bool, name: &str) {
    if watch {
        println!("Usage: {name} [-vzrP] [-e EVENT] [-t SECONDS] [-a|-d EVENT] [-S|-M] FILE ...");
    } else {
        println!("Usage: {name} [-cmrPq] [-e EVENT] [-t SECONDS] [--format FMT] [-S|-M] FILE ...");
    }
    println!("  -S, --filesystem  watch the entire filesystem containing FILE");
    println!("  -M, --mount       watch the entire mount containing FILE");
    println!("  -r, --recursive   watch directories recursively");
    println!("  -e, --event       select events");
    println!("  -t, --timeout     stop after the timeout");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanotify_event_names_use_inotify_compatible_labels() {
        assert_eq!(event_names(IN_CREATE | IN_DELETE), vec!["CREATE", "DELETE"]);
    }

    #[test]
    fn event_paths_split_directory_and_file_names() {
        assert_eq!(
            split_event_path(Path::new("/tmp/file"), false),
            (b"/tmp/".to_vec(), b"file".to_vec())
        );
        assert_eq!(
            split_event_path(Path::new("/tmp/dir"), true),
            (b"/tmp/dir/".to_vec(), Vec::new())
        );
    }
}
