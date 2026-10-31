use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::MAX_PAYLOAD;

pub(super) use crate::cli::MonitorClass as Class;

#[derive(Clone)]
pub(super) struct QueuedRecord {
    pub(super) class: Option<Class>,
    pub(super) provider: String,
    pub(super) value: Value,
}

pub(super) fn parse_class(value: &str) -> Option<Class> {
    match value {
        "process" => Some(Class::Process),
        "files" => Some(Class::Files),
        "sockets" => Some(Class::Sockets),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct DropCounts {
    counts: HashMap<(Class, String), u64>,
}

pub(super) struct Queue {
    sender: SyncSender<QueuedRecord>,
    dropped: Arc<Mutex<DropCounts>>,
}

impl Queue {
    pub(super) fn new(sender: SyncSender<QueuedRecord>, dropped: Arc<Mutex<DropCounts>>) -> Self {
        Self { sender, dropped }
    }

    pub(super) fn send(&self, class: Option<Class>, provider: &str, value: Value) -> bool {
        let record = QueuedRecord {
            class,
            provider: provider.to_owned(),
            value,
        };
        match self.sender.try_send(record) {
            Ok(()) => true,
            Err(TrySendError::Full(record)) => {
                if let Some(class) = record.class {
                    let mut dropped = self.dropped.lock().expect("drop counter lock");
                    *dropped.counts.entry((class, record.provider)).or_default() += 1;
                }
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub(super) fn take_dropped(&self) -> Vec<(Class, String, u64)> {
        let mut dropped = self.dropped.lock().expect("drop counter lock");
        let counts = std::mem::take(&mut dropped.counts);
        counts
            .into_iter()
            .map(|((class, provider), count)| (class, provider, count))
            .collect()
    }
}

#[derive(Clone)]
pub(super) struct SourceContext {
    pub(super) queue: Arc<Queue>,
    stop: Arc<AtomicBool>,
    pub(super) full: bool,
}

impl SourceContext {
    pub(super) fn new(queue: Arc<Queue>, stop: Arc<AtomicBool>, full: bool) -> Self {
        Self { queue, stop, full }
    }

    pub(super) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub(super) fn event(
        &self,
        class: Class,
        provider: &str,
        event: &str,
        data: Value,
        detection: &str,
    ) {
        let mut record = json!({
            "type":"event",
            "event":event,
            "class":class.id(),
            "provider":provider,
            "detection":detection,
            "data":data,
        });
        if serde_json::to_vec(&record).map_or(true, |line| line.len() > MAX_PAYLOAD) {
            record["data"] = json!({"truncated":true});
        }
        self.queue.send(Some(class), provider, record);
    }

    /// Reports a provider that stopped delivering events: an explicit gap plus its cause.
    pub(super) fn provider_stopped(&self, class: Class, provider: &str, message: &str) {
        self.queue.send(
            Some(class),
            provider,
            json!({
                "type":"gap",
                "class":class.id(),
                "provider":provider,
                "reason":"provider_stopped",
                "dropped":0,
            }),
        );
        self.queue.send(
            Some(class),
            provider,
            json!({
                "type":"diagnostic",
                "class":class.id(),
                "diagnostic":{
                    "provider":provider,
                    "code":"provider_stopped",
                    "message":message,
                },
            }),
        );
    }
}
