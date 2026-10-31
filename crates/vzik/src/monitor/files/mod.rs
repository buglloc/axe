use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use serde_json::{Value, json};

use crate::cli::MonitorOptions;
use crate::procfs;
use crate::protocol::linux_bytes;

use super::contract::{Class, SourceContext};
use super::process::process_state;
use super::{MAX_FILE_STATE, MAX_WATCHES};

pub(super) struct FileSource {
    inotify: Inotify,
    watches: HashMap<WatchDescriptor, PathBuf>,
    roots: Vec<PathBuf>,
    path_glob: Option<glob::Pattern>,
    pub(super) setup_gaps: Vec<(String, String)>,
}

impl FileSource {
    pub(super) fn new(options: &MonitorOptions) -> io::Result<Self> {
        let inotify = Inotify::init()?;
        let mut watches = HashMap::new();
        let mut setup_gaps = Vec::new();
        let roots = options.filters.clone();
        let path_glob = options
            .path_glob
            .as_deref()
            .map(glob::Pattern::new)
            .transpose()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

        for root in &roots {
            let metadata = fs::symlink_metadata(root)?;
            if !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("file filter is not a directory: {}", root.display()),
                ));
            }
            let mut directories = vec![root.clone()];
            while let Some(directory) = directories.pop() {
                if watches.len() >= MAX_WATCHES {
                    setup_gaps.push(("watch_limit".to_owned(), directory.display().to_string()));
                    break;
                }
                match inotify.watches().add(
                    &directory,
                    WatchMask::CREATE
                        | WatchMask::DELETE
                        | WatchMask::MODIFY
                        | WatchMask::ATTRIB
                        | WatchMask::MOVED_FROM
                        | WatchMask::MOVED_TO
                        | WatchMask::DELETE_SELF
                        | WatchMask::MOVE_SELF,
                ) {
                    Ok(watch) => {
                        watches.insert(watch, directory.clone());
                    }
                    Err(error) if error.raw_os_error() == Some(libc::ENOSPC) => {
                        setup_gaps
                            .push(("watch_limit".to_owned(), directory.display().to_string()));
                        break;
                    }
                    Err(error) => {
                        setup_gaps.push((
                            "watch_unavailable".to_owned(),
                            directory.display().to_string(),
                        ));
                        if options.verbose {
                            setup_gaps.push(("watch_error".to_owned(), error.to_string()));
                        }
                        continue;
                    }
                }

                let entries = match fs::read_dir(&directory) {
                    Ok(entries) => entries,
                    Err(error) => {
                        setup_gaps.push((
                            "directory_unreadable".to_owned(),
                            directory.display().to_string(),
                        ));
                        if options.verbose {
                            setup_gaps.push(("directory_error".to_owned(), error.to_string()));
                        }
                        continue;
                    }
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
                        directories.push(path);
                    }
                }
            }
        }

        Ok(Self {
            inotify,
            watches,
            roots,
            path_glob,
            setup_gaps,
        })
    }

    fn matches(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| {
            let Ok(relative) = path.strip_prefix(root) else {
                return false;
            };
            self.path_glob
                .as_ref()
                .is_none_or(|pattern| pattern.matches_path(relative))
        })
    }

    fn path_for(&self, watch: &WatchDescriptor, name: Option<&OsStr>) -> Option<PathBuf> {
        let directory = self.watches.get(watch)?;
        Some(match name {
            Some(name) => directory.join(name),
            None => directory.clone(),
        })
    }
}

pub(super) struct FanotifySource {
    fd: RawFd,
    roots: Vec<PathBuf>,
    path_glob: Option<glob::Pattern>,
}

impl Drop for FanotifySource {
    fn drop(&mut self) {
        // SAFETY: the descriptor is created by fanotify_source and owned by this value.
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl FanotifySource {
    pub(super) fn new(options: &MonitorOptions) -> io::Result<Self> {
        let flags = libc::FAN_CLOEXEC | libc::FAN_NONBLOCK | libc::FAN_CLASS_NOTIF;
        let event_flags = (libc::O_RDONLY | libc::O_LARGEFILE | libc::O_CLOEXEC) as u32;
        // SAFETY: flags and event_flags are constant Linux fanotify parameters.
        let fd = unsafe { libc::fanotify_init(flags, event_flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let source = Self {
            fd,
            roots: options.filters.clone(),
            path_glob: options
                .path_glob
                .as_deref()
                .map(glob::Pattern::new)
                .transpose()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
        };
        let mask = libc::FAN_MODIFY
            | libc::FAN_CLOSE_WRITE
            | libc::FAN_ATTRIB
            | libc::FAN_CREATE
            | libc::FAN_DELETE
            | libc::FAN_MOVED_FROM
            | libc::FAN_MOVED_TO
            | libc::FAN_EVENT_ON_CHILD;
        for path in fanotify_mark_paths(&source.roots) {
            let bytes = path.as_os_str().as_bytes();
            let path = CString::new(bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in filter path"))?;
            // SAFETY: fd is owned by source; path is a valid NUL-terminated pathname.
            let result = unsafe {
                libc::fanotify_mark(
                    source.fd,
                    libc::FAN_MARK_ADD | libc::FAN_MARK_FILESYSTEM,
                    mask,
                    libc::AT_FDCWD,
                    path.as_ptr(),
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(source)
    }

    fn matches(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| {
            let Ok(relative) = path.strip_prefix(root) else {
                return false;
            };
            self.path_glob
                .as_ref()
                .is_none_or(|pattern| pattern.matches_path(relative))
        })
    }
}

fn fanotify_mark_paths(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = roots.to_vec();
    let Ok(mountinfo) = fs::read("/proc/self/mountinfo") else {
        return paths;
    };
    for line in mountinfo.split(|byte| *byte == b'\n') {
        let fields = line.split(|byte| *byte == b' ').collect::<Vec<_>>();
        let Some(mountpoint) = fields.get(4) else {
            continue;
        };
        let mountpoint = PathBuf::from(std::ffi::OsString::from_vec(procfs::unescape_mount(
            mountpoint,
        )));
        if roots.iter().any(|root| mountpoint.starts_with(root)) && !paths.contains(&mountpoint) {
            paths.push(mountpoint);
        }
    }
    paths
}
pub(super) fn fanotify_worker(context: SourceContext, source: FanotifySource) {
    let mut buffer = vec![0_u8; 64 << 10];
    while !context.stopped() {
        let mut pollfd = libc::pollfd {
            fd: source.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pollfd points to a valid local descriptor and has the declared length.
        let ready = unsafe { libc::poll(&mut pollfd, 1, 250) };
        if ready <= 0 {
            continue;
        }
        // SAFETY: buffer is valid for the requested length and source owns the descriptor.
        let length = unsafe { libc::read(source.fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if length < 0 {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EINTR)) {
                continue;
            }
            context.diagnostic(
                Some(Class::Files),
                "fanotify",
                "source_stopped",
                &error.to_string(),
            );
            break;
        }
        let mut offset = 0;
        while offset + 24 <= length as usize {
            let event_length =
                u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap_or_default())
                    as usize;
            if event_length < 24 || offset + event_length > length as usize {
                break;
            }
            let mask = u64::from_ne_bytes(
                buffer[offset + 8..offset + 16]
                    .try_into()
                    .unwrap_or_default(),
            );
            let fd = i32::from_ne_bytes(
                buffer[offset + 16..offset + 20]
                    .try_into()
                    .unwrap_or_default(),
            );
            let pid = i32::from_ne_bytes(
                buffer[offset + 20..offset + 24]
                    .try_into()
                    .unwrap_or_default(),
            );
            if mask & libc::FAN_Q_OVERFLOW != 0 {
                context.queue.send(
                    Some(Class::Files),
                    "fanotify",
                    json!({"type":"gap","class":"files","source":"fanotify","reason":"queue_overflow","dropped":0}),
                );
            } else if fd >= 0 {
                let path = fs::read_link(format!("/proc/self/fd/{fd}")).ok();
                // SAFETY: fanotify returns an owned event descriptor that must be closed.
                unsafe {
                    libc::close(fd);
                }
                if let Some(path) = path {
                    let path = strip_deleted_path(path);
                    if source.matches(&path) {
                        let event = if mask & (libc::FAN_MODIFY | libc::FAN_CLOSE_WRITE) != 0 {
                            Some("content_modified")
                        } else if mask & libc::FAN_ATTRIB != 0 {
                            Some("metadata_modified")
                        } else if mask
                            & (libc::FAN_CREATE
                                | libc::FAN_DELETE
                                | libc::FAN_MOVED_FROM
                                | libc::FAN_MOVED_TO)
                            != 0
                        {
                            Some("namespace_changed")
                        } else {
                            None
                        };
                        if let Some(event) = event {
                            let process = u32::try_from(pid)
                                .ok()
                                .filter(|pid| *pid > 0)
                                .and_then(|pid| process_state(pid, context.full))
                                .map(|state| state.value);
                            context.event(
                                Class::Files,
                                "fanotify",
                                event,
                                json!({
                                    "path":linux_bytes(path.as_os_str().as_bytes()),
                                    "object":file_value(&path),
                                    "process":process,
                                    "process_attribution":if pid > 0 {"available"} else {"unavailable"},
                                }),
                                "exact",
                            );
                        }
                    }
                }
            }
            offset += event_length;
        }
    }
}
fn strip_deleted_path(path: PathBuf) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    let suffix = b" (deleted)";
    if let Some(bytes) = bytes.strip_suffix(suffix) {
        PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
    } else {
        path
    }
}
pub(super) fn file_worker(context: SourceContext, mut source: FileSource) {
    for (reason, root) in &source.setup_gaps {
        context.queue.send(
            Some(Class::Files),
            "inotify",
            json!({"type":"gap","class":"files","source":"inotify","reason":reason,"root":root,"dropped":0}),
        );
    }
    let fd = source.inotify.as_raw_fd();
    // SAFETY: fd belongs to the inotify object held by this worker.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut buffer = vec![0_u8; 64 << 10];
    let mut moved_directories = HashMap::<u32, PathBuf>::new();
    while !context.stopped() {
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pollfd points to a valid local descriptor and has the declared length.
        let ready = unsafe { libc::poll(&mut pollfd, 1, 250) };
        if ready <= 0 {
            continue;
        }
        let events = match source.inotify.read_events(&mut buffer) {
            Ok(events) => events
                .map(|event| {
                    (
                        event.wd.clone(),
                        event.mask,
                        event.name.map(OsStr::to_os_string),
                        event.cookie,
                    )
                })
                .collect::<Vec<_>>(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => {
                context.diagnostic(
                    Some(Class::Files),
                    "inotify",
                    "source_stopped",
                    &error.to_string(),
                );
                break;
            }
        };
        for (watch, mask, name, cookie) in events {
            if mask.contains(EventMask::Q_OVERFLOW) {
                context.queue.send(
                    Some(Class::Files),
                    "inotify",
                    json!({"type":"gap","class":"files","source":"inotify","reason":"queue_overflow","dropped":0}),
                );
                continue;
            }
            let Some(path) = source.path_for(&watch, name.as_deref()) else {
                continue;
            };
            if mask.contains(EventMask::MOVED_FROM) && mask.contains(EventMask::ISDIR) {
                moved_directories.insert(cookie, path.clone());
            }
            if mask.contains(EventMask::MOVED_TO) && mask.contains(EventMask::ISDIR) {
                if let Some(old) = moved_directories.remove(&cookie) {
                    for watched in source.watches.values_mut() {
                        if watched.starts_with(&old) {
                            *watched = path
                                .join(watched.strip_prefix(&old).unwrap_or_else(|_| Path::new("")));
                        }
                    }
                } else {
                    let _ = add_directory_watches(&mut source, &path, &context);
                }
            }
            if !source.matches(&path) {
                continue;
            }
            let event = if mask.intersects(EventMask::MODIFY | EventMask::CLOSE_WRITE) {
                Some("content_modified")
            } else if mask.contains(EventMask::ATTRIB) {
                Some("metadata_modified")
            } else if mask.intersects(
                EventMask::CREATE | EventMask::DELETE | EventMask::MOVED_FROM | EventMask::MOVED_TO,
            ) {
                Some("namespace_changed")
            } else {
                None
            };
            if let Some(event) = event {
                let data = file_value(&path);
                context.event(
                    Class::Files,
                    "inotify",
                    event,
                    json!({
                        "path":linux_bytes(path.as_os_str().as_bytes()),
                        "object":data,
                        "process":Value::Null,
                        "process_attribution":"unavailable",
                    }),
                    "exact",
                );
            }
        }
    }
}
fn add_directory_watches(
    source: &mut FileSource,
    root: &Path,
    context: &SourceContext,
) -> io::Result<()> {
    let mut directories = vec![root.to_owned()];
    while let Some(directory) = directories.pop() {
        if source.watches.len() >= MAX_WATCHES {
            context.queue.send(
                Some(Class::Files),
                "inotify",
                json!({"type":"gap","class":"files","source":"inotify","reason":"watch_limit","root":directory,"dropped":0}),
            );
            break;
        }
        if let Ok(watch) = source.inotify.watches().add(
            &directory,
            WatchMask::CREATE
                | WatchMask::DELETE
                | WatchMask::MODIFY
                | WatchMask::ATTRIB
                | WatchMask::MOVED_FROM
                | WatchMask::MOVED_TO,
        ) {
            source.watches.insert(watch, directory.clone());
        }
        if let Ok(entries) = fs::read_dir(&directory) {
            for entry in entries.flatten() {
                if fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir()) {
                    directories.push(entry.path());
                }
            }
        }
    }
    Ok(())
}
fn file_value(path: &Path) -> Value {
    match fs::symlink_metadata(path) {
        Ok(metadata) => json!({
            "device":metadata.dev(),
            "inode":metadata.ino(),
            "is_directory":metadata.is_dir(),
            "mode":metadata.mode(),
        }),
        Err(error) => json!({"availability":"unavailable","error":error.kind().to_string()}),
    }
}
pub(super) fn file_snapshot(filters: &[PathBuf], path_glob: Option<&str>) -> Vec<Value> {
    let pattern = path_glob.and_then(|value| glob::Pattern::new(value).ok());
    let mut result = Vec::new();
    let mut directories = filters.to_vec();
    while let Some(directory) = directories.pop() {
        if result.len() >= MAX_FILE_STATE {
            break;
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if result.len() >= MAX_FILE_STATE {
                break;
            }
            let path = entry.path();
            let relative = path.strip_prefix(&directory).unwrap_or(&path);
            if pattern
                .as_ref()
                .is_none_or(|pattern| pattern.matches_path(relative))
            {
                result.push(json!({
                    "path":linux_bytes(path.as_os_str().as_bytes()),
                    "object":file_value(&path),
                }));
            }
            if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
                directories.push(path);
            }
        }
    }
    result
}
