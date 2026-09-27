//! Byte-oriented parsers for Linux procfs tables.
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

pub(crate) fn ascii_fields(line: &[u8]) -> Vec<&[u8]> {
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
}
