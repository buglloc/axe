use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::thread;

use serde_json::{Value, json};

use crate::procfs;
use crate::protocol::linux_bytes;

use super::contract::{Class, SourceContext};
use super::process::namespace_id;
use super::{MAX_SOCKET_STATE, SOCKET_SCAN_INTERVAL};

pub(super) struct SocketState {
    pub(super) value: Value,
}
pub(super) fn read_socket_states() -> io::Result<BTreeMap<String, SocketState>> {
    let mut states = BTreeMap::new();
    for (path, protocol, ipv6) in [
        ("/proc/net/tcp", "tcp", false),
        ("/proc/net/tcp6", "tcp", true),
        ("/proc/net/udp", "udp", false),
        ("/proc/net/udp6", "udp", true),
    ] {
        let bytes = fs::read(path)?;
        for line in bytes
            .split(|byte| *byte == b'\n')
            .skip(1)
            .filter(|line| !line.is_empty())
        {
            let socket = procfs::parse_inet_socket(line, ipv6)?;
            let key = format!("{protocol}:{ipv6}:{}", socket.inode);
            states.insert(
                key.clone(),
                SocketState {
                    value: json!({
                        "network_namespace_id":namespace_id(Path::new("/proc/self/ns/net")),
                        "inode":socket.inode,
                        "protocol":protocol,
                        "family":if ipv6 {"ipv6"} else {"ipv4"},
                        "local_address":socket.local.ip().to_string(),
                        "local_port":socket.local.port(),
                        "remote_address":socket.remote.ip().to_string(),
                        "remote_port":socket.remote.port(),
                        "state":procfs::socket_state(protocol, socket.state),
                        "uid":socket.uid,
                    }),
                },
            );
        }
    }
    let bytes = fs::read("/proc/net/unix")?;
    for line in bytes
        .split(|byte| *byte == b'\n')
        .skip(1)
        .filter(|line| !line.is_empty())
    {
        if let Some(socket) = procfs::parse_unix_socket(line) {
            let key = format!("unix:{}", socket.inode);
            states.insert(
                key.clone(),
                SocketState {
                    value: json!({
                        "network_namespace_id":namespace_id(Path::new("/proc/self/ns/net")),
                        "inode":socket.inode,
                        "protocol":"unix",
                        "family":"unix",
                        "state":format!("{:02x}", socket.state),
                        "socket_type":socket.socket_type,
                        "flags":socket.flags,
                        "path":socket.path.map(linux_bytes),
                        "listening":socket.listening(),
                    }),
                },
            );
        }
    }
    while states.len() > MAX_SOCKET_STATE {
        states.pop_last();
    }
    Ok(states)
}
pub(super) fn socket_worker(context: SourceContext, initial: BTreeMap<String, SocketState>) {
    let mut previous = initial;
    while !context.stopped() {
        thread::sleep(SOCKET_SCAN_INTERVAL);
        if context.stopped() {
            break;
        }
        let current = match read_socket_states() {
            Ok(states) => states,
            Err(error) => {
                context.diagnostic(
                    Some(Class::Sockets),
                    "procfs",
                    "source_stopped",
                    &error.to_string(),
                );
                break;
            }
        };
        for (key, state) in &current {
            if !previous.contains_key(key) {
                context.event(
                    Class::Sockets,
                    "procfs",
                    "socket_observed",
                    state.value.clone(),
                    "sampled",
                );
            }
        }
        for key in previous.keys().filter(|key| !current.contains_key(*key)) {
            context.event(
                Class::Sockets,
                "procfs",
                "socket_disappeared",
                json!({"identity":key}),
                "sampled",
            );
        }
        previous = current;
    }
}
