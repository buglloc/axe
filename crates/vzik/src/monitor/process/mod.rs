use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::os::fd::RawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use serde_json::{Map, Value, json};

use crate::cli::MonitorOptions;
use crate::procfs;
use crate::protocol::linux_bytes;

use super::contract::{Class, Queue, SourceContext};
use super::{MAX_FULL_FIELD, MAX_PROCESS_STATE, PROCESS_SCAN_INTERVAL};

#[derive(Clone)]
pub(super) struct ProcessState {
    pub(super) key: ProcessKey,
    pub(super) value: Value,
    pub(super) executable: Vec<u8>,
    pub(super) cmdline: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ProcessKey {
    pub(super) pid: u32,
    pub(super) start_time: u64,
}

pub(super) struct ProcessSource {
    pub(super) connector: Option<ConnectorFd>,
    pub(super) initial: BTreeMap<ProcessKey, ProcessState>,
    pub(super) provider: &'static str,
    pub(super) detection: &'static str,
}

pub(super) struct ConnectorFd(RawFd);

impl Drop for ConnectorFd {
    fn drop(&mut self) {
        // SAFETY: the descriptor is created by connector_socket and owned by this value.
        unsafe {
            libc::close(self.0);
        }
    }
}

pub(super) fn prepare_process(
    options: &MonitorOptions,
    queue: &Arc<Queue>,
) -> Option<ProcessSource> {
    let initial = scan_processes(options.full);
    if initial.is_empty() && fs::read_dir("/proc").is_err() {
        return None;
    }
    let connector = connector_socket().ok();
    if connector.is_none() && options.verbose {
        queue.send(
            Some(Class::Process),
            "proc_connector",
            json!({"type":"diagnostic","class":"process","diagnostic":{"provider":"proc_connector","code":"provider_unavailable","message":"proc connector unavailable; using /proc scan"}}),
        );
    }
    let connector_available = connector.is_some();
    Some(ProcessSource {
        connector,
        initial,
        provider: if connector_available {
            "proc_connector"
        } else {
            "procfs_scan"
        },
        detection: if connector_available {
            "exact"
        } else {
            "sampled"
        },
    })
}

pub(super) fn process_worker(context: SourceContext, source: ProcessSource) {
    if let Some(connector) = source.connector {
        connector_worker(context, connector);
    } else {
        scan_process_worker(context, source.initial);
    }
}

fn scan_process_worker(context: SourceContext, mut previous: BTreeMap<ProcessKey, ProcessState>) {
    while !context.stopped() {
        thread::sleep(PROCESS_SCAN_INTERVAL);
        if context.stopped() {
            break;
        }
        let current = scan_processes(context.full);
        for (key, state) in &current {
            if !previous.contains_key(key) {
                context.event(
                    Class::Process,
                    "procfs_scan",
                    "process_created",
                    state.value.clone(),
                    "sampled",
                );
            } else if previous.get(key).is_some_and(|old| {
                old.executable != state.executable || old.cmdline != state.cmdline
            }) {
                context.event(
                    Class::Process,
                    "procfs_scan",
                    "process_exec",
                    state.value.clone(),
                    "sampled",
                );
            }
        }
        for (key, state) in &previous {
            if !current.contains_key(key) {
                context.event(
                    Class::Process,
                    "procfs_scan",
                    "process_exit",
                    json!({"pid":key.pid,"pid_namespace_id":state.value["pid_namespace_id"],"process_start_time":key.start_time}),
                    "sampled",
                );
            }
        }
        previous = current;
    }
}

fn connector_worker(context: SourceContext, connector: ConnectorFd) {
    let mut buffer = [0_u8; 8192];
    while !context.stopped() {
        let mut pollfd = libc::pollfd {
            fd: connector.0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pollfd points to a valid local descriptor and has the declared length.
        let ready = unsafe { libc::poll(&mut pollfd, 1, 250) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            context.provider_stopped(Class::Process, "proc_connector", "poll failed");
            break;
        }
        if ready == 0 {
            continue;
        }
        // SAFETY: buffer is valid for the requested length and connector owns the descriptor.
        let length =
            unsafe { libc::recv(connector.0, buffer.as_mut_ptr().cast(), buffer.len(), 0) };
        if length < 0 {
            let error = io::Error::last_os_error();
            context.provider_stopped(Class::Process, "proc_connector", &error.to_string());
            break;
        }
        for event in parse_connector_events(&buffer[..length as usize]) {
            let (event_name, pid, data) = event;
            context.event(
                Class::Process,
                "proc_connector",
                event_name,
                process_event_data(pid, context.full, data),
                "exact",
            );
        }
    }
}

fn process_event_data(pid: u32, full: bool, event: Option<Value>) -> Value {
    let process = process_state(pid, full).map(|state| state.value);
    match event {
        Some(event) => json!({"process":process,"event":event}),
        None => json!({"process":process}),
    }
}

pub(super) fn parse_connector_events(bytes: &[u8]) -> Vec<(&'static str, u32, Option<Value>)> {
    let mut events = Vec::new();
    let mut offset = 0;
    while offset + 16 <= bytes.len() {
        let length = u16::from_ne_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        if length < 16 || offset + length > bytes.len() {
            break;
        }
        let payload = &bytes[offset + 16..offset + length];
        if payload.len() >= 36 {
            let data = &payload[20..];
            if data.len() >= 32 {
                let what = u32::from_ne_bytes(data[0..4].try_into().unwrap_or_default());
                let first = u32::from_ne_bytes(data[16..20].try_into().unwrap_or_default());
                let second = u32::from_ne_bytes(data[20..24].try_into().unwrap_or_default());
                if first == second && first != 0 {
                    let item = match what {
                        1 => Some(("process_fork", first, Some(json!({"child_pid":first})))),
                        2 => Some(("process_exec", first, None)),
                        0x8000_0000 => Some((
                            "process_exit",
                            first,
                            Some(json!({
                                "exit_code":u32::from_ne_bytes(data[24..28].try_into().unwrap_or_default()),
                                "exit_signal":u32::from_ne_bytes(data[28..32].try_into().unwrap_or_default()),
                            })),
                        )),
                        _ => None,
                    };
                    if let Some(item) = item {
                        let event_name = match item.0 {
                            "process_fork" => "process_created",
                            name => name,
                        };
                        events.push((event_name, item.1, item.2));
                    }
                }
            }
        }
        offset += (length + 3) & !3;
    }
    events
}

fn connector_socket() -> io::Result<ConnectorFd> {
    if !same_namespace("user")
        || !same_namespace("pid")
        || !Path::new("/proc/net/connector").exists()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "proc connector is not visible",
        ));
    }
    // SAFETY: arguments are constants for a Linux connector netlink socket.
    let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_DGRAM, libc::NETLINK_CONNECTOR) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        // SAFETY: sockaddr_nl is fully initialized before bind.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as u16;
        address.nl_pid = std::process::id();
        let bind_result = unsafe {
            libc::bind(
                fd,
                (&address as *const libc::sockaddr_nl).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if bind_result < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut message = [0_u8; 24];
        message[0..4].copy_from_slice(&1_u32.to_ne_bytes());
        message[4..8].copy_from_slice(&1_u32.to_ne_bytes());
        message[16..18].copy_from_slice(&4_u16.to_ne_bytes());
        message[20..24].copy_from_slice(&1_u32.to_ne_bytes());
        // SAFETY: message is a valid connector subscription buffer.
        let sent = unsafe { libc::send(fd, message.as_ptr().cast(), message.len(), 0) };
        if sent != message.len() as isize {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fcntl operates on the descriptor just created above.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ConnectorFd(fd))
    })();
    if result.is_err() {
        // SAFETY: fd was created in this function and has not been transferred.
        unsafe {
            libc::close(fd);
        }
    }
    result
}

fn same_namespace(name: &str) -> bool {
    fs::read_link(format!("/proc/self/ns/{name}")).ok()
        == fs::read_link(format!("/proc/1/ns/{name}")).ok()
}

pub(super) fn scan_processes(full: bool) -> BTreeMap<ProcessKey, ProcessState> {
    let mut states = BTreeMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return states;
    };
    for entry in entries.flatten() {
        if states.len() >= MAX_PROCESS_STATE {
            break;
        }
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Some(state) = process_state(pid, full) {
            states.insert(state.key, state);
        }
    }
    states
}

pub(super) fn process_state(pid: u32, full: bool) -> Option<ProcessState> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let stat = fs::read(root.join("stat")).ok()?;
    let parsed = procfs::parse_stat_head(&stat)?;
    let fields = parsed
        .fields
        .split(|byte| byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    let ppid = fields
        .get(1)
        .and_then(|field| std::str::from_utf8(field).ok()?.parse::<u32>().ok())
        .unwrap_or(0);
    let start_time = fields
        .get(19)
        .and_then(|field| std::str::from_utf8(field).ok()?.parse::<u64>().ok())
        .unwrap_or(0);
    let pid_namespace_id = namespace_id(&root.join("ns/pid"));
    let executable = fs::read_link(root.join("exe"))
        .map(|path| path.as_os_str().as_bytes().to_vec())
        .unwrap_or_default();
    let cmdline = read_bounded(&root.join("cmdline"));
    let mut data = Map::new();
    data.insert("pid".into(), json!(pid));
    data.insert("ppid".into(), json!(ppid));
    data.insert(
        "pid_namespace_id".into(),
        pid_namespace_id.map_or(Value::Null, |id| json!(id)),
    );
    data.insert("process_start_time".into(), json!(start_time));
    data.insert("comm".into(), linux_bytes(parsed.comm));
    data.insert("executable".into(), linux_bytes(&executable));
    data.insert("argv".into(), bounded_nul_values(&cmdline));
    if full {
        let environment = read_bounded(&root.join("environ"));
        data.insert("environment".into(), bounded_nul_values(&environment));
    }
    Some(ProcessState {
        key: ProcessKey { pid, start_time },
        value: Value::Object(data),
        executable,
        cmdline,
    })
}

fn read_bounded(path: &Path) -> Vec<u8> {
    let mut file = fs::File::open(path).ok();
    let mut bytes = Vec::new();
    if let Some(file) = file.as_mut() {
        let _ = file
            .take((MAX_FULL_FIELD as u64).saturating_add(1))
            .read_to_end(&mut bytes);
    }
    bytes.truncate(MAX_FULL_FIELD);
    bytes
}

fn bounded_nul_values(bytes: &[u8]) -> Value {
    let bytes = &bytes[..bytes.len().min(MAX_FULL_FIELD)];
    Value::Array(
        bytes
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
            .map(linux_bytes)
            .collect(),
    )
}

pub(super) fn namespace_id(path: &Path) -> Option<u64> {
    let value = fs::read_link(path).ok()?.into_os_string().into_vec();
    let start = value.iter().position(|byte| *byte == b'[')? + 1;
    let end = value[start..].iter().position(|byte| *byte == b']')? + start;
    std::str::from_utf8(&value[start..end]).ok()?.parse().ok()
}
