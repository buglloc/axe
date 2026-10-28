//! Byte-oriented parsers for Linux procfs tables and per-process files.
//!
//! The parsers never read files and never format output: callers own
//! acquisition bounds, deadlines, coverage, and presentation.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

/// One data row of `/proc/net/{tcp,tcp6,udp,udp6}`.
pub struct InetSocket {
    pub local: SocketAddr,
    pub remote: SocketAddr,
    /// Kernel state code; see [`tcp_state`].
    pub state: u8,
    pub uid: u64,
    pub inode: u64,
}

/// Parses one data row of `/proc/net/{tcp,udp}` or, with `ipv6`, `/proc/net/{tcp6,udp6}`.
pub fn parse_inet_socket(line: &[u8], ipv6: bool) -> io::Result<InetSocket> {
    let fields = ascii_fields(line);
    if fields.len() < 10 {
        return Err(malformed("malformed inet socket"));
    }

    Ok(InetSocket {
        local: socket_address(fields[1], ipv6)?,
        remote: socket_address(fields[2], ipv6)?,
        state: hex_u64(fields[3])? as u8,
        uid: parse_u64(fields[7])?,
        inode: parse_u64(fields[9])?,
    })
}

/// Returns the lower-case kernel name of a TCP state code.
pub fn tcp_state(state: u8) -> Option<&'static str> {
    Some(match state {
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
        _ => return None,
    })
}

/// Returns the state name of a `/proc/net/{tcp,udp}` row; UDP reuses two TCP codes.
pub(crate) fn socket_state(protocol: &str, state: u8) -> &'static str {
    if protocol == "udp" {
        return match state {
            0x07 => "unconnected",
            0x01 => "established",
            _ => "unknown",
        };
    }
    tcp_state(state).unwrap_or("unknown")
}

/// One data row of `/proc/net/unix`; unparsable numeric fields read as zero.
pub struct UnixSocket<'a> {
    pub flags: u64,
    pub socket_type: u64,
    pub state: u64,
    pub inode: u64,
    pub path: Option<&'a [u8]>,
}

impl UnixSocket<'_> {
    pub fn listening(&self) -> bool {
        self.flags & 0x0001_0000 != 0 || self.state == 1
    }
}

/// Parses one data row of `/proc/net/unix`; `None` when it has fewer than seven fields.
pub fn parse_unix_socket(line: &[u8]) -> Option<UnixSocket<'_>> {
    let fields = ascii_fields(line);
    if fields.len() < 7 {
        return None;
    }

    Some(UnixSocket {
        flags: hex_u64(fields[3]).unwrap_or(0),
        socket_type: hex_u64(fields[4]).unwrap_or(0),
        state: hex_u64(fields[5]).unwrap_or(0),
        inode: parse_u64(fields[6]).unwrap_or(0),
        path: fields.get(7).copied(),
    })
}

/// Extracts the inode from a `socket:[INODE]` file-descriptor link target.
pub fn socket_inode(link: &[u8]) -> Option<u64> {
    let value = link.strip_prefix(b"socket:[")?.strip_suffix(b"]")?;
    parse_u64(value).ok()
}

/// One row of `/proc/modules`, with fields kept as the kernel printed them.
pub struct Module<'a> {
    pub name: &'a [u8],
    pub size: &'a [u8],
    pub instances: &'a [u8],
    /// Comma-terminated dependency list; empty when the kernel prints `-`.
    pub dependencies: &'a [u8],
    pub state: &'a [u8],
    pub address: &'a [u8],
}

/// Parses one row of `/proc/modules`; `None` when it has fewer than six fields.
pub fn parse_module(line: &[u8]) -> Option<Module<'_>> {
    let fields = ascii_fields(line);
    if fields.len() < 6 {
        return None;
    }

    Some(Module {
        name: fields[0],
        size: fields[1],
        instances: fields[2],
        dependencies: if fields[3] == b"-" { b"" } else { fields[3] },
        state: fields[4],
        address: fields[5],
    })
}

/// Leading fields of `/proc/PID/stat`.
pub struct StatHead<'a> {
    pub pid: u32,
    /// Command name between the first `(` and the last `) `; it may contain both.
    pub comm: &'a [u8],
    /// Space-separated fields after the command name, starting with the state.
    pub fields: &'a [u8],
}

/// Splits `/proc/PID/stat` into PID, command name, and remaining fields.
pub fn parse_stat_head(bytes: &[u8]) -> Option<StatHead<'_>> {
    let open = bytes.iter().position(|byte| *byte == b'(')?;
    let close = bytes.windows(2).rposition(|window| window == b") ")?;
    if open == 0 || close <= open {
        return None;
    }

    Some(StatHead {
        pid: u32::try_from(parse_u64(&bytes[..open]).ok()?).ok()?,
        comm: &bytes[open + 1..close],
        fields: &bytes[close + 2..],
    })
}

/// Returns the real UID from the first `Uid:` line of `/proc/PID/status`.
pub fn status_uid(status: &[u8]) -> Option<u32> {
    let line = status
        .split(|byte| *byte == b'\n')
        .find_map(|line| line.strip_prefix(b"Uid:"))?;
    let real = line
        .split(|byte| byte.is_ascii_whitespace())
        .find(|field| !field.is_empty())?;
    u32::try_from(parse_u64(real).ok()?).ok()
}

/// Decodes the `\ooo` octal escapes of mountinfo and fstab fields.
///
/// A backslash not followed by three octal digits encoding one byte stays literal.
pub fn unescape_mount(field: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\'
            && let Some(digits) = field.get(index + 1..index + 4)
            && digits.iter().all(|digit| (b'0'..=b'7').contains(digit))
            && let Ok(byte) = u8::try_from(
                digits
                    .iter()
                    .fold(0_u16, |value, digit| value * 8 + u16::from(digit - b'0')),
            )
        {
            decoded.push(byte);
            index += 4;
            continue;
        }
        decoded.push(field[index]);
        index += 1;
    }
    decoded
}

pub(crate) fn malformed(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn parse_u64(bytes: &[u8]) -> io::Result<u64> {
    let bytes = bytes.trim_ascii();
    if bytes.is_empty() || bytes.iter().any(|byte| !byte.is_ascii_digit()) {
        return Err(malformed("expected unsigned decimal integer"));
    }

    bytes.iter().try_fold(0_u64, |current, byte| {
        current
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or_else(|| malformed("integer overflow"))
    })
}

pub(crate) fn hex_u64(value: &[u8]) -> io::Result<u64> {
    let text =
        std::str::from_utf8(value).map_err(|_| malformed("hexadecimal field is not ASCII"))?;
    u64::from_str_radix(text, 16).map_err(|_| malformed("invalid hexadecimal field"))
}

/// Splits a procfs row into non-empty ASCII-whitespace-separated fields.
pub fn ascii_fields(line: &[u8]) -> Vec<&[u8]> {
    line.split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .collect()
}

pub(crate) fn ipv4_hex(value: &[u8]) -> io::Result<Ipv4Addr> {
    let raw = u32::try_from(hex_u64(value)?).map_err(|_| malformed("IPv4 address overflow"))?;
    Ok(Ipv4Addr::from(raw.to_le_bytes()))
}

pub(crate) fn ipv6_bytes(value: &[u8]) -> io::Result<[u8; 16]> {
    if value.len() != 32 {
        return Err(malformed("invalid IPv6 address length"));
    }

    let mut bytes = [0_u8; 16];
    let (pairs, remainder) = value.as_chunks::<2>();
    debug_assert!(remainder.is_empty());

    for (index, pair) in pairs.iter().enumerate() {
        bytes[index] = u8::try_from(hex_u64(pair)?).map_err(|_| malformed("IPv6 byte overflow"))?;
    }
    Ok(bytes)
}

fn socket_address(value: &[u8], ipv6: bool) -> io::Result<SocketAddr> {
    let separator = value
        .iter()
        .position(|byte| *byte == b':')
        .ok_or_else(|| malformed("missing socket port"))?;

    let address = &value[..separator];
    let port = u16::try_from(hex_u64(&value[separator + 1..])?)
        .map_err(|_| malformed("socket port overflow"))?;

    let address = if ipv6 {
        let mut bytes = ipv6_bytes(address)?;
        for word in bytes.as_chunks_mut::<4>().0 {
            word.reverse();
        }
        Ipv6Addr::from(bytes).into()
    } else {
        ipv4_hex(address)?.into()
    };

    Ok(SocketAddr::new(address, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_ipv6_socket_in_word_little_endian_order() {
        let line = b"0: 00000000000000000000000001000000:0016 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000 0 0 777";
        let socket = parse_inet_socket(line, true).expect("parse socket");

        assert_eq!(socket.local, "[::1]:22".parse().expect("address"));
        assert_eq!(socket.inode, 777);
    }

    #[test]
    fn module_dash_means_no_dependencies() {
        let module =
            parse_module(b"xt_tcpudp 16384 2 - Live 0x0000000000000000").expect("parse module");

        assert_eq!(module.name, b"xt_tcpudp");
        assert_eq!(module.dependencies, b"");
    }

    #[test]
    fn stat_head_splits_on_last_parenthesis_and_keeps_name_bytes() {
        let head = parse_stat_head(b"42 (a) b\xff (c)) S 7 42").expect("parse stat");

        assert_eq!(head.pid, 42);
        assert_eq!(head.comm, b"a) b\xff (c)");
        assert_eq!(head.fields, b"S 7 42");
        assert!(parse_stat_head(b"(init) S 0").is_none());
        assert!(parse_stat_head(b"42 init) S 0").is_none());
        assert!(parse_stat_head(b"4294967296 (x) S 0").is_none());
    }

    #[test]
    fn status_uid_reads_real_uid_from_first_uid_line() {
        let status = b"Name:\tx\xff\nUid:\t1000\t0\t0\t0\nGid:\t5\t5\t5\t5\n";

        assert_eq!(status_uid(status), Some(1000));
        assert_eq!(status_uid(b"Name:\tx\n"), None);
        assert_eq!(status_uid(b"Uid:\t-1\t0\t0\t0\n"), None);
    }

    #[test]
    fn mount_unescape_decodes_octal_bytes_and_keeps_invalid_escapes() {
        assert_eq!(
            unescape_mount(b"/mnt/a\\040b\\011\\012\\134\xd1\x82"),
            b"/mnt/a b\t\n\\\xd1\x82"
        );
        assert_eq!(unescape_mount(b"\\777\\08x\\12"), b"\\777\\08x\\12");
        assert_eq!(unescape_mount(b"\\377"), b"\xff");
    }

    #[test]
    fn socket_state_names_udp_codes_separately_from_tcp() {
        assert_eq!(socket_state("udp", 0x07), "unconnected");
        assert_eq!(socket_state("tcp", 0x07), "close");
        assert_eq!(socket_state("udp", 0x0a), "unknown");
        assert_eq!(socket_state("tcp", 0x0a), "listen");
    }
}
