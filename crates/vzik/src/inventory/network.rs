use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nix::ifaddrs::getifaddrs;
use nix::sys::socket::SockaddrStorage;
use serde_json::{Value, json};

use super::{
    emit, finish, item_limit, output_limit, parse_u64, path_value, read_bounded, sorted_entries,
    source_failure, text, trim_ascii, unavailable,
};
use crate::cli::{CapabilityId, Invocation, Request, SocketSelection};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, ProtocolError, RecordSink, check_deadline,
};

const SOURCE_LIMIT: u64 = 16 << 20;
const MAX_OWNERSHIP_PROCESSES: usize = 32_768;
const MAX_FDS_PER_PROCESS: usize = 1024;
const MAX_OWNERSHIP_FDS: usize = 262_144;

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match invocation.capability {
        CapabilityId::NetworkInterfaces => interfaces(invocation, sink, deadline),
        CapabilityId::NetworkAddresses => addresses(invocation, sink, deadline),
        CapabilityId::NetworkRoutes => routes(invocation, sink, deadline),
        CapabilityId::NetworkNeighbors => neighbors(invocation, sink, deadline),
        CapabilityId::NetworkSockets | CapabilityId::NetworkListeners => {
            sockets(invocation, sink, deadline)
        }
        CapabilityId::NetworkFirewall => firewall(invocation, sink, deadline),
        _ => unreachable!("network dispatcher received non-network capability"),
    }
}

fn interfaces<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::NetworkInterfaces;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let root = Path::new("/sys/class/net");
    let entries = match sorted_entries(root) {
        Ok(entries) => entries,
        Err(_) => return interfaces_procfs(sink, deadline, max_items, coverage),
    };

    for path in entries {
        check_deadline(deadline)?;
        coverage.scanned += 1;
        if coverage.observed as usize >= max_items {
            return Ok(finish(coverage, true, Some("max_items")));
        }

        let mut data = serde_json::Map::new();
        data.insert(
            "name".into(),
            text(path.file_name().unwrap_or_default().as_bytes()),
        );
        data.insert("source".into(), path_value(&path));

        for (file, field) in [("ifindex", "index"), ("mtu", "mtu"), ("type", "link_type")] {
            match read_bounded(&path.join(file), 4096).and_then(|(bytes, _)| parse_u64(&bytes)) {
                Ok(number) => {
                    data.insert(field.into(), json!(number));
                }
                Err(_) => {
                    partial = true;
                    coverage.skipped += 1;
                }
            }
        }

        for (file, field) in [
            ("address", "hardware_address"),
            ("operstate", "operstate"),
            ("flags", "flags"),
        ] {
            match read_bounded(&path.join(file), 4096) {
                Ok((bytes, _)) => {
                    data.insert(field.into(), text(trim_ascii(&bytes)));
                }
                Err(_) => {
                    partial = true;
                    coverage.skipped += 1;
                }
            }
        }

        if !emit(sink, capability, Value::Object(data), &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    Ok(finish(coverage, partial, None))
}

fn interfaces_procfs<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    max_items: usize,
    mut coverage: Coverage,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::NetworkInterfaces;
    let path = Path::new("/proc/net/dev");
    let (bytes, truncated) = match read_bounded(path, SOURCE_LIMIT) {
        Ok(value) => value,
        Err(error) => return unavailable(sink, capability, path, error, coverage),
    };

    coverage.scanned = bytes.len() as u64;

    for line in bytes.split(|byte| *byte == b'\n').skip(2) {
        check_deadline(deadline)?;
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };

        if coverage.observed as usize >= max_items {
            return Ok(finish(coverage, true, Some("max_items")));
        }

        let fields = line[separator + 1..]
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();

        let data = json!({
            "name":text(trim_ascii(&line[..separator])),
            "receive_bytes":fields.first().and_then(|value| parse_u64(value).ok()),
            "receive_packets":fields.get(1).and_then(|value| parse_u64(value).ok()),
            "transmit_bytes":fields.get(8).and_then(|value| parse_u64(value).ok()),
            "transmit_packets":fields.get(9).and_then(|value| parse_u64(value).ok()),
            "source":path_value(path),
        });

        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    Ok(finish(
        coverage,
        truncated,
        truncated.then_some("max_source_bytes"),
    ))
}

fn routes<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::NetworkRoutes;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut any_source = false;
    for (path, family) in [
        (Path::new("/proc/net/route"), "ipv4"),
        (Path::new("/proc/net/ipv6_route"), "ipv6"),
    ] {
        check_deadline(deadline)?;
        let (bytes, truncated) = match read_bounded(path, SOURCE_LIMIT) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                coverage.vanished += 1;
                continue;
            }
            Err(error) => {
                partial = true;
                source_failure(sink, capability, path, &error, &mut coverage)?;
                continue;
            }
        };

        any_source = true;
        coverage.scanned += bytes.len() as u64;
        partial |= truncated;

        for (line_number, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            if line.is_empty() || (family == "ipv4" && line_number == 0) {
                continue;
            }

            check_deadline(deadline)?;
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }

            let parsed = if family == "ipv4" {
                parse_ipv4_route(line)
            } else {
                parse_ipv6_route(line)
            };

            match parsed {
                Ok(data) => {
                    if !emit(sink, capability, data, &mut coverage)? {
                        return Ok(output_limit(coverage));
                    }
                }
                Err(error) => {
                    partial = true;
                    source_failure(sink, capability, path, &error, &mut coverage)?;
                }
            }
        }
    }

    if !any_source {
        return Ok(CapabilityReport {
            outcome: crate::protocol::Outcome::Unavailable,
            coverage,
            limits_hit: Vec::new(),
        });
    }

    Ok(finish(coverage, partial, None))
}

fn parse_ipv4_route(line: &[u8]) -> io::Result<Value> {
    let fields = ascii_fields(line);
    if fields.len() < 11 {
        return Err(super::malformed("malformed IPv4 route"));
    }

    let destination = ipv4_hex(fields[1])?;
    let gateway = ipv4_hex(fields[2])?;
    let flags = hex_u64(fields[3])?;
    let metric = parse_u64(fields[6])?;
    let mask = ipv4_hex(fields[7])?;
    let prefix = u32::from(mask).count_ones();

    Ok(json!({
        "family":"ipv4",
        "interface":text(fields[0]),
        "destination":format!("{destination}/{prefix}"),
        "gateway":gateway.to_string(),
        "metric":metric,
        "flags":flags,
        "source":path_value(Path::new("/proc/net/route")),
    }))
}

fn parse_ipv6_route(line: &[u8]) -> io::Result<Value> {
    let fields = ascii_fields(line);
    if fields.len() < 10 {
        return Err(super::malformed("malformed IPv6 route"));
    }

    let destination = ipv6_route_hex(fields[0])?;
    let prefix = hex_u64(fields[1])?;
    let gateway = ipv6_route_hex(fields[4])?;
    let metric = hex_u64(fields[5])?;
    let flags = hex_u64(fields[8])?;

    Ok(json!({
        "family":"ipv6",
        "interface":text(fields[9]),
        "destination":format!("{destination}/{prefix}"),
        "gateway":gateway.to_string(),
        "metric":metric,
        "flags":flags,
        "source":path_value(Path::new("/proc/net/ipv6_route")),
    }))
}

#[derive(Debug)]
struct SocketRecord {
    data: serde_json::Map<String, Value>,
    sort_key: Vec<u8>,
    inode: u64,
    listening: bool,
}

fn sockets<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = invocation.capability;
    let Request::SocketList {
        max_items,
        selection,
    } = &invocation.request
    else {
        return Err(ProtocolError::Internal(format!(
            "{} socket list request mismatch",
            capability.id()
        )));
    };

    let max_items = *max_items;
    let selection = *selection;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut records = Vec::with_capacity(max_items.min(4096));
    let mut items_truncated = false;

    for (path, protocol, family) in [
        ("/proc/net/tcp", "tcp", "ipv4"),
        ("/proc/net/tcp6", "tcp", "ipv6"),
        ("/proc/net/udp", "udp", "ipv4"),
        ("/proc/net/udp6", "udp", "ipv6"),
    ] {
        if items_truncated {
            break;
        }

        let path = Path::new(path);
        let (bytes, truncated) = match read_bounded(path, SOURCE_LIMIT) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                coverage.vanished += 1;
                continue;
            }
            Err(error) => {
                partial = true;
                source_failure(sink, capability, path, &error, &mut coverage)?;
                continue;
            }
        };

        coverage.scanned += bytes.len() as u64;
        partial |= truncated;

        for line in bytes.split(|byte| *byte == b'\n').skip(1) {
            check_deadline(deadline)?;
            if line.is_empty() {
                continue;
            }
            match parse_inet_socket(line, protocol, family, path) {
                Ok(record) if socket_selected(selection, protocol, record.listening) => {
                    records.push(record);
                }
                Ok(_) => {}
                Err(error) => {
                    partial = true;
                    source_failure(sink, capability, path, &error, &mut coverage)?;
                }
            }
            if records.len() >= max_items {
                partial = true;
                items_truncated = true;
                break;
            }
        }
    }

    parse_unix_sockets(
        capability,
        selection,
        max_items,
        sink,
        &mut coverage,
        &mut partial,
        &mut records,
        &mut items_truncated,
    )?;

    records.sort_by(|left, right| left.sort_key.cmp(&right.sort_key));
    records.truncate(max_items);
    let wanted = records
        .iter()
        .map(|record| record.inode)
        .collect::<HashSet<_>>();

    let owners = socket_owners(&wanted, deadline, &mut coverage, &mut partial)?;

    for mut record in records {
        if let Some(processes) = owners.get(&record.inode) {
            record.data.insert("processes".into(), json!(processes));
        }
        if !emit(sink, capability, Value::Object(record.data), &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    Ok(finish(
        coverage,
        partial,
        items_truncated.then_some("max_items"),
    ))
}

fn parse_inet_socket(
    line: &[u8],
    protocol: &str,
    family: &str,
    source: &Path,
) -> io::Result<SocketRecord> {
    let fields = ascii_fields(line);
    if fields.len() < 10 {
        return Err(super::malformed("malformed inet socket"));
    }

    let (local_address, local_port) = socket_address(fields[1], family)?;
    let (remote_address, remote_port) = socket_address(fields[2], family)?;
    let state_code = hex_u64(fields[3])? as u8;
    let uid = parse_u64(fields[7])?;
    let inode = parse_u64(fields[9])?;

    let listening = if protocol == "tcp" {
        state_code == 0x0a
    } else {
        local_port != 0 && remote_port == 0
    };

    let state = socket_state(protocol, state_code);

    Ok(SocketRecord {
        sort_key: [protocol.as_bytes(), b"\0", family.as_bytes(), b"\0", line].concat(),
        data: json!({
            "protocol":protocol,
            "family":family,
            "local_address":local_address,
            "local_port":local_port,
            "remote_address":remote_address,
            "remote_port":remote_port,
            "state":state,
            "inode":inode,
            "uid":uid,
            "listening":listening,
            "source":path_value(source),
        })
        .as_object()
        .expect("object literal")
        .clone(),
        inode,
        listening,
    })
}

fn socket_selected(selection: SocketSelection, protocol: &str, listening: bool) -> bool {
    match selection {
        SocketSelection::All => true,
        SocketSelection::Listeners => listening,
        SocketSelection::InetAndUnixListeners => protocol != "unix" || listening,
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_unix_sockets<W: Write>(
    capability: CapabilityId,
    selection: SocketSelection,
    max_items: usize,
    sink: &mut RecordSink<'_, W>,
    coverage: &mut Coverage,
    partial: &mut bool,
    records: &mut Vec<SocketRecord>,
    items_truncated: &mut bool,
) -> Result<(), ProtocolError> {
    let path = Path::new("/proc/net/unix");
    let (bytes, truncated) = match read_bounded(path, SOURCE_LIMIT) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            *partial = true;
            source_failure(sink, capability, path, &error, coverage)?;
            return Ok(());
        }
    };

    coverage.scanned += bytes.len() as u64;
    *partial |= truncated;

    for line in bytes.split(|byte| *byte == b'\n').skip(1) {
        if line.is_empty() {
            continue;
        }

        if records.len() >= max_items {
            *items_truncated = true;
            break;
        }

        let fields = ascii_fields(line);
        if fields.len() < 7 {
            *partial = true;
            coverage.skipped += 1;
            continue;
        }

        let flags = hex_u64(fields[3]).unwrap_or(0);
        let kind = hex_u64(fields[4]).unwrap_or(0);
        let state = hex_u64(fields[5]).unwrap_or(0);
        let inode = parse_u64(fields[6]).unwrap_or(0);
        let listening = flags & 0x0001_0000 != 0 || state == 1;

        if !socket_selected(selection, "unix", listening) {
            continue;
        }

        records.push(SocketRecord {
            sort_key: [b"unix".as_slice(), b"\0", line].concat(),
            data: json!({
                "protocol":"unix",
                "family":"unix",
                "state":format!("{state:02x}"),
                "socket_type":kind,
                "flags":flags,
                "inode":inode,
                "path":fields.get(7).map(|value| text(value)),
                "listening":listening,
                "source":path_value(path),
            })
            .as_object()
            .expect("object literal")
            .clone(),
            inode,
            listening,
        });
    }

    Ok(())
}

fn socket_owners(
    wanted: &HashSet<u64>,
    deadline: ExecutionContext<'_>,
    coverage: &mut Coverage,
    partial: &mut bool,
) -> Result<HashMap<u64, Vec<Value>>, ProtocolError> {
    let mut owners = HashMap::<u64, Vec<Value>>::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        *partial = true;
        return Ok(owners);
    };

    let mut lowest = BinaryHeap::<u32>::with_capacity(MAX_OWNERSHIP_PROCESSES + 1);
    for entry in entries {
        check_deadline(deadline)?;
        let Some(pid) = entry.ok().and_then(|entry| {
            std::str::from_utf8(entry.file_name().as_bytes())
                .ok()?
                .parse::<u32>()
                .ok()
        }) else {
            continue;
        };
        if lowest.len() < MAX_OWNERSHIP_PROCESSES {
            lowest.push(pid);
        } else if lowest.peek().is_some_and(|largest| pid < *largest) {
            lowest.pop();
            lowest.push(pid);
            *partial = true;
        } else {
            *partial = true;
        }
    }

    let mut pids = lowest.into_vec();
    pids.sort_unstable();

    let mut total_fds = 0;
    let mut seen = HashSet::<(u64, u32)>::new();
    for pid in pids {
        check_deadline(deadline)?;
        let process_path = Path::new("/proc").join(pid.to_string());
        let fd_dir = process_path.join("fd");
        let fds = match fs::read_dir(&fd_dir) {
            Ok(fds) => fds,
            Err(error) => {
                *partial = true;
                match error.kind() {
                    io::ErrorKind::NotFound => coverage.vanished += 1,
                    io::ErrorKind::PermissionDenied => coverage.denied += 1,
                    _ => coverage.skipped += 1,
                }
                continue;
            }
        };

        let name = read_bounded(&process_path.join("comm"), 4096)
            .ok()
            .map(|(bytes, _)| text(trim_ascii(&bytes)));
        let exe = fs::read_link(process_path.join("exe"))
            .ok()
            .map(|path| text(path.as_os_str().as_bytes()));

        for (process_fds, fd) in fds.enumerate() {
            if process_fds >= MAX_FDS_PER_PROCESS || total_fds >= MAX_OWNERSHIP_FDS {
                *partial = true;
                break;
            }
            total_fds += 1;
            let Ok(target) = fd.and_then(|entry| fs::read_link(entry.path())) else {
                continue;
            };
            let bytes = target.as_os_str().as_bytes();
            let Some(inode) = socket_inode(bytes) else {
                continue;
            };
            if wanted.contains(&inode) && seen.insert((inode, pid)) {
                owners.entry(inode).or_default().push(json!({
                    "pid":pid,
                    "name":name,
                    "exe":exe,
                }));
            }
        }

        if total_fds >= MAX_OWNERSHIP_FDS {
            break;
        }
    }

    Ok(owners)
}

fn socket_inode(bytes: &[u8]) -> Option<u64> {
    let value = bytes.strip_prefix(b"socket:[")?.strip_suffix(b"]")?;
    parse_u64(value).ok()
}

fn socket_address(value: &[u8], family: &str) -> io::Result<(String, u16)> {
    let separator = value
        .iter()
        .position(|byte| *byte == b':')
        .ok_or_else(|| super::malformed("missing socket port"))?;

    let address = &value[..separator];
    let port = u16::try_from(hex_u64(&value[separator + 1..])?)
        .map_err(|_| super::malformed("socket port overflow"))?;

    let address = if family == "ipv4" {
        ipv4_hex(address)?.to_string()
    } else {
        ipv6_socket_hex(address)?.to_string()
    };

    Ok((address, port))
}

fn ipv4_hex(value: &[u8]) -> io::Result<Ipv4Addr> {
    let raw =
        u32::try_from(hex_u64(value)?).map_err(|_| super::malformed("IPv4 address overflow"))?;
    Ok(Ipv4Addr::from(raw.to_le_bytes()))
}

fn ipv6_route_hex(value: &[u8]) -> io::Result<Ipv6Addr> {
    Ok(Ipv6Addr::from(ipv6_bytes(value)?))
}

fn ipv6_socket_hex(value: &[u8]) -> io::Result<Ipv6Addr> {
    let mut bytes = ipv6_bytes(value)?;
    for word in bytes.as_chunks_mut::<4>().0 {
        word.reverse();
    }
    Ok(Ipv6Addr::from(bytes))
}

fn ipv6_bytes(value: &[u8]) -> io::Result<[u8; 16]> {
    if value.len() != 32 {
        return Err(super::malformed("invalid IPv6 address length"));
    }

    let mut bytes = [0_u8; 16];
    let (pairs, remainder) = value.as_chunks::<2>();
    debug_assert!(remainder.is_empty());

    for (index, pair) in pairs.iter().enumerate() {
        bytes[index] =
            u8::try_from(hex_u64(pair)?).map_err(|_| super::malformed("IPv6 byte overflow"))?;
    }
    Ok(bytes)
}

fn hex_u64(value: &[u8]) -> io::Result<u64> {
    let text = std::str::from_utf8(value)
        .map_err(|_| super::malformed("hexadecimal field is not ASCII"))?;
    u64::from_str_radix(text, 16).map_err(|_| super::malformed("invalid hexadecimal field"))
}

fn ascii_fields(line: &[u8]) -> Vec<&[u8]> {
    line.split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .collect()
}

fn socket_state(protocol: &str, state: u8) -> &'static str {
    if protocol == "udp" {
        return match state {
            0x07 => "unconnected",
            0x01 => "established",
            _ => "unknown",
        };
    }
    match state {
        0x01 => "established",
        0x02 => "syn_sent",
        0x03 => "syn_recv",
        0x04 => "fin_wait1",
        0x05 => "fin_wait2",
        0x06 => "time_wait",
        0x07 => "close",
        0x08 => "close_wait",
        0x09 => "last_ack",
        0x0a => "listen",
        0x0b => "closing",
        _ => "unknown",
    }
}

fn addresses<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::NetworkAddresses;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let entries = match getifaddrs() {
        Ok(entries) => entries,
        Err(error) => {
            return unavailable(
                sink,
                capability,
                Path::new("/proc/net"),
                io::Error::other(format!("enumerate interface addresses: {error}")),
                coverage,
            );
        }
    };

    let mut records = Vec::new();

    for entry in entries {
        check_deadline(deadline)?;
        coverage.scanned += 1;
        let Some((family, address)) = entry.address.as_ref().and_then(sockaddr_ip) else {
            continue;
        };
        let prefix_length = entry
            .netmask
            .as_ref()
            .and_then(|mask| sockaddr_prefix(mask, family));
        let broadcast = entry
            .broadcast
            .as_ref()
            .and_then(sockaddr_ip)
            .map(|(_, address)| address);
        let destination = entry
            .destination
            .as_ref()
            .and_then(sockaddr_ip)
            .map(|(_, address)| address);
        let key = (
            entry.interface_name.clone(),
            family.to_owned(),
            address.clone(),
        );
        records.push((
            key,
            json!({
                "interface":entry.interface_name,
                "family":family,
                "address":address,
                "prefix_length":prefix_length,
                "broadcast":broadcast,
                "destination":destination,
                "flags":entry.flags.bits(),
                "source":{"display":"getifaddrs"},
            }),
        ));
    }

    records.sort_by(|left, right| left.0.cmp(&right.0));
    let truncated = records.len() > max_items;

    for (_, data) in records.into_iter().take(max_items) {
        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    Ok(finish(
        coverage,
        truncated,
        truncated.then_some("max_items"),
    ))
}

fn sockaddr_ip(address: &SockaddrStorage) -> Option<(&'static str, String)> {
    if let Some(address) = address.as_sockaddr_in() {
        return Some(("ipv4", address.ip().to_string()));
    }
    address
        .as_sockaddr_in6()
        .map(|address| ("ipv6", address.ip().to_string()))
}

fn sockaddr_prefix(address: &SockaddrStorage, family: &str) -> Option<u32> {
    match family {
        "ipv4" => address
            .as_sockaddr_in()
            .map(|address| address.ip().octets().into_iter().map(u8::count_ones).sum()),
        "ipv6" => address
            .as_sockaddr_in6()
            .map(|address| address.ip().octets().into_iter().map(u8::count_ones).sum()),
        _ => None,
    }
}

fn neighbors<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::NetworkNeighbors;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let path = Path::new("/proc/net/arp");
    let (bytes, truncated) = match read_bounded(path, SOURCE_LIMIT) {
        Ok(value) => value,
        Err(error) => return unavailable(sink, capability, path, error, coverage),
    };

    coverage.scanned = bytes.len() as u64;

    for line in bytes.split(|byte| *byte == b'\n').skip(1) {
        check_deadline(deadline)?;
        let fields = ascii_fields(line);
        if fields.len() < 6 {
            continue;
        }

        if coverage.observed as usize >= max_items {
            return Ok(finish(coverage, true, Some("max_items")));
        }

        let data = json!({
            "family":"ipv4",
            "address":text(fields[0]),
            "hardware_type":text(fields[1]),
            "flags":text(fields[2]),
            "hardware_address":text(fields[3]),
            "mask":text(fields[4]),
            "interface":text(fields[5]),
            "source":path_value(path),
        });

        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }
    Ok(finish(
        coverage,
        truncated,
        truncated.then_some("max_source_bytes"),
    ))
}

fn firewall<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::NetworkFirewall;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let mut partial = false;
    for path in [
        "/proc/net/ip_tables_names",
        "/proc/net/ip6_tables_names",
        "/proc/net/arp_tables_names",
    ] {
        check_deadline(deadline)?;
        let path = Path::new(path);
        let (bytes, truncated) = match read_bounded(path, SOURCE_LIMIT) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                coverage.vanished += 1;
                continue;
            }
            Err(error) => {
                partial = true;
                source_failure(sink, capability, path, &error, &mut coverage)?;
                continue;
            }
        };
        coverage.scanned += bytes.len() as u64;
        partial |= truncated;
        for line in bytes.split(|byte| *byte == b'\n') {
            let line = trim_ascii(line);
            if line.is_empty() {
                continue;
            }
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            if !emit(
                sink,
                capability,
                json!({
                    "scope":"runtime_table_name",
                    "source":path_value(path),
                    "value":text(line),
                }),
                &mut coverage,
            )? {
                return Ok(output_limit(coverage));
            }
        }
    }

    for path in [
        "/etc/nftables.conf",
        "/etc/iptables/rules.v4",
        "/etc/iptables/rules.v6",
        "/etc/sysconfig/iptables",
        "/etc/sysconfig/ip6tables",
        "/etc/ufw/user.rules",
        "/etc/ufw/user6.rules",
    ] {
        check_deadline(deadline)?;
        let path = Path::new(path);
        let (bytes, truncated) = match read_bounded(path, SOURCE_LIMIT) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                coverage.vanished += 1;
                continue;
            }
            Err(error) => {
                partial = true;
                source_failure(sink, capability, path, &error, &mut coverage)?;
                continue;
            }
        };
        coverage.scanned += bytes.len() as u64;
        partial |= truncated;
        for (line_number, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            let line = trim_ascii(line);
            if line.is_empty() || line.starts_with(b"#") {
                continue;
            }
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            if !emit(
                sink,
                capability,
                json!({
                    "scope":"static_rule",
                    "source":path_value(path),
                    "line":line_number + 1,
                    "value":text(line),
                }),
                &mut coverage,
            )? {
                return Ok(output_limit(coverage));
            }
        }
    }

    Ok(finish(coverage, partial, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_keeps_inet_sockets_and_only_listening_unix_sockets() {
        assert!(socket_selected(
            SocketSelection::InetAndUnixListeners,
            "tcp",
            false
        ));
        assert!(socket_selected(
            SocketSelection::InetAndUnixListeners,
            "udp",
            false
        ));
        assert!(socket_selected(
            SocketSelection::InetAndUnixListeners,
            "unix",
            true
        ));
        assert!(!socket_selected(
            SocketSelection::InetAndUnixListeners,
            "unix",
            false
        ));
        assert!(socket_selected(SocketSelection::All, "unix", false));
        assert!(!socket_selected(SocketSelection::Listeners, "tcp", false));
    }

    #[test]
    fn parses_ipv4_listening_socket() {
        let line = b"0: 0100007F:1B39 00000000:0000 0A 0:0 00:00000000 00000000 1000 0 12345";
        let record = parse_inet_socket(line, "tcp", "ipv4", Path::new("/proc/net/tcp"))
            .expect("parse socket");
        assert!(record.listening);
        assert_eq!(record.data["local_address"], "127.0.0.1");
        assert_eq!(record.data["local_port"], 6969);
        assert_eq!(record.inode, 12345);
    }

    #[test]
    fn decodes_ipv4_route_endianness() {
        assert_eq!(
            ipv4_hex(b"0100007F").expect("decode"),
            Ipv4Addr::new(127, 0, 0, 1)
        );
    }

    #[test]
    fn decodes_ipv6_route_in_network_byte_order() {
        let line = b"2a00137081a05dab0000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000000 00000000 00000000 00000001 eth0";
        let route = parse_ipv6_route(line).expect("parse IPv6 route");

        assert_eq!(route["destination"], "2a00:1370:81a0:5dab::/64");
        assert_eq!(route["gateway"], "::");
    }

    #[test]
    fn decodes_ipv6_socket_in_word_little_endian_order() {
        assert_eq!(
            ipv6_socket_hex(b"00000000000000000000000001000000").expect("decode"),
            Ipv6Addr::LOCALHOST
        );
    }
}
