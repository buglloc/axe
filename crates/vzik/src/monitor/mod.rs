use std::collections::HashMap;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::cli::MonitorOptions;
use crate::protocol::linux_bytes;

mod contract;
mod files;
mod process;
mod sockets;

use contract::{Class, DropCounts, Queue, QueuedRecord, SourceContext, parse_class};

const SCHEMA: &str = "raskop/vzik-monitor";
const SCHEMA_VERSION: u64 = 1;
pub(super) const QUEUE_CAPACITY: usize = 1024;
pub(super) const MAX_PROCESS_STATE: usize = 32_768;
pub(super) const MAX_SOCKET_STATE: usize = 65_536;
pub(super) const MAX_FILE_STATE: usize = 65_536;
pub(super) const MAX_WATCHES: usize = 65_536;
pub(super) const MAX_PAYLOAD: usize = 128 << 10;
pub(super) const PROCESS_SCAN_INTERVAL: Duration = Duration::from_millis(250);
pub(super) const SOCKET_SCAN_INTERVAL: Duration = Duration::from_millis(500);
pub(super) const MAX_FULL_FIELD: usize = 64 << 10;

struct Stats {
    source: String,
    detection: String,
    events: u64,
    gaps: u64,
    dropped: u64,
    evictions: u64,
}

impl Stats {
    fn new(source: &str, detection: &str) -> Self {
        Self {
            source: source.to_owned(),
            detection: detection.to_owned(),
            events: 0,
            gaps: 0,
            dropped: 0,
            evictions: 0,
        }
    }
}

struct Output<'a, W> {
    writer: &'a mut W,
    sequence: u64,
    events: u64,
    stats: HashMap<Class, Stats>,
}

impl<'a, W: Write> Output<'a, W> {
    fn new(writer: &'a mut W) -> Self {
        Self {
            writer,
            sequence: 0,
            events: 0,
            stats: HashMap::new(),
        }
    }

    fn write(&mut self, mut value: Value, sequenced: bool) -> io::Result<()> {
        value["schema"] = json!(SCHEMA);
        value["schema_version"] = json!(SCHEMA_VERSION);
        if sequenced {
            value["sequence"] = json!(self.sequence);
            self.sequence = self.sequence.saturating_add(1);
        }
        let mut line = serde_json::to_vec(&value).map_err(io::Error::other)?;
        line.push(b'\n');
        self.writer.write_all(&line)
    }

    fn event(&mut self, record: Value) -> io::Result<()> {
        let class = record["class"].as_str().and_then(|id| match id {
            "process" => Some(Class::Process),
            "files" => Some(Class::Files),
            "sockets" => Some(Class::Sockets),
            _ => None,
        });
        if let Some(class) = class {
            let stats = self
                .stats
                .entry(class)
                .or_insert_with(|| Stats::new("unknown", "unknown"));
            stats.events += 1;
        }
        self.events += 1;
        self.write(record, true)
    }
    fn queued(&mut self, record: QueuedRecord) -> io::Result<()> {
        let value = record.value;
        match value["type"].as_str() {
            Some("event") => self.event(value),
            Some("gap") => {
                let Some(class) = value["class"].as_str().and_then(parse_class) else {
                    return self.write(value, false);
                };
                let source = value["source"].as_str().unwrap_or(&record.source);
                let reason = value["reason"].as_str().unwrap_or("unknown");
                let dropped = value["dropped"].as_u64().unwrap_or(0);
                self.gap(class, source, reason, dropped)
            }
            _ => self.write(value, false),
        }
    }

    fn gap(&mut self, class: Class, source: &str, reason: &str, dropped: u64) -> io::Result<()> {
        let stats = self
            .stats
            .entry(class)
            .or_insert_with(|| Stats::new(source, "unknown"));
        stats.gaps += 1;
        stats.dropped = stats.dropped.saturating_add(dropped);
        self.write(
            json!({
                "type":"gap",
                "class":class.id(),
                "source":source,
                "reason":reason,
                "dropped":dropped,
            }),
            true,
        )
    }

    fn snapshot(
        &mut self,
        id: u64,
        classes: &[Class],
        process: &[Value],
        sockets: &[Value],
        files: &[Value],
    ) -> io::Result<()> {
        self.write(
            json!({"type":"snapshot_start", "snapshot_id":id, "checkpoint_sequence":self.sequence}),
            true,
        )?;
        for class in [Class::Process, Class::Sockets, Class::Files] {
            if !classes.contains(&class) {
                continue;
            }
            let items = match class {
                Class::Process => process,
                Class::Sockets => sockets,
                Class::Files => files,
            };
            self.write(
                json!({
                    "type":"snapshot_state",
                    "class":class.id(),
                    "snapshot_id":id,
                    "items":items,
                }),
                true,
            )?;
        }
        self.write(
            json!({"type":"snapshot_end", "snapshot_id":id, "checkpoint_sequence":self.sequence}),
            true,
        )
    }
}

pub fn run(
    options: MonitorOptions,
    writer: &mut impl Write,
    interruption: Option<&AtomicUsize>,
) -> i32 {
    let selected = options.classes.clone();
    let process_enabled = selected.contains(&Class::Process);
    let files_enabled = selected.contains(&Class::Files);
    let sockets_enabled = selected.contains(&Class::Sockets);

    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
    let queue = Arc::new(Queue::new(
        sender,
        Arc::new(Mutex::new(DropCounts::default())),
    ));
    let mut output = Output::new(writer);
    let mut stats = HashMap::new();

    let process = if process_enabled {
        process::prepare_process(&options, &queue)
    } else {
        None
    };
    let (fanotify, files, file_error) = if files_enabled {
        match files::FanotifySource::new(&options) {
            Ok(source) => (Some(source), None, None),
            Err(fanotify_error) => match files::FileSource::new(&options) {
                Ok(source) => (
                    None,
                    Some(source),
                    options
                        .verbose
                        .then_some(("fanotify".to_owned(), fanotify_error.to_string())),
                ),
                Err(error) => (None, None, Some(("inotify".to_owned(), error.to_string()))),
            },
        }
    } else {
        (None, None, None)
    };
    let sockets = if sockets_enabled {
        sockets::read_socket_states().unwrap_or_default()
    } else {
        std::collections::BTreeMap::new()
    };
    let socket_source = "procfs";

    let process_source = process
        .as_ref()
        .map_or("procfs_scan", |source| source.source);
    let process_detection = process
        .as_ref()
        .map_or("sampled", |source| source.detection);
    if process_enabled {
        stats.insert(
            Class::Process,
            Stats::new(process_source, process_detection),
        );
    }

    let file_source = if fanotify.is_some() {
        "fanotify"
    } else if files.is_some() {
        "inotify"
    } else {
        "unavailable"
    };
    let file_detection = if fanotify.is_some() || files.is_some() {
        "exact"
    } else {
        "unavailable"
    };
    if files_enabled {
        stats.insert(Class::Files, Stats::new(file_source, file_detection));
    }
    if sockets_enabled {
        stats.insert(Class::Sockets, Stats::new(socket_source, "sampled"));
    }
    output.stats = stats;

    let event_classes = selected
        .iter()
        .map(|class| {
            let value = match *class {
                Class::Process => json!({"source":process_source,"detection":process_detection}),
                Class::Files => json!({"source":file_source,"detection":file_detection}),
                Class::Sockets => json!({"source":socket_source,"detection":"sampled"}),
            };
            (class.id().to_owned(), value)
        })
        .collect::<Map<_, _>>();
    let _ = output.write(
        json!({
            "type":"stream_start",
            "collector":{
                "name":"vzik",
                "version":env!("CARGO_PKG_VERSION"),
                "build_id":concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION")),
                "target":format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            },
            "event_classes":event_classes,
            "filters":options.filters.iter().map(|path| linux_bytes(path.as_os_str().as_bytes())).collect::<Vec<_>>(),
            "glob":options.path_glob,
            "limits":{"queue":QUEUE_CAPACITY,"max_processes":MAX_PROCESS_STATE,"max_sockets":MAX_SOCKET_STATE,"max_files":MAX_FILE_STATE,"max_watches":MAX_WATCHES,"max_payload_bytes":MAX_PAYLOAD},
            "full":options.full,
            "sensitive":process_enabled || options.full,
            "verbose":options.verbose,
        }),
        false,
    );

    if let Some((source, message)) = file_error {
        let _ = output.write(
            json!({
                "type":"diagnostic",
                "diagnostic":{
                    "class":"files",
                    "source":source,
                    "code":"source_unavailable",
                    "message":message,
                },
            }),
            false,
        );
    }

    for (reason, root) in files
        .as_ref()
        .into_iter()
        .flat_map(|source| source.setup_gaps.iter())
    {
        let _ = output.gap(Class::Files, "inotify", reason, 0);
        let _ = output.write(
            json!({
                "type":"diagnostic",
                "diagnostic":{"class":"files","source":"inotify","code":reason,"root":root},
            }),
            false,
        );
    }

    let process_items = process
        .as_ref()
        .map(|source| {
            source
                .initial
                .values()
                .map(|state| state.value.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let socket_items = sockets
        .values()
        .map(|state| state.value.clone())
        .collect::<Vec<_>>();
    let file_items = if files_enabled {
        files::file_snapshot(&options.filters, options.path_glob.as_deref())
    } else {
        Vec::new()
    };
    let _ = output.snapshot(0, &selected, &process_items, &socket_items, &file_items);

    let context = SourceContext::new(Arc::clone(&queue), Arc::clone(&stop), options.full);
    let mut workers = Vec::new();
    if let Some(process) = process {
        let context = context.clone();
        workers.push(thread::spawn(move || {
            process::process_worker(context, process)
        }));
    }
    if let Some(fanotify) = fanotify {
        let context = context.clone();
        workers.push(thread::spawn(move || {
            files::fanotify_worker(context, fanotify)
        }));
    } else if let Some(files) = files {
        let context = context.clone();
        workers.push(thread::spawn(move || files::file_worker(context, files)));
    }
    if sockets_enabled {
        let context = context.clone();
        workers.push(thread::spawn(move || {
            sockets::socket_worker(context, sockets)
        }));
    }

    let start = Instant::now();
    let snapshot_interval = options.snapshot_interval;
    let mut next_snapshot = snapshot_interval.map(|interval| start + interval);
    let stop_reason = loop {
        if context.stopped() || interruption.is_some_and(|value| value.load(Ordering::Relaxed) != 0)
        {
            break "signal";
        }
        if options
            .duration
            .is_some_and(|duration| start.elapsed() >= duration)
        {
            break "duration";
        }
        if options
            .max_events
            .is_some_and(|limit| output.events >= limit)
        {
            break "max_events";
        }
        if let Some(deadline) = next_snapshot
            && Instant::now() >= deadline
        {
            let id = output.sequence;
            let process_items = if process_enabled {
                process::scan_processes(options.full)
                    .into_values()
                    .map(|state| state.value)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let socket_items = if sockets_enabled {
                sockets::read_socket_states()
                    .unwrap_or_default()
                    .into_values()
                    .map(|state| state.value)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let file_items = if files_enabled {
                files::file_snapshot(&options.filters, options.path_glob.as_deref())
            } else {
                Vec::new()
            };
            if output
                .snapshot(id, &selected, &process_items, &socket_items, &file_items)
                .is_err()
            {
                stop.store(true, Ordering::Relaxed);
                return 141;
            }
            next_snapshot = snapshot_interval.map(|interval| Instant::now() + interval);
        }

        if let Err(error) = drain_drops(&queue, &mut output) {
            stop.store(true, Ordering::Relaxed);
            return if error.kind() == io::ErrorKind::BrokenPipe {
                141
            } else {
                5
            };
        }

        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(record) => {
                if let Err(error) = output.queued(record) {
                    stop.store(true, Ordering::Relaxed);
                    return if error.kind() == io::ErrorKind::BrokenPipe {
                        141
                    } else {
                        5
                    };
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break "source_stopped",
        }
    };

    stop.store(true, Ordering::Relaxed);
    for worker in workers {
        let _ = worker.join();
    }
    if let Err(error) = drain_drops(&queue, &mut output) {
        return if error.kind() == io::ErrorKind::BrokenPipe {
            141
        } else {
            5
        };
    }
    while let Ok(record) = receiver.try_recv() {
        if let Err(error) = output.queued(record) {
            return if error.kind() == io::ErrorKind::BrokenPipe {
                141
            } else {
                5
            };
        }
        if options
            .max_events
            .is_some_and(|limit| output.events >= limit)
        {
            break;
        }
    }

    let collection_outcome = if output.stats.values().any(|stats| {
        stats.detection != "exact" || stats.gaps != 0 || stats.dropped != 0 || stats.evictions != 0
    }) {
        "degraded"
    } else {
        "complete"
    };
    let classes = selected
        .iter()
        .map(|class| {
            let stats = output.stats.get(class).expect("class stats");
            (
                class.id().to_owned(),
                json!({
                    "source":stats.source,
                    "detection":stats.detection,
                    "events":stats.events,
                    "gaps":stats.gaps,
                    "dropped":stats.dropped,
                    "evictions":stats.evictions,
                }),
            )
        })
        .collect::<Map<_, _>>();
    if output
        .write(
            json!({
                "type":"stream_end",
                "stop_reason":stop_reason,
                "collection_outcome":collection_outcome,
                "classes":classes,
                "completion":collection_outcome == "complete",
            }),
            false,
        )
        .is_err()
    {
        return 141;
    }
    if output.writer.flush().is_err() {
        return 5;
    }
    if collection_outcome == "complete" {
        0
    } else {
        3
    }
}

fn drain_drops<W: Write>(queue: &Queue, output: &mut Output<'_, W>) -> io::Result<()> {
    for (class, source, count) in queue.take_dropped() {
        output.gap(class, &source, "output_queue", count)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_parser_keeps_only_process_events() {
        let mut message = vec![0_u8; 16 + 20 + 32];
        let message_length = message.len() as u16;
        message[0..2].copy_from_slice(&message_length.to_ne_bytes());
        let data = 36;
        message[data..data + 4].copy_from_slice(&2_u32.to_ne_bytes());
        message[data + 16..data + 20].copy_from_slice(&42_u32.to_ne_bytes());
        message[data + 20..data + 24].copy_from_slice(&42_u32.to_ne_bytes());
        let events = process::parse_connector_events(&message);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "process_exec");
        assert_eq!(events[0].1, 42);
    }

    #[test]
    fn duration_parser_is_bounded_and_unitful() {
        assert_eq!(
            crate::cli::parse_duration("250ms").unwrap(),
            Duration::from_millis(250)
        );
        assert!(crate::cli::parse_duration("0s").is_err());
        assert!(crate::cli::parse_duration("10").is_err());
    }
}
