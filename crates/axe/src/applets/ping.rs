use std::ffi::{CString, OsString};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use tokio::io::unix::AsyncFd;
use tokio::io::{Interest, Ready};

pub fn ping(args: Vec<OsString>) -> i32 {
    run_ping(args, false)
}

pub fn ping6(args: Vec<OsString>) -> i32 {
    run_ping(args, true)
}

pub fn traceroute(args: Vec<OsString>) -> i32 {
    run_traceroute(args, false)
}

pub fn traceroute6(args: Vec<OsString>) -> i32 {
    run_traceroute(args, true)
}

#[derive(Clone, Copy)]
enum Family {
    Any,
    V4,
    V6,
}

impl Family {
    fn accepts(self, address: IpAddr) -> bool {
        match self {
            Self::Any => true,
            Self::V4 => address.is_ipv4(),
            Self::V6 => address.is_ipv6(),
        }
    }
}

#[derive(Debug)]
struct ArgumentError {
    message: String,
    status: i32,
}

impl From<String> for ArgumentError {
    fn from(message: String) -> Self {
        Self { message, status: 2 }
    }
}

impl From<&str> for ArgumentError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

fn probe_io(error: io::Error) -> ArgumentError {
    ArgumentError {
        message: error.to_string(),
        status: 1,
    }
}

struct PingOptions {
    host: String,
    family: Family,
    count: Option<u64>,
    interval: Duration,
    timeout: Duration,
    size: usize,
    quiet: bool,
    numeric: bool,
    interface: Option<String>,
}

fn run_ping(args: Vec<OsString>, force_v6: bool) -> i32 {
    let options = match parse_ping(args, force_v6) {
        Ok(Some(options)) => options,
        Ok(None) => return 0,
        Err(error) => {
            eprintln!("ping: {}", error.message);
            return error.status;
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ping: {error}");
            return 2;
        }
    };
    match runtime.block_on(ping_async(options)) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            eprintln!("ping: {error}");
            2
        }
    }
}

fn parse_ping(args: Vec<OsString>, force_v6: bool) -> Result<Option<PingOptions>, ArgumentError> {
    let mut family = if force_v6 { Family::V6 } else { Family::Any };
    let values = utf8_args(args)?;
    let mut count = None;
    let mut interval = Duration::from_secs(1);
    let mut timeout = Duration::from_secs(2);
    let mut size = 56;
    let mut quiet = false;
    let mut numeric = false;
    let mut interface = None;
    let mut host = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        let (option, inline) = split_option(
            &value,
            &[
                ("-c", "--count"),
                ("-i", "--interval"),
                ("-W", "--timeout"),
                ("-s", "--size"),
                ("-I", "--interface"),
            ],
        );
        match option {
            "-4" if force_v6 => return Err("-4 conflicts with ping6".into()),
            "-4" => family = Family::V4,
            "-6" => family = Family::V6,
            "-c" | "--count" => {
                let value = option_argument(inline, &mut iter);
                let parsed = parse_value::<u64>(value.clone(), "count")
                    .ok()
                    .filter(|count| *count > 0)
                    .ok_or_else(|| ArgumentError {
                        message: value.map_or_else(
                            || "count requires an argument".into(),
                            |value| format!("invalid argument: '{value}'"),
                        ),
                        status: 1,
                    })?;
                count = Some(parsed);
            }
            "-i" | "--interval" => {
                interval = parse_duration(option_argument(inline, &mut iter), "timing interval")?;
            }
            "-W" | "--timeout" => {
                timeout = parse_duration(option_argument(inline, &mut iter), "linger time")?;
            }
            "-s" | "--size" => size = parse_value(option_argument(inline, &mut iter), "size")?,
            "-I" | "--interface" => {
                interface = Some(
                    option_argument(inline, &mut iter).ok_or("interface requires an argument")?,
                );
            }
            "-q" | "--quiet" => quiet = true,
            "-n" | "--numeric" => numeric = true,
            "-h" | "--help" => {
                println!(
                    "Usage: ping [-46nq] [-c COUNT] [-i INTERVAL] [-W TIMEOUT] [-s SIZE] HOST"
                );
                return Ok(None);
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown option '{value}'").into());
            }
            _ if host.is_none() => host = Some(value),
            _ => return Err("too many hosts".into()),
        }
    }
    let host = host.ok_or("missing host operand")?;
    if size > 65_527 {
        return Err("packet size exceeds the maximum IP payload".into());
    }
    Ok(Some(PingOptions {
        host,
        family,
        count,
        interval,
        timeout,
        size,
        quiet,
        numeric,
        interface,
    }))
}

async fn ping_async(options: PingOptions) -> Result<bool, String> {
    let addresses = resolve(&options.host, options.family).await?;
    let mut socket = ProbeSocket::for_addresses(
        &addresses,
        options.family,
        true,
        options.interface.as_deref(),
        0,
    )
    .map_err(|error| error.to_string())?;
    let destination = socket.destination;
    if destination.is_ipv4() && options.size > 65_507 {
        return Err("packet size exceeds the maximum IPv4 payload".into());
    }
    let mut packet = vec![0x61; options.size + 8];
    let timing = options.size >= std::mem::size_of::<libc::timeval>();

    let source = if options.interface.is_some() {
        Some(
            socket_local_address(socket.fd.get_ref())
                .map_err(|error| error.to_string())?
                .ip(),
        )
    } else {
        None
    };
    print!("PING {} ({destination}) ", options.host);
    if let (Some(source), Some(interface)) = (source, options.interface.as_deref()) {
        print!("from {source} {interface}: ");
    }
    if destination.is_ipv4() {
        println!("{}({}) bytes of data.", options.size, options.size + 28);
    } else {
        println!("{} data bytes", options.size);
    }

    let started = Instant::now();
    let mut last_transmitted = started;
    let mut transmitted = 0_u64;
    let mut rtt_stats = RttStats::default();
    loop {
        if options.count.is_some_and(|count| transmitted >= count) {
            break;
        }
        transmitted += 1;
        if transmitted > 1 {
            last_transmitted = Instant::now();
        }
        let reply = tokio::select! {
            reply = socket.probe(
                transmitted as u16,
                0,
                &mut packet,
                (!options.timeout.is_zero()).then_some(options.timeout),
            ) => reply.map_err(|error| error.to_string())?,
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|error| error.to_string())?;
                break;
            }
        };
        if let Some(reply) = reply {
            match reply.kind {
                ReplyKind::Echo => {
                    rtt_stats.record(reply.elapsed);
                    if !options.quiet {
                        let display_host =
                            if options.numeric || options.host.parse::<IpAddr>().is_ok() {
                                destination.to_string()
                            } else {
                                format!("{} ({destination})", options.host)
                            };
                        let ttl = reply
                            .ttl
                            .ok_or("echo reply has no received TTL or hop limit")?;
                        print!(
                            "{} bytes from {display_host}: icmp_seq={transmitted} ttl={ttl}",
                            reply.bytes
                        );
                        if timing {
                            print!(" time={:.3} ms", reply.elapsed.as_secs_f64() * 1000.0);
                        }
                        println!();
                    }
                }
                kind if !options.quiet => println!(
                    "From {} icmp_seq={transmitted} {}",
                    reply.source,
                    kind.description(destination.is_ipv6())
                ),
                _ => {}
            }
        }

        if options.count.is_some_and(|count| transmitted >= count) {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(options.interval) => {}
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|error| error.to_string())?;
                break;
            }
        }
    }

    let loss = if transmitted == 0 {
        0.0
    } else {
        (transmitted - rtt_stats.received) as f64 * 100.0 / transmitted as f64
    };
    println!("\n--- {} ping statistics ---", options.host);
    let received = rtt_stats.received;
    println!(
        "{transmitted} packets transmitted, {received} received, {loss:.0}% packet loss, time {}ms",
        last_transmitted.duration_since(started).as_millis()
    );
    if received > 0 && timing {
        let minimum_ms = rtt_stats.minimum_us as f64 / 1000.0;
        let maximum_ms = rtt_stats.maximum_us as f64 / 1000.0;
        let (average_ms, mean_deviation) = rtt_stats.average_and_deviation();
        println!(
            "rtt min/avg/max/mdev = {minimum_ms:.3}/{average_ms:.3}/{maximum_ms:.3}/{mean_deviation:.3} ms",
        );
    } else {
        println!();
    }
    Ok(received > 0)
}

#[derive(Default)]
struct RttStats {
    received: u64,
    total_us: u128,
    total_squared_us: f64,
    minimum_us: u128,
    maximum_us: u128,
}

impl RttStats {
    fn record(&mut self, rtt: Duration) {
        let microseconds = rtt.as_micros();
        self.minimum_us = if self.received == 0 {
            microseconds
        } else {
            self.minimum_us.min(microseconds)
        };
        self.maximum_us = self.maximum_us.max(microseconds);
        self.received += 1;
        self.total_us += microseconds;
        self.total_squared_us += (microseconds as f64).powi(2);
    }

    fn average_and_deviation(&self) -> (f64, f64) {
        let count = self.received as f64;
        let average_us = self.total_us as f64 / count;
        let variance = self.total_squared_us / count - average_us * average_us;
        (average_us / 1000.0, variance.max(0.0).sqrt() / 1000.0)
    }
}

#[derive(Clone, Copy)]
enum TraceMethod {
    Udp,
    Icmp,
    UdpFixed,
}

struct TraceOptions {
    host: String,
    family: Family,
    method: TraceMethod,
    port: Option<u16>,
    max_hops: u8,
    first_hop: u8,
    probes: u8,
    timeout: Duration,
    numeric: bool,
}

fn run_traceroute(args: Vec<OsString>, force_v6: bool) -> i32 {
    let options = match parse_trace(args, force_v6) {
        Ok(Some(options)) => options,
        Ok(None) => return 0,
        Err(error) => {
            eprintln!("traceroute: {error}");
            return 2;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("traceroute: {error}");
            return 1;
        }
    };
    match runtime.block_on(trace_async(options)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("traceroute: {}", error.message);
            error.status
        }
    }
}

fn parse_trace(args: Vec<OsString>, force_v6: bool) -> Result<Option<TraceOptions>, String> {
    let mut family = if force_v6 { Family::V6 } else { Family::Any };
    let values = utf8_args(args)?;
    let mut method = TraceMethod::Udp;
    let mut port = None;
    let mut max_hops = 30;
    let mut first_hop = 1;
    let mut probes = 3;
    let mut timeout = Duration::from_secs(5);
    let mut numeric = false;
    let mut host = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        let (option, inline) = split_option(
            &value,
            &[
                ("-m", "--max-hops"),
                ("-f", "--first"),
                ("-q", "--queries"),
                ("-w", "--wait"),
                ("-p", "--port"),
            ],
        );
        match option {
            "-4" if force_v6 => return Err("-4 conflicts with traceroute6".into()),
            "-4" => family = Family::V4,
            "-6" => family = Family::V6,
            "-I" => method = TraceMethod::Icmp,
            "-U" => method = TraceMethod::UdpFixed,
            "-m" | "--max-hops" => {
                max_hops = parse_value(option_argument(inline, &mut iter), "max hops")?;
            }
            "-f" | "--first" => {
                first_hop = parse_value(option_argument(inline, &mut iter), "first hop")?;
            }
            "-q" | "--queries" => {
                probes = parse_value(option_argument(inline, &mut iter), "queries")?;
            }
            "-w" | "--wait" => {
                timeout =
                    parse_duration(option_argument(inline, &mut iter), "wait specifications")?;
            }
            "-p" | "--port" => {
                port = Some(parse_value(option_argument(inline, &mut iter), "port")?);
            }
            "-n" => numeric = true,
            "-h" | "--help" => {
                println!(
                    "Usage: traceroute [-46IUn] [-m MAX_HOPS] [-f FIRST_HOP] [-q PROBES] [-w WAIT] [-p PORT] HOST"
                );
                return Ok(None);
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ if host.is_none() => host = Some(value),
            _ => return Err("too many hosts".into()),
        }
    }
    let host = host.ok_or("missing host operand")?;
    if first_hop == 0 || max_hops < first_hop || probes == 0 || port == Some(0) {
        return Err("invalid hop, probe count or port".into());
    }
    Ok(Some(TraceOptions {
        host,
        family,
        method,
        port,
        max_hops,
        first_hop,
        probes,
        timeout,
        numeric,
    }))
}

async fn trace_async(options: TraceOptions) -> Result<(), ArgumentError> {
    let addresses = resolve(&options.host, options.family).await?;
    let icmp = matches!(options.method, TraceMethod::Icmp);
    let port = match options.method {
        TraceMethod::Icmp => 0,
        TraceMethod::Udp => options.port.unwrap_or(33_434),
        TraceMethod::UdpFixed => options.port.unwrap_or(53),
    };
    let mut socket = ProbeSocket::for_addresses(&addresses, options.family, icmp, None, port)
        .map_err(probe_io)?;
    let destination = socket.destination;
    let mut packet = vec![0; if icmp { 40 } else { 32 }];
    let packet_length =
        packet.len() + if destination.is_ipv4() { 20 } else { 40 } + if icmp { 0 } else { 8 };
    println!(
        "traceroute to {} ({destination}), {} hops max, {packet_length} byte packets",
        options.host, options.max_hops
    );
    let mut sequence = 0_u16;
    for ttl in options.first_hop..=options.max_hops {
        socket.set_hops(ttl).map_err(probe_io)?;
        print!("{ttl:2} ");
        let mut reached = false;
        let mut last_source = None;
        for _ in 0..options.probes {
            if matches!(options.method, TraceMethod::UdpFixed) {
                socket.renew_udp_source().map_err(probe_io)?;
                socket.set_hops(ttl).map_err(probe_io)?;
            }
            let port = match options.method {
                TraceMethod::Icmp => 0,
                TraceMethod::Udp => options.port.unwrap_or(33_434).wrapping_add(sequence),
                TraceMethod::UdpFixed => options.port.unwrap_or(53),
            };
            sequence = sequence.wrapping_add(1);
            match socket
                .probe(sequence, port, &mut packet, Some(options.timeout))
                .await
                .map_err(probe_io)?
            {
                Some(reply) => {
                    if last_source != Some(reply.source) {
                        if !options.numeric
                            && reply.source == destination
                            && options.host.parse::<IpAddr>().is_err()
                        {
                            print!(" {} ({})  ", options.host, reply.source);
                        } else {
                            print!(" {}  ", reply.source);
                        }
                        last_source = Some(reply.source);
                    } else {
                        print!("  ");
                    }
                    print!("{:.3} ms", reply.elapsed.as_secs_f64() * 1000.0);
                    let at_destination = reply.source == destination
                        && (matches!(reply.kind, ReplyKind::Echo | ReplyKind::Udp)
                            || (!icmp && reply.kind.port_unreachable(destination.is_ipv6())));
                    reached |= at_destination;
                    if !at_destination {
                        print!("{}", reply.kind.trace_annotation(destination.is_ipv6()));
                    }
                }
                None => print!(" *"),
            }
        }
        println!();
        if reached {
            break;
        }
    }
    Ok(())
}

async fn resolve(host: &str, family: Family) -> Result<Vec<IpAddr>, String> {
    if let Ok(address) = host.parse::<IpAddr>() {
        return family
            .accepts(address)
            .then(|| vec![address])
            .ok_or_else(|| format!("{host}: Address family for hostname not supported"));
    }
    let mut builder =
        hickory_resolver::TokioResolver::builder_tokio().map_err(|error| error.to_string())?;
    builder.options_mut().ip_strategy = match family {
        Family::Any => hickory_resolver::config::LookupIpStrategy::Ipv6AndIpv4,
        Family::V4 => hickory_resolver::config::LookupIpStrategy::Ipv4Only,
        Family::V6 => hickory_resolver::config::LookupIpStrategy::Ipv6Only,
    };
    let resolver = builder.build().map_err(|error| error.to_string())?;
    let addresses: Vec<_> = resolver
        .lookup_ip(host)
        .await
        .map_err(|error| error.to_string())?
        .iter()
        .filter(|address| family.accepts(*address))
        .collect();
    if addresses.is_empty() {
        Err(format!("{host}: Address family for hostname not supported"))
    } else {
        Ok(addresses)
    }
}

#[derive(Clone, Copy)]
enum ReplyKind {
    Echo,
    Udp,
    TimeExceeded,
    Unreachable { code: u8, mtu: u32 },
}

impl ReplyKind {
    fn port_unreachable(self, ipv6: bool) -> bool {
        matches!(self, Self::Unreachable { code, .. } if code == if ipv6 { 4 } else { 3 })
    }

    fn description(self, ipv6: bool) -> &'static str {
        match self {
            Self::TimeExceeded => "Time to live exceeded",
            Self::Unreachable { code: 0, .. } if !ipv6 => "Destination Net Unreachable",
            Self::Unreachable { code: 1, .. } if !ipv6 => "Destination Host Unreachable",
            kind if kind.port_unreachable(ipv6) => "Destination Port Unreachable",
            Self::Unreachable { .. } => "Destination Unreachable",
            Self::Echo | Self::Udp => "Reply",
        }
    }

    fn trace_annotation(self, ipv6: bool) -> String {
        match self {
            Self::Unreachable { code: 0, .. } => " !N".into(),
            Self::Unreachable { code: 1, .. } if !ipv6 => " !H".into(),
            Self::Unreachable { code: 2, .. } if !ipv6 => " !P".into(),
            Self::Unreachable { code: 4, mtu } if !ipv6 => format!(" !F-{mtu}"),
            Self::Unreachable {
                code: 9 | 10 | 13, ..
            } if !ipv6 => " !X".into(),
            Self::Unreachable { code: 1, .. } if ipv6 => " !X".into(),
            Self::Unreachable { code: 3, .. } if ipv6 => " !H".into(),
            Self::Unreachable { code, .. } => format!(" !<{code}>"),
            _ => String::new(),
        }
    }
}

struct ProbeReply {
    source: IpAddr,
    kind: ReplyKind,
    ttl: Option<u8>,
    bytes: usize,
    elapsed: Duration,
}

struct ProbeSocket {
    fd: AsyncFd<OwnedFd>,
    destination: IpAddr,
    icmp: bool,
    raw: bool,
    identifier: u16,
    buffer: Vec<u8>,
}

impl ProbeSocket {
    fn for_addresses(
        addresses: &[IpAddr],
        family: Family,
        icmp: bool,
        interface: Option<&str>,
        port: u16,
    ) -> io::Result<Self> {
        let mut last_error = None;
        for &destination in addresses {
            match Self::new(destination, icmp, interface, port) {
                Ok(socket) => return Ok(socket),
                Err(error) if matches!(family, Family::Any) => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::AddrNotAvailable, "no matching addresses")
        }))
    }

    fn new(
        destination: IpAddr,
        icmp: bool,
        interface: Option<&str>,
        port: u16,
    ) -> io::Result<Self> {
        let family = if destination.is_ipv4() {
            libc::AF_INET
        } else {
            libc::AF_INET6
        };
        let protocol = if !icmp {
            libc::IPPROTO_UDP
        } else if destination.is_ipv4() {
            libc::IPPROTO_ICMP
        } else {
            libc::IPPROTO_ICMPV6
        };
        // SAFETY: socket has no borrowed pointers and returns a new descriptor on success.
        let mut descriptor = unsafe {
            libc::socket(
                family,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                protocol,
            )
        };
        let mut raw = false;
        if descriptor < 0 && icmp {
            // SAFETY: the failed socket call did not acquire a descriptor.
            descriptor = unsafe {
                libc::socket(
                    family,
                    libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    protocol,
                )
            };
            raw = true;
        }
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: descriptor is newly acquired and ownership is transferred exactly once.
        let fd = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let (level, receive_error, receive_ttl) = if destination.is_ipv4() {
            (libc::IPPROTO_IP, libc::IP_RECVERR, libc::IP_RECVTTL)
        } else {
            (
                libc::IPPROTO_IPV6,
                libc::IPV6_RECVERR,
                libc::IPV6_RECVHOPLIMIT,
            )
        };
        set_socket_int(&fd, level, receive_error, 1)?;
        set_socket_int(&fd, level, receive_ttl, 1)?;
        if let Some(interface) = interface {
            let interface = CString::new(interface).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid interface name")
            })?;
            // SAFETY: the NUL-terminated interface bytes remain alive for the complete call.
            let result = unsafe {
                libc::setsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_BINDTODEVICE,
                    interface.as_ptr().cast(),
                    interface.as_bytes_with_nul().len() as libc::socklen_t,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let unspecified = if destination.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        };
        socket_address_call(&fd, SocketAddr::new(unspecified, 0), false)?;
        let identifier = if raw {
            std::process::id() as u16
        } else {
            socket_local_address(&fd)?.port()
        };
        socket_address_call(&fd, SocketAddr::new(destination, port), true)?;
        Ok(Self {
            fd: AsyncFd::new(fd)?,
            destination,
            icmp,
            raw,
            identifier,
            buffer: vec![0; 65_536],
        })
    }

    fn set_hops(&self, hops: u8) -> io::Result<()> {
        let (level, option) = if self.destination.is_ipv4() {
            (libc::IPPROTO_IP, libc::IP_TTL)
        } else {
            (libc::IPPROTO_IPV6, libc::IPV6_UNICAST_HOPS)
        };
        set_socket_int(self.fd.get_ref(), level, option, i32::from(hops))
    }

    fn renew_udp_source(&mut self) -> io::Result<()> {
        let local = if self.destination.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        };
        // Keep the old socket bound until the new ephemeral port is allocated, so a minimal
        // ICMP quotation for fixed-port UDP cannot be mistaken for the preceding probe.
        let socket = std::net::UdpSocket::bind(SocketAddr::new(local, 0))?;
        socket.set_nonblocking(true)?;
        let fd: OwnedFd = socket.into();
        let (level, error, ttl) = if self.destination.is_ipv4() {
            (libc::IPPROTO_IP, libc::IP_RECVERR, libc::IP_RECVTTL)
        } else {
            (
                libc::IPPROTO_IPV6,
                libc::IPV6_RECVERR,
                libc::IPV6_RECVHOPLIMIT,
            )
        };
        set_socket_int(&fd, level, error, 1)?;
        set_socket_int(&fd, level, ttl, 1)?;
        self.fd = AsyncFd::new(fd)?;
        Ok(())
    }

    async fn probe(
        &mut self,
        sequence: u16,
        port: u16,
        packet: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<ProbeReply>> {
        // Consume delayed errors before changing the UDP destination or sending another sequence.
        loop {
            match receive_datagram(self.fd.get_ref(), &mut self.buffer, true) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        socket_address_call(
            self.fd.get_ref(),
            SocketAddr::new(self.destination, port),
            true,
        )?;
        if self.icmp {
            packet[0] = if self.destination.is_ipv4() { 8 } else { 128 };
            packet[1..4].fill(0);
            packet[4..6].copy_from_slice(&self.identifier.to_be_bytes());
            packet[6..8].copy_from_slice(&sequence.to_be_bytes());
            if self.destination.is_ipv4() {
                let checksum = internet_checksum(packet);
                packet[2..4].copy_from_slice(&checksum.to_be_bytes());
            }
        } else {
            packet[..2].copy_from_slice(&sequence.to_be_bytes());
        }
        let started = Instant::now();
        self.fd
            .async_io(Interest::WRITABLE, |fd| {
                // SAFETY: packet is readable for packet.len() bytes during this nonblocking call.
                let sent =
                    unsafe { libc::send(fd.as_raw_fd(), packet.as_ptr().cast(), packet.len(), 0) };
                if sent < 0 {
                    Err(io::Error::last_os_error())
                } else if sent as usize != packet.len() {
                    Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "incomplete probe datagram",
                    ))
                } else {
                    Ok(())
                }
            })
            .await?;
        let receive = self.receive(sequence, port, packet, started);
        match timeout {
            Some(timeout) => match tokio::time::timeout(timeout, receive).await {
                Ok(reply) => reply.map(Some),
                Err(_) => Ok(None),
            },
            None => receive.await.map(Some),
        }
    }

    async fn next_datagram(&mut self) -> io::Result<(Datagram, bool)> {
        loop {
            let mut guard = self.fd.ready(Interest::READABLE | Interest::ERROR).await?;
            match receive_datagram(self.fd.get_ref(), &mut self.buffer, true) {
                Ok(datagram) => return Ok((datagram, true)),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if guard.ready().is_error() {
                        guard.clear_ready_matching(Ready::ERROR);
                    }
                }
                Err(error) => return Err(error),
            }
            match receive_datagram(self.fd.get_ref(), &mut self.buffer, false) {
                Ok(datagram) => return Ok((datagram, false)),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if guard.ready().is_readable() {
                        guard.clear_ready_matching(Ready::READABLE | Ready::READ_CLOSED);
                    }
                }
                Err(error) => {
                    // An ICMP error can arrive between draining MSG_ERRQUEUE and recvmsg.
                    match receive_datagram(self.fd.get_ref(), &mut self.buffer, true) {
                        Ok(datagram) => return Ok((datagram, true)),
                        Err(queue_error) if queue_error.kind() == io::ErrorKind::WouldBlock => {
                            return Err(error);
                        }
                        Err(queue_error) => return Err(queue_error),
                    }
                }
            }
        }
    }

    async fn receive(
        &mut self,
        sequence: u16,
        port: u16,
        sent: &[u8],
        started: Instant,
    ) -> io::Result<ProbeReply> {
        loop {
            let (datagram, error_queue) = self.next_datagram().await?;
            let data = &self.buffer[..datagram.length];
            if error_queue {
                if datagram.address.ip() != self.destination {
                    continue;
                }
                if self.icmp {
                    if !echo_matches(data, self.identifier, sequence) {
                        continue;
                    }
                } else if datagram.address.port() != port
                    || (!data.is_empty() && !sent.starts_with(data))
                {
                    continue;
                }
                let Some(error) = datagram.error else {
                    continue;
                };
                let ipv6 = self.destination.is_ipv6();
                let kind = match (ipv6, error.kind, error.code) {
                    (false, 5, _) => continue,
                    (false, 11, _) | (true, 3, _) => ReplyKind::TimeExceeded,
                    (false, 3, code) | (true, 1, code) => ReplyKind::Unreachable {
                        code,
                        mtu: error.info,
                    },
                    _ => return Err(io::Error::from_raw_os_error(error.errno as i32)),
                };
                let source = error
                    .offender
                    .ok_or_else(|| io::Error::from_raw_os_error(error.errno as i32))?;
                return Ok(ProbeReply {
                    source,
                    kind,
                    ttl: datagram.ttl,
                    bytes: 0,
                    elapsed: started.elapsed(),
                });
            }
            if datagram.address.ip() != self.destination {
                continue;
            }
            let (data, ttl) = if self.raw && self.destination.is_ipv4() {
                let Some(header_size) = ipv4_header_length(data) else {
                    continue;
                };
                (&data[header_size..], Some(data[8]))
            } else {
                (data, datagram.ttl)
            };
            let kind = if self.icmp {
                let reply_type = if self.destination.is_ipv4() { 0 } else { 129 };
                if !echo_matches(data, self.identifier, sequence)
                    || data[0] != reply_type
                    || data[1] != 0
                    || data.get(8..) != sent.get(8..)
                {
                    continue;
                }
                if ttl.is_none() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "echo reply has no received TTL or hop limit",
                    ));
                }
                ReplyKind::Echo
            } else {
                if datagram.address.port() != port {
                    continue;
                }
                ReplyKind::Udp
            };
            return Ok(ProbeReply {
                source: datagram.address.ip(),
                kind,
                ttl,
                bytes: data.len(),
                elapsed: started.elapsed(),
            });
        }
    }
}

fn echo_matches(data: &[u8], identifier: u16, sequence: u16) -> bool {
    data.len() >= 8
        && data[4..6] == identifier.to_be_bytes()
        && data[6..8] == sequence.to_be_bytes()
}

fn ipv4_header_length(packet: &[u8]) -> Option<usize> {
    let length = usize::from(*packet.first()? & 15) * 4;
    (packet[0] >> 4 == 4 && length >= 20 && packet.len() >= length).then_some(length)
}

fn internet_checksum(packet: &[u8]) -> u16 {
    let mut sum = 0_u32;
    for chunk in packet.chunks(2) {
        sum += u32::from(u16::from_be_bytes([chunk[0], *chunk.get(1).unwrap_or(&0)]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn set_socket_int(fd: &OwnedFd, level: i32, option: i32, value: i32) -> io::Result<()> {
    // SAFETY: value is readable for the declared size and fd is owned by the caller.
    let result = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            level,
            option,
            (&value as *const i32).cast(),
            std::mem::size_of_val(&value) as libc::socklen_t,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn socket_address_call(fd: &OwnedFd, address: SocketAddr, connect: bool) -> io::Result<()> {
    // SAFETY: zero initializes every field of sockaddr_storage, including padding.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let length = match address {
        SocketAddr::V4(address) => {
            let value = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: address.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(address.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: storage has sufficient size and alignment for sockaddr_in.
            unsafe { std::ptr::write((&mut storage as *mut libc::sockaddr_storage).cast(), value) };
            std::mem::size_of_val(&value)
        }
        SocketAddr::V6(address) => {
            let value = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: address.port().to_be(),
                sin6_flowinfo: address.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: address.ip().octets(),
                },
                sin6_scope_id: address.scope_id(),
            };
            // SAFETY: storage has sufficient size and alignment for sockaddr_in6.
            unsafe { std::ptr::write((&mut storage as *mut libc::sockaddr_storage).cast(), value) };
            std::mem::size_of_val(&value)
        }
    } as libc::socklen_t;
    // SAFETY: the pointer and length describe the initialized sockaddr variant.
    let result = unsafe {
        let pointer = (&storage as *const libc::sockaddr_storage).cast();
        if connect {
            libc::connect(fd.as_raw_fd(), pointer, length)
        } else {
            libc::bind(fd.as_raw_fd(), pointer, length)
        }
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn socket_local_address(fd: &OwnedFd) -> io::Result<SocketAddr> {
    // SAFETY: zero initializes every field of sockaddr_storage, including padding.
    let mut address: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&address) as libc::socklen_t;
    // SAFETY: address and length are writable for their declared sizes.
    let result = unsafe {
        libc::getsockname(
            fd.as_raw_fd(),
            (&mut address as *mut libc::sockaddr_storage).cast(),
            &mut length,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    socket_address(&address, length)
}

fn socket_address(
    storage: &libc::sockaddr_storage,
    length: libc::socklen_t,
) -> io::Result<SocketAddr> {
    match i32::from(storage.ss_family) {
        libc::AF_INET if length as usize >= std::mem::size_of::<libc::sockaddr_in>() => {
            // SAFETY: the family and returned length identify an initialized sockaddr_in.
            let address = unsafe {
                std::ptr::read_unaligned(
                    (storage as *const libc::sockaddr_storage).cast::<libc::sockaddr_in>(),
                )
            };
            Ok(SocketAddr::new(
                Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()).into(),
                u16::from_be(address.sin_port),
            ))
        }
        libc::AF_INET6 if length as usize >= std::mem::size_of::<libc::sockaddr_in6>() => {
            // SAFETY: the family and returned length identify an initialized sockaddr_in6.
            let address = unsafe {
                std::ptr::read_unaligned(
                    (storage as *const libc::sockaddr_storage).cast::<libc::sockaddr_in6>(),
                )
            };
            Ok(std::net::SocketAddrV6::new(
                Ipv6Addr::from(address.sin6_addr.s6_addr),
                u16::from_be(address.sin6_port),
                address.sin6_flowinfo,
                address.sin6_scope_id,
            )
            .into())
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid socket address",
        )),
    }
}

struct IcmpError {
    kind: u8,
    code: u8,
    info: u32,
    errno: u32,
    offender: Option<IpAddr>,
}

struct Datagram {
    length: usize,
    address: SocketAddr,
    ttl: Option<u8>,
    error: Option<IcmpError>,
}

fn receive_datagram(fd: &OwnedFd, buffer: &mut [u8], error_queue: bool) -> io::Result<Datagram> {
    // SAFETY: these C structures contain only integer fields and pointers; zero is valid.
    let mut address: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut control = [0_usize; 64];
    let mut vector = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    // SAFETY: all msghdr pointer fields are initialized immediately below before use.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_name = (&mut address as *mut libc::sockaddr_storage).cast();
    message.msg_namelen = std::mem::size_of_val(&address) as libc::socklen_t;
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control) as _;
    let flags = libc::MSG_DONTWAIT | if error_queue { libc::MSG_ERRQUEUE } else { 0 };
    // SAFETY: all writable buffers are alive and have the sizes declared in message.
    let length = unsafe { libc::recvmsg(fd.as_raw_fd(), &mut message, flags) };
    if length < 0 {
        return Err(io::Error::last_os_error());
    }
    if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated probe datagram",
        ));
    }
    let mut datagram = Datagram {
        length: length as usize,
        address: socket_address(&address, message.msg_namelen)?,
        ttl: None,
        error: None,
    };
    // SAFETY: recvmsg initialized a bounded, usize-aligned ancillary buffer.
    let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !header.is_null() {
        // SAFETY: CMSG_FIRSTHDR/NXTHDR point to complete cmsghdr values within control.
        let value = unsafe { &*header };
        // SAFETY: CMSG_LEN only computes the aligned header size.
        let header_length = unsafe { libc::CMSG_LEN(0) } as usize;
        // libc uses usize on glibc and socklen_t on musl for this bounded buffer.
        #[allow(clippy::unnecessary_cast)]
        let record_length = value.cmsg_len as usize;
        if record_length < header_length {
            break;
        }
        let data_length = record_length - header_length;
        // SAFETY: header points into the kernel-validated ancillary buffer.
        let data = unsafe { libc::CMSG_DATA(header) };
        match (value.cmsg_level, value.cmsg_type) {
            (libc::IPPROTO_IP, libc::IP_TTL) | (libc::IPPROTO_IPV6, libc::IPV6_HOPLIMIT)
                if data_length >= std::mem::size_of::<i32>() =>
            {
                // SAFETY: the size check covers an initialized ancillary integer.
                datagram.ttl =
                    u8::try_from(unsafe { std::ptr::read_unaligned(data.cast::<i32>()) }).ok();
            }
            (libc::IPPROTO_IP, libc::IP_RECVERR) | (libc::IPPROTO_IPV6, libc::IPV6_RECVERR)
                if data_length >= std::mem::size_of::<libc::sock_extended_err>() =>
            {
                // SAFETY: data_length covers the complete initialized error structure.
                let error =
                    unsafe { std::ptr::read_unaligned(data.cast::<libc::sock_extended_err>()) };
                let mut offender = None;
                let offset = std::mem::size_of_val(&error);
                let remaining = data_length - offset;
                if remaining >= std::mem::size_of::<libc::sa_family_t>() {
                    // SAFETY: sockaddr_storage is integer-only, and copying is bounded by both buffers.
                    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
                    let size = remaining.min(std::mem::size_of_val(&storage));
                    // SAFETY: offset and size lie within this ancillary record and storage.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            data.add(offset),
                            (&mut storage as *mut libc::sockaddr_storage).cast(),
                            size,
                        )
                    };
                    offender = socket_address(&storage, size as libc::socklen_t)
                        .ok()
                        .map(|address| address.ip());
                }
                if error.ee_origin != libc::SO_EE_ORIGIN_ICMP
                    && error.ee_origin != libc::SO_EE_ORIGIN_ICMP6
                {
                    return Err(io::Error::from_raw_os_error(error.ee_errno as i32));
                }
                datagram.error = Some(IcmpError {
                    kind: error.ee_type,
                    code: error.ee_code,
                    info: error.ee_info,
                    errno: error.ee_errno,
                    offender,
                });
            }
            _ => {}
        }
        // SAFETY: message and header still refer to the same unchanged control buffer.
        header = unsafe { libc::CMSG_NXTHDR(&message, header) };
    }
    Ok(datagram)
}

fn split_option<'a>(value: &'a str, options: &[(&'a str, &'a str)]) -> (&'a str, Option<&'a str>) {
    for &(short, long) in options {
        if let Some(argument) = value
            .strip_prefix(short)
            .filter(|argument| !argument.is_empty())
        {
            return (short, Some(argument));
        }
        if let Some(argument) = value
            .strip_prefix(long)
            .and_then(|suffix| suffix.strip_prefix('='))
        {
            return (long, Some(argument));
        }
    }
    (value, None)
}

fn option_argument(
    inline: Option<&str>,
    iter: &mut impl Iterator<Item = String>,
) -> Option<String> {
    inline.map(str::to_owned).or_else(|| iter.next())
}

fn parse_duration(value: Option<String>, name: &str) -> Result<Duration, String> {
    let value = value.ok_or_else(|| format!("{name} requires an argument"))?;
    let duration = value
        .parse::<f64>()
        .ok()
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .filter(|duration| Instant::now().checked_add(*duration).is_some())
        .ok_or_else(|| format!("bad {name}: '{value}'"))?;
    Ok(duration)
}

fn parse_value<T: std::str::FromStr>(value: Option<String>, name: &str) -> Result<T, String> {
    let value = value.ok_or_else(|| format!("{name} requires an argument"))?;
    value
        .parse()
        .map_err(|_| format!("invalid {name}: '{value}'"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtt_statistics_keep_long_run_counts_and_population_deviation() {
        let mut stats = RttStats::default();
        for _ in 0..65_536 {
            stats.record(Duration::from_micros(1000));
            stats.record(Duration::from_micros(3000));
        }
        assert_eq!(stats.received, 131_072);
        assert_eq!(stats.average_and_deviation(), (2.0, 1.0));
        assert_eq!((stats.minimum_us, stats.maximum_us), (1000, 3000));
    }

    #[test]
    fn rtt_variance_clamps_negative_roundoff_to_zero() {
        let stats = RttStats {
            received: 1,
            total_us: 1000,
            total_squared_us: 1_000_000.0 - f64::EPSILON * 1_000_000.0,
            minimum_us: 1000,
            maximum_us: 1000,
        };
        assert_eq!(stats.average_and_deviation(), (1.0, 0.0));
    }

    #[test]
    fn ping_accepts_counts_beyond_the_wire_sequence_space() {
        let options = parse_ping(
            vec![
                "ping".into(),
                "-c".into(),
                "65536".into(),
                "127.0.0.1".into(),
            ],
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(options.count, Some(65_536));
    }

    #[test]
    fn ping_requires_a_host() {
        assert!(parse_ping(vec!["ping".into()], false).is_err());
    }

    #[test]
    fn traceroute_rejects_zero_probes() {
        assert!(
            parse_trace(
                vec![
                    "traceroute".into(),
                    "-q".into(),
                    "0".into(),
                    "localhost".into()
                ],
                false
            )
            .is_err()
        );
    }
}
