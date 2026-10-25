#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::{Ipv4Addr, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use hickory_resolver::proto::op::{Message, OpCode, ResponseCode};
use hickory_resolver::proto::rr::rdata::{A, AAAA, CNAME, MX, NS, PTR, SOA, TXT};
use hickory_resolver::proto::rr::{Name, RData, Record, RecordType};

fn axe_command(applet: &str, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .args(["--applet", applet, "--"])
        .args(args)
        .env("LC_ALL", "C");
    command
}

fn axe(applet: &str, args: &[&str]) -> Output {
    axe_command(applet, args).output().expect("run AXE applet")
}

fn raw_axe(applet: &str, args: &[&str]) -> Output {
    let mut command = axe_command(applet, args);
    // SAFETY: the callback uses only stack storage and async-signal-safe syscalls
    // in the fork child. Capture/responder threads and ordinary probes stay unfiltered.
    unsafe { command.pre_exec(deny_datagram_icmp) };
    command.output().expect("run AXE with datagram ICMP denied")
}

fn deny_datagram_icmp() -> std::io::Result<()> {
    const fn statement(code: u16, value: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k: value,
        }
    }

    const fn jump(code: u16, value: u32, yes: u8, no: u8) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: yes,
            jf: no,
            k: value,
        }
    }

    // The released Linux ABIs use direct socket syscalls and little-endian u64 arguments.
    let audit_arch = if cfg!(all(
        target_arch = "x86_64",
        target_pointer_width = "64",
        target_endian = "little"
    )) {
        0xc000_003e
    } else if cfg!(all(
        target_arch = "aarch64",
        target_pointer_width = "64",
        target_endian = "little"
    )) {
        0xc000_00b7
    } else {
        return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
    };
    const LOAD: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
    const EQUAL: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
    const RETURN: u16 = (libc::BPF_RET | libc::BPF_K) as u16;
    const ARGUMENTS: u32 = std::mem::offset_of!(libc::seccomp_data, args) as u32;
    const ARGUMENT_SIZE: u32 = std::mem::size_of::<u64>() as u32;
    let mut filter = [
        statement(LOAD, std::mem::offset_of!(libc::seccomp_data, arch) as u32),
        jump(EQUAL, audit_arch, 1, 0),
        statement(RETURN, libc::SECCOMP_RET_KILL_PROCESS),
        statement(LOAD, std::mem::offset_of!(libc::seccomp_data, nr) as u32),
        jump(EQUAL, libc::SYS_socket as u32, 0, 10),
        statement(LOAD, ARGUMENTS),
        jump(EQUAL, libc::AF_INET as u32, 1, 0),
        jump(EQUAL, libc::AF_INET6 as u32, 0, 7),
        statement(LOAD, ARGUMENTS + ARGUMENT_SIZE),
        statement(
            (libc::BPF_ALU | libc::BPF_AND | libc::BPF_K) as u16,
            !(libc::SOCK_NONBLOCK as u32 | libc::SOCK_CLOEXEC as u32),
        ),
        jump(EQUAL, libc::SOCK_DGRAM as u32, 0, 4),
        statement(LOAD, ARGUMENTS + 2 * ARGUMENT_SIZE),
        jump(EQUAL, libc::IPPROTO_ICMP as u32, 1, 0),
        jump(EQUAL, libc::IPPROTO_ICMPV6 as u32, 0, 1),
        statement(RETURN, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
        statement(RETURN, libc::SECCOMP_RET_ALLOW),
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: prctl takes scalar arguments here and changes only this child.
    if unsafe {
        libc::prctl(
            libc::PR_SET_NO_NEW_PRIVS,
            1 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the kernel validates and copies the live stack-backed program before returning.
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER as libc::c_ulong,
            &program as *const libc::sock_fprog,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // Validate the installed policy before exec, so a broken mask cannot silently
    // leave the datagram path enabled while the supposed raw-fallback assertions pass.
    for (family, protocol) in [
        (libc::AF_INET, libc::IPPROTO_ICMP),
        (libc::AF_INET6, libc::IPPROTO_ICMPV6),
    ] {
        for flags in [0, libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC] {
            // SAFETY: socket has scalar arguments and acquires a descriptor only on success.
            let descriptor = unsafe { libc::socket(family, libc::SOCK_DGRAM | flags, protocol) };
            if descriptor >= 0 {
                // SAFETY: this unexpected fresh descriptor is owned by the child.
                unsafe { libc::close(descriptor) };
                return Err(std::io::Error::from_raw_os_error(libc::EPROTO));
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EPERM) {
                return Err(error);
            }
        }
    }
    Ok(())
}

fn text(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn invalid_probe_numbers_are_argument_errors_without_network_activity() {
    for (applet, option, host) in [
        ("ping", "-i", "127.0.0.1"),
        ("ping6", "-i", "::1"),
        ("ping", "-W", "127.0.0.1"),
        ("ping6", "-W", "::1"),
        ("traceroute", "-w", "127.0.0.1"),
        ("traceroute6", "-w", "::1"),
    ] {
        for value in ["-1", "NaN", "inf", "1e300"] {
            let output = axe(applet, &[option, value, host]);
            assert_eq!(
                output.status.code(),
                Some(2),
                "{applet} {option} {value}: {output:?}"
            );
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.is_empty(), "{output:?}");
        }
    }
    for applet in ["ping", "ping6"] {
        for count in ["not-a-number", "0", "-1"] {
            let output = axe(applet, &["-c", count, "127.0.0.1"]);
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.is_empty());
        }
    }
    for applet in ["ping", "traceroute"] {
        let output = axe(applet, &["-4", "::1"]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    let output = axe("host", &["-t", "not-a-type", "audit.test."]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    let output = axe("nslookup", &["-type=not-a-type", "audit.test."]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    for (applet, option) in [("host", "-WNaN"), ("nslookup", "-timeout=inf")] {
        let output = axe(applet, &[option, "audit.test."]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

fn reference_program(name: &str) -> PathBuf {
    let path = std::env::var_os("AXE_REFERENCE_PATH")
        .or_else(|| std::env::var_os("PATH"))
        .expect("AXE_REFERENCE_PATH or PATH must contain unshare and ip");
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("missing fixture tool {name}"))
}

fn isolated_namespace() -> bool {
    const TEST: &str = "namespace_wire_probe_and_dns_contracts";
    let Ok(parent_net) = std::env::var("AXE_PROBE_PARENT_NET") else {
        let output = Command::new(reference_program("unshare"))
            .args([
                "--user",
                "--map-root-user",
                "--net",
                "--mount",
                "--propagation",
                "unchanged",
                "--",
            ])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--ignored", "--nocapture"])
            .env(
                "AXE_PROBE_PARENT_NET",
                fs::read_link("/proc/self/ns/net").unwrap(),
            )
            .env(
                "AXE_PROBE_PARENT_USER",
                fs::read_link("/proc/self/ns/user").unwrap(),
            )
            .env(
                "AXE_PROBE_PARENT_MOUNT",
                fs::read_link("/proc/self/ns/mnt").unwrap(),
            )
            .output()
            .unwrap();
        assert!(output.status.success(), "namespace fixture: {output:?}");
        return false;
    };
    assert_ne!(
        fs::read_link("/proc/self/ns/net").unwrap(),
        PathBuf::from(parent_net),
        "refusing host netns mutations"
    );
    let user = File::open("/proc/self/ns/user").unwrap();
    assert_ne!(
        fs::read_link("/proc/self/ns/user").unwrap(),
        PathBuf::from(std::env::var("AXE_PROBE_PARENT_USER").unwrap()),
        "refusing host userns fixture"
    );
    assert_ne!(
        fs::read_link("/proc/self/ns/mnt").unwrap(),
        PathBuf::from(std::env::var("AXE_PROBE_PARENT_MOUNT").unwrap()),
        "refusing host mount namespace fixture"
    );
    let user = user.metadata().unwrap();
    for namespace in ["net", "mnt"] {
        let namespace = File::open(format!("/proc/self/ns/{namespace}")).unwrap();
        // SAFETY: NS_GET_USERNS takes no pointer and returns a new owned descriptor.
        let owner = unsafe { libc::ioctl(namespace.as_raw_fd(), 0xb701 as libc::c_ulong) };
        assert!(
            owner >= 0,
            "namespace owner: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the successful ioctl returned a fresh descriptor owned by this scope.
        let owner = unsafe { File::from_raw_fd(owner) }.metadata().unwrap();
        assert_eq!(
            (owner.dev(), owner.ino()),
            (user.dev(), user.ino()),
            "fixture namespace is not owned by this user namespace"
        );
    }
    // SAFETY: "/" is a valid C string; null optional arguments and flags make only
    // this verified mount namespace private before any fixture bind mount.
    let result = unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            (libc::MS_REC | libc::MS_PRIVATE) as libc::c_ulong,
            std::ptr::null(),
        )
    };
    assert_eq!(
        result,
        0,
        "private mount namespace: {}",
        std::io::Error::last_os_error()
    );
    true
}

struct ResolverFile {
    path: PathBuf,
    mounted: bool,
}

impl ResolverFile {
    fn install() -> Self {
        let path =
            std::env::temp_dir().join(format!("axe-network-resolver-{}", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let mut resolver = Self {
            path,
            mounted: false,
        };
        file.write_all(b"nameserver 127.0.0.54\noptions timeout:1 attempts:1\n")
            .unwrap();
        let source = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
        // SAFETY: the source fd and C strings remain alive; the caller has verified
        // private mount/network namespace ownership before installing this file.
        let result = unsafe {
            libc::mount(
                source.as_ptr(),
                c"/etc/resolv.conf".as_ptr(),
                std::ptr::null(),
                libc::MS_BIND as libc::c_ulong,
                std::ptr::null(),
            )
        };
        assert_eq!(
            result,
            0,
            "fixture resolver bind: {}",
            std::io::Error::last_os_error()
        );
        resolver.mounted = true;
        resolver
    }
}

impl Drop for ResolverFile {
    fn drop(&mut self) {
        if self.mounted {
            // SAFETY: this guard owns the bind mount in the verified private namespace.
            let result = unsafe { libc::umount2(c"/etc/resolv.conf".as_ptr(), libc::MNT_DETACH) };
            if !thread::panicking() {
                assert_eq!(
                    result,
                    0,
                    "fixture resolver unmount: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        let result = fs::remove_file(&self.path);
        if !thread::panicking() {
            result.expect("remove fixture resolver file");
        }
    }
}

struct Threads {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Threads {
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        }
    }
}

impl Drop for Threads {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let result = thread.join();
            if !thread::panicking() {
                result.expect("fixture worker");
            }
        }
    }
}

struct DnsFixture {
    _threads: Threads,
    queries: Arc<Mutex<Vec<(String, RecordType)>>>,
}

fn name(value: &str) -> Name {
    Name::from_utf8(value).unwrap()
}

impl DnsFixture {
    fn start() -> Self {
        let socket = UdpSocket::bind("127.0.0.54:53").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let queries = Arc::new(Mutex::new(Vec::new()));
        let observed = queries.clone();
        let mut threads = Threads::new();
        let stop = threads.stop.clone();
        threads.threads.push(thread::spawn(move || {
            let mut buffer = [0; 4096];
            while !stop.load(Ordering::Relaxed) {
                let (length, peer) = match socket.recv_from(&mut buffer) {
                    Ok(value) => value,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => panic!("DNS fixture receive: {error}"),
                };
                let query = Message::from_vec(&buffer[..length]).unwrap();
                let question = query.queries[0].clone();
                let owner = question.name().to_string();
                let kind = question.query_type();
                let mut rows = observed.lock().unwrap();
                assert!(rows.len() < 256, "unbounded DNS query loop");
                rows.push((owner.clone(), kind));
                drop(rows);
                let mut response = Message::response(query.metadata.id, OpCode::Query);
                response.metadata.authoritative = true;
                response.metadata.recursion_desired = query.metadata.recursion_desired;
                response.metadata.recursion_available = true;
                response.add_query(question);
                if owner == "missing.audit.test."
                    || owner == "43.2.0.192.in-addr.arpa."
                    || owner == "missing.followup.audit.test."
                {
                    response.metadata.response_code = ResponseCode::NXDomain;
                } else if owner == "42.2.0.192.in-addr.arpa." && kind == RecordType::PTR {
                    response.add_answer(Record::from_rdata(
                        name(&owner),
                        60,
                        RData::PTR(PTR(name("audit.test."))),
                    ));
                } else if owner == "probe.audit.test." {
                    let data = match kind {
                        RecordType::A => Some(RData::A(A(Ipv4Addr::new(127, 0, 0, 2)))),
                        RecordType::AAAA => Some(RData::AAAA(AAAA("2001:db8::2".parse().unwrap()))),
                        _ => None,
                    };
                    if let Some(data) = data {
                        response.add_answer(Record::from_rdata(name(&owner), 60, data));
                    }
                } else if owner == "alias-failed.audit.test." {
                    response.add_answer(Record::from_rdata(
                        name(&owner),
                        60,
                        RData::CNAME(CNAME(name("missing.followup.audit.test."))),
                    ));
                } else if owner == "." {
                    let data = match kind {
                        RecordType::NS => Some(RData::NS(NS(name(".")))),
                        RecordType::SOA => Some(RData::SOA(SOA::new(
                            name("."),
                            name(r"foo\.."),
                            1,
                            3600,
                            600,
                            86400,
                            60,
                        ))),
                        _ => None,
                    };
                    if let Some(data) = data {
                        response.add_answer(Record::from_rdata(name("."), 60, data));
                    }
                } else if owner == r"foo\.." && kind == RecordType::A {
                    response.add_answer(Record::from_rdata(
                        name(&owner),
                        60,
                        RData::A(A(Ipv4Addr::new(192, 0, 2, 42))),
                    ));
                } else if owner == "audit.test." || owner == "alias.audit.test." {
                    if owner == "alias.audit.test." {
                        response.add_answer(Record::from_rdata(
                            name(&owner),
                            60,
                            RData::CNAME(CNAME(name("audit.test."))),
                        ));
                    }
                    for data in [
                        RData::A(A(Ipv4Addr::new(192, 0, 2, 42))),
                        RData::AAAA(AAAA("2001:db8::42".parse().unwrap())),
                        RData::MX(MX::new(10, name("mail.audit.test."))),
                        RData::NS(NS(name("ns.audit.test."))),
                        RData::SOA(SOA::new(
                            name("ns.audit.test."),
                            name("admin.audit.test."),
                            1,
                            3600,
                            600,
                            86400,
                            60,
                        )),
                        RData::TXT(TXT::new(vec!["fixture=local-only".into()])),
                    ] {
                        if kind == RecordType::ANY || data.record_type() == kind {
                            response.add_answer(Record::from_rdata(name("audit.test."), 60, data));
                        }
                    }
                }
                socket.send_to(&response.to_vec().unwrap(), peer).unwrap();
            }
        }));
        Self {
            _threads: threads,
            queries,
        }
    }
}

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = bytes
        .chunks(2)
        .map(|pair| u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)])))
        .sum();
    while sum >> 16 != 0 {
        sum = (sum & 65535) + (sum >> 16);
    }
    !(sum as u16)
}

fn raw_ipv4(protocol: i32, source: Ipv4Addr) -> UdpSocket {
    // SAFETY: socket returns a new descriptor and has no borrowed arguments.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_RAW | libc::SOCK_CLOEXEC, protocol) };
    assert!(fd >= 0, "raw socket: {}", std::io::Error::last_os_error());
    // SAFETY: ownership of the fresh descriptor transfers exactly once.
    let socket = unsafe { UdpSocket::from_raw_fd(fd) };
    let address = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(source.octets()),
        },
        sin_zero: [0; 8],
    };
    // SAFETY: address has the declared initialized sockaddr_in layout and size.
    assert_eq!(
        unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&address as *const libc::sockaddr_in).cast(),
                std::mem::size_of_val(&address) as libc::socklen_t,
            )
        },
        0
    );
    socket
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    socket.set_ttl(37).unwrap();
    socket
}

struct IcmpFixture {
    _threads: Threads,
    _udp_listeners: Vec<UdpSocket>,
}

impl IcmpFixture {
    fn start() -> Self {
        let listeners = [33434, 33435, 53]
            .into_iter()
            .map(|port| UdpSocket::bind((Ipv4Addr::new(127, 0, 0, 2), port)).unwrap())
            .collect();
        let mut threads = Threads::new();
        for protocol in [libc::IPPROTO_ICMP, libc::IPPROTO_UDP] {
            let receiver = raw_ipv4(protocol, Ipv4Addr::UNSPECIFIED);
            let destination = raw_ipv4(libc::IPPROTO_ICMP, Ipv4Addr::new(127, 0, 0, 2));
            let router = raw_ipv4(libc::IPPROTO_ICMP, Ipv4Addr::new(127, 0, 0, 3));
            let unrelated = raw_ipv4(libc::IPPROTO_ICMP, Ipv4Addr::new(127, 0, 0, 4));
            let stop = threads.stop.clone();
            threads.threads.push(thread::spawn(move || {
                let mut buffer = [0; 65536];
                while !stop.load(Ordering::Relaxed) {
                    let (length, peer) = match receiver.recv_from(&mut buffer) {
                        Ok(value) => value,
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            continue;
                        }
                        Err(error) => panic!("ICMP fixture receive: {error}"),
                    };
                    let packet = &buffer[..length];
                    if length < 28 || packet[16..20] != [127, 0, 0, 2] {
                        continue;
                    }
                    let header = usize::from(packet[0] & 15) * 4;
                    if length < header + 8
                        || (protocol == libc::IPPROTO_ICMP && packet[header] != 8)
                    {
                        continue;
                    }
                    let expired = packet[8] == 1;
                    if !expired && protocol == libc::IPPROTO_ICMP {
                        let mut redirect = vec![5, 1, 0, 0, 127, 0, 0, 3];
                        redirect.extend_from_slice(&packet[..header + 8]);
                        let check = checksum(&redirect);
                        redirect[2..4].copy_from_slice(&check.to_be_bytes());
                        router.send_to(&redirect, peer).unwrap();
                    }
                    let mut reply = if !expired && protocol == libc::IPPROTO_ICMP {
                        let mut reply = packet[header..].to_vec();
                        reply[0] = 0;
                        reply[2..4].fill(0);
                        reply
                    } else {
                        let mut reply = vec![0; 8];
                        reply[0] = if expired { 11 } else { 3 };
                        reply[1] = if expired { 0 } else { 3 };
                        reply.extend_from_slice(&packet[..header + 8]);
                        reply
                    };
                    if expired && protocol == libc::IPPROTO_ICMP {
                        let mut stale = reply.clone();
                        stale[8 + header + 7] ^= 1;
                        let check = checksum(&stale);
                        stale[2..4].copy_from_slice(&check.to_be_bytes());
                        unrelated.send_to(&stale, peer).unwrap();
                    }
                    let check = checksum(&reply);
                    reply[2..4].copy_from_slice(&check.to_be_bytes());
                    (if expired { &router } else { &destination })
                        .send_to(&reply, peer)
                        .unwrap();
                }
            }));
        }
        Self {
            _threads: threads,
            _udp_listeners: listeners,
        }
    }
}

#[derive(Clone, Debug)]
struct Packet {
    family: u8,
    protocol: u8,
    kind: u8,
    length: usize,
    ttl: u8,
    destination_port: u16,
}

struct Capture {
    _threads: Threads,
    packets: Arc<Mutex<Vec<Packet>>>,
}

impl Capture {
    fn start() -> Self {
        // SAFETY: socket returns a new descriptor and takes no borrowed pointers.
        let fd = unsafe {
            libc::socket(
                libc::AF_PACKET,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                i32::from(3_u16.to_be()),
            )
        };
        assert!(
            fd >= 0,
            "packet capture: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the fresh descriptor transfers to the socket exactly once; recv uses no address conversion.
        let socket = unsafe { UdpSocket::from_raw_fd(fd) };
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let packets = Arc::new(Mutex::new(Vec::new()));
        let observed = packets.clone();
        let mut threads = Threads::new();
        let stop = threads.stop.clone();
        threads.threads.push(thread::spawn(move || {
            let mut buffer = [0; 65550];
            loop {
                let length = match socket.recv(&mut buffer) {
                    Ok(value) => value,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        continue;
                    }
                    Err(error) => panic!("capture receive: {error}"),
                };
                if length < 34 {
                    continue;
                }
                let bytes = &buffer[14..length];
                let (family, header, protocol, packet_length, ttl) =
                    match u16::from_be_bytes([buffer[12], buffer[13]]) {
                        0x0800 => (
                            4,
                            usize::from(bytes[0] & 15) * 4,
                            bytes[9],
                            usize::from(u16::from_be_bytes([bytes[2], bytes[3]])),
                            bytes[8],
                        ),
                        0x86dd if bytes.len() >= 40 => (
                            6,
                            40,
                            bytes[6],
                            40 + usize::from(u16::from_be_bytes([bytes[4], bytes[5]])),
                            bytes[7],
                        ),
                        _ => continue,
                    };
                if bytes.len() < header + 8 {
                    continue;
                }
                let packet = Packet {
                    family,
                    protocol,
                    length: packet_length,
                    ttl,
                    kind: bytes[header],
                    destination_port: u16::from_be_bytes([bytes[header + 2], bytes[header + 3]]),
                };
                let mut rows = observed.lock().unwrap();
                assert!(rows.len() < 4096, "unbounded fixture packet stream");
                rows.push(packet);
            }
        }));
        Self {
            _threads: threads,
            packets,
        }
    }
}

#[test]
#[ignore = "creates and verifies private user/network/mount namespaces; requires unshare and iproute2"]
fn namespace_wire_probe_and_dns_contracts() {
    if !isolated_namespace() {
        return;
    }
    let setup = Command::new(reference_program("ip"))
        .args(["link", "set", "lo", "up"])
        .output()
        .unwrap();
    assert!(setup.status.success(), "{setup:?}");
    fs::write("/proc/sys/net/ipv4/icmp_echo_ignore_all", "1\n").unwrap();
    let fixture = DnsFixture::start();
    let _resolver = ResolverFile::install();
    let responder = IcmpFixture::start();
    let capture = Capture::start();
    let ping = text(&axe("ping", &["-n", "-c1", "-W1", "127.0.0.2"]));
    assert!(
        ping.contains("64 bytes from 127.0.0.2: icmp_seq=1 ttl=37 time="),
        "{ping}"
    );
    let default_ping = text(&axe("ping", &["-n", "-c1", "-W1", "probe.audit.test."]));
    assert!(
        default_ping.starts_with("PING probe.audit.test. (127.0.0.2) "),
        "{default_ping}"
    );
    assert!(
        default_ping.contains("1 packets transmitted, 1 received"),
        "{default_ping}"
    );
    for method in [None, Some("-I"), Some("-U")] {
        let mut args = vec!["-n", "-m2", "-q1", "-w0.5", "probe.audit.test."];
        if let Some(method) = method {
            args.insert(0, method);
        }
        let trace = text(&axe("traceroute", &args));
        assert!(
            trace.starts_with("traceroute to probe.audit.test. (127.0.0.2)"),
            "{trace}"
        );
        assert!(trace.contains(" 2  127.0.0.2  "), "{trace}");
    }
    for (applet, args) in [
        ("ping", vec!["-4", "-n", "-c1", "-W1", "probe.audit.test."]),
        (
            "traceroute",
            vec!["-4", "-n", "-m2", "-q1", "-w0.5", "probe.audit.test."],
        ),
    ] {
        let output = text(&axe(applet, &args));
        assert!(output.contains("(127.0.0.2)"), "{output}");
    }
    for (applet, args, status) in [
        (
            "ping",
            vec!["-6", "-n", "-c1", "-W1", "probe.audit.test."],
            2,
        ),
        (
            "traceroute",
            vec!["-6", "-n", "-m1", "-q1", "-w0.5", "probe.audit.test."],
            1,
        ),
    ] {
        let output = axe(applet, &args);
        assert_eq!(output.status.code(), Some(status), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    for method in [None, Some("-I"), Some("-U")] {
        let mut args = vec!["-n", "-m2", "-q1", "-w0.5", "127.0.0.2"];
        if let Some(method) = method {
            args.insert(0, method);
        }
        let trace = text(&axe("traceroute", &args));
        let rows: Vec<_> = trace.lines().collect();
        assert!(rows[0].ends_with("60 byte packets"), "{trace}");
        assert!(rows[1].starts_with(" 1  127.0.0.3  "), "{trace}");
        assert!(rows[2].starts_with(" 2  127.0.0.2  "), "{trace}");
        assert!(!trace.contains('*'), "{trace}");
    }
    let ping6 = text(&axe("ping6", &["-n", "-c1", "-W1", "::1"]));
    assert!(
        ping6.starts_with("PING ::1 (::1) 56 data bytes\n"),
        "{ping6}"
    );
    assert!(
        ping6.contains("64 bytes from ::1: icmp_seq=1 ttl="),
        "{ping6}"
    );
    for (applet, address, expected_header) in [
        (
            "ping",
            "127.0.0.2",
            "PING 127.0.0.2 (127.0.0.2) from 127.0.0.1 lo: 56(84) bytes of data.",
        ),
        ("ping6", "::1", "PING ::1 (::1) from ::1 lo: 56 data bytes"),
    ] {
        let output = text(&axe(applet, &["-n", "-c1", "-W1", "-Ilo", address]));
        assert_eq!(output.lines().next(), Some(expected_header), "{output}");
        assert!(
            output.contains("1 packets transmitted, 1 received"),
            "{output}"
        );
    }
    for (applet, address, packet_length) in
        [("traceroute", "127.0.0.1", 60), ("traceroute6", "::1", 80)]
    {
        for method in [None, Some("-U")] {
            let mut args = vec!["-n", "-m1", "-q2", "-w0.5", address];
            if let Some(method) = method {
                args.insert(0, method);
            }
            let output = text(&axe(applet, &args));
            let rows: Vec<_> = output.lines().collect();
            assert_eq!(rows.len(), 2, "{applet} {args:?}: {output}");
            assert!(
                rows[0].ends_with(&format!("{packet_length} byte packets")),
                "{output}"
            );
            assert!(rows[1].starts_with(&format!(" 1  {address}  ")), "{output}");
            assert_eq!(rows[1].matches(" ms").count(), 2, "{output}");
            assert!(!rows[1].contains('*'), "{output}");
        }
    }
    for method in [None, Some("-I"), Some("-U")] {
        let mut args = vec!["-n", "-m1", "-q1", "-w0.5", "::1"];
        if let Some(method) = method {
            args.insert(0, method);
        }
        let trace = text(&axe("traceroute6", &args));
        assert!(
            trace.lines().next().unwrap().ends_with("80 byte packets"),
            "{trace}"
        );
        assert!(trace.contains(" 1  ::1  "), "{trace}");
    }
    for (applet, address, header) in [
        (
            "ping",
            "127.0.0.2",
            "PING 127.0.0.2 (127.0.0.2) 8(36) bytes of data.\n",
        ),
        ("ping6", "::1", "PING ::1 (::1) 8 data bytes\n"),
    ] {
        let output = text(&axe(applet, &["-q", "-c1", "-W1", "-s8", address]));
        assert!(output.starts_with(header), "{output}");
        assert!(
            output.contains("1 packets transmitted, 1 received, 0% packet loss, time 0ms\n\n"),
            "{output}"
        );
        assert!(!output.contains("bytes from"), "{output}");
        assert!(!output.contains("rtt min/avg"), "{output}");
    }
    let raw_ping = text(&raw_axe("ping", &["-n", "-c1", "-W1", "127.0.0.2"]));
    assert!(raw_ping.contains("icmp_seq=1 ttl=37 time="), "{raw_ping}");
    let raw_ping6 = text(&raw_axe("ping6", &["-n", "-c1", "-W1", "::1"]));
    assert!(
        raw_ping6.contains("64 bytes from ::1: icmp_seq=1 ttl="),
        "{raw_ping6}"
    );
    let raw_trace = text(&raw_axe(
        "traceroute",
        &["-I", "-n", "-m2", "-q1", "-w0.5", "127.0.0.2"],
    ));
    assert!(raw_trace.contains(" 1  127.0.0.3  "), "{raw_trace}");
    assert!(raw_trace.contains(" 2  127.0.0.2  "), "{raw_trace}");
    drop(responder);
    let packets = capture.packets.clone();
    drop(capture);
    let packets = packets.lock().unwrap();
    assert!(packets.iter().any(|packet| packet.family == 4
        && packet.protocol == 1
        && packet.kind == 0
        && packet.ttl == 37));
    assert!(
        packets
            .iter()
            .any(|packet| packet.family == 4 && packet.protocol == 1 && packet.kind == 5),
        "missing redirect notification: {packets:?}"
    );
    for family in [4, 6] {
        let length = if family == 4 { 60 } else { 80 };
        for port in [33434, 53] {
            assert!(
                packets.iter().any(|packet| packet.family == family
                    && packet.protocol == 17
                    && packet.length == length
                    && packet.destination_port == port),
                "missing UDP {family}/{port}: {packets:?}"
            );
        }
        assert!(
            packets.iter().any(|packet| packet.family == family
                && packet.protocol == if family == 4 { 1 } else { 58 }
                && packet.kind == if family == 4 { 8 } else { 128 }
                && packet.length == length),
            "missing ICMP {family}: {packets:?}"
        );
    }
    let hop_limit = ping6
        .split("ttl=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u8>()
        .unwrap();
    assert!(packets.iter().any(|packet| packet.family == 6
        && packet.protocol == 58
        && packet.kind == 129
        && packet.ttl == hop_limit));
    drop(packets);

    fixture.queries.lock().unwrap().clear();
    let server = "127.0.0.54";
    let host_header =
        "Using domain server:\nName: 127.0.0.54\nAddress: 127.0.0.54#53\nAliases: \n\n";
    let ns_header = "Server:\t\t127.0.0.54\nAddress:\t127.0.0.54#53\n\n";
    let host = text(&axe("host", &["audit.test.", server]));
    assert_eq!(
        host,
        format!(
            "{host_header}audit.test has address 192.0.2.42\naudit.test has IPv6 address 2001:db8::42\naudit.test mail is handled by 10 mail.audit.test.\n"
        )
    );
    assert_eq!(
        *fixture.queries.lock().unwrap(),
        vec![
            ("audit.test.".into(), RecordType::A),
            ("audit.test.".into(), RecordType::AAAA),
            ("audit.test.".into(), RecordType::MX)
        ]
    );
    for (kind, answer) in [
        ("A", "Name:\taudit.test\nAddress: 192.0.2.42\n"),
        ("AAAA", "Name:\taudit.test\nAddress: 2001:db8::42\n"),
        ("MX", "audit.test\tmail exchanger = 10 mail.audit.test.\n"),
    ] {
        assert_eq!(
            text(&axe(
                "nslookup",
                &[
                    "-timeout=1",
                    &format!("-type={kind}"),
                    "audit.test.",
                    server
                ]
            )),
            format!("{ns_header}{answer}\n")
        );
    }
    assert_eq!(
        text(&axe("nslookup", &["192.0.2.42", server])),
        "42.2.0.192.in-addr.arpa\tname = audit.test.\n\n"
    );
    let before = fixture.queries.lock().unwrap().len();
    assert_eq!(
        text(&axe("host", &["-tA", "192.0.2.42", server])),
        format!("{host_header}42.2.0.192.in-addr.arpa domain name pointer audit.test.\n")
    );
    assert_eq!(
        text(&axe("nslookup", &["-type=A", "192.0.2.42", server])),
        format!("{ns_header}42.2.0.192.in-addr.arpa\tname = audit.test.\n\n")
    );
    assert_eq!(
        &fixture.queries.lock().unwrap()[before..],
        &[
            ("42.2.0.192.in-addr.arpa.".into(), RecordType::PTR),
            ("42.2.0.192.in-addr.arpa.".into(), RecordType::PTR)
        ]
    );
    assert_eq!(
        text(&axe("host", &["-W1", "-tA", "alias.audit.test.", server])),
        format!(
            "{host_header}alias.audit.test is an alias for audit.test.\naudit.test has address 192.0.2.42\n"
        )
    );
    assert_eq!(
        text(&axe("nslookup", &["-type=A", "alias.audit.test.", server])),
        format!(
            "{ns_header}alias.audit.test\tcanonical name = audit.test.\nName:\taudit.test\nAddress: 192.0.2.42\n\n"
        )
    );
    let before = fixture.queries.lock().unwrap().len();
    let any = text(&axe("host", &["-a", "audit.test.", server]));
    assert!(any.starts_with(&format!("Trying \"audit.test\"\n{host_header};; ->>HEADER<<- opcode: QUERY, status: NOERROR, id: ")), "{any}");
    assert!(
        any.contains(";; flags: qr aa rd ra; QUERY: 1, ANSWER: 6, AUTHORITY: 0, ADDITIONAL: 0"),
        "{any}"
    );
    assert!(
        any.contains("audit.test.\t\t60\tIN\tTXT\t\"fixture=local-only\"\n"),
        "{any}"
    );
    assert_eq!(
        &fixture.queries.lock().unwrap()[before..],
        &[("audit.test.".into(), RecordType::ANY)]
    );
    assert_eq!(
        text(&axe("host", &["-tA", "empty.audit.test.", server])),
        format!("{host_header}empty.audit.test has no A record\n")
    );
    assert_eq!(
        text(&axe("nslookup", &["-type=A", "empty.audit.test.", server])),
        format!("{ns_header}*** Can't find empty.audit.test: No answer\n\n")
    );
    for applet in ["host", "nslookup"] {
        let option = if applet == "host" { "-tA" } else { "-type=A" };
        let output = axe(applet, &[option, "missing.audit.test.", server]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let expected = if applet == "host" {
            format!("{host_header}Host missing.audit.test. not found: 3(NXDOMAIN)\n")
        } else {
            format!("{ns_header}** server can't find missing.audit.test: NXDOMAIN\n\n")
        };
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
    for applet in ["host", "nslookup"] {
        for explicit in [false, true] {
            let mut args = vec!["192.0.2.43", server];
            if explicit {
                args.insert(0, if applet == "host" { "-tA" } else { "-type=A" });
            }
            let output = axe(applet, &args);
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            let expected = if applet == "host" {
                format!("{host_header}Host 43.2.0.192.in-addr.arpa. not found: 3(NXDOMAIN)\n")
            } else {
                format!(
                    "{}** server can't find 43.2.0.192.in-addr.arpa: NXDOMAIN\n\n",
                    if explicit { ns_header } else { "" }
                )
            };
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        }
        let before = fixture.queries.lock().unwrap().len();
        let output = axe(applet, &["alias-failed.audit.test.", server]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let expected = if applet == "host" {
            format!(
                "{host_header}alias-failed.audit.test is an alias for missing.followup.audit.test.\nHost missing.followup.audit.test. not found: 3(NXDOMAIN)\n"
            )
        } else {
            format!(
                "{ns_header}alias-failed.audit.test\tcanonical name = missing.followup.audit.test.\n** server can't find missing.followup.audit.test: NXDOMAIN\n\n"
            )
        };
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        assert_eq!(
            &fixture.queries.lock().unwrap()[before..],
            &[
                ("alias-failed.audit.test.".into(), RecordType::A),
                ("missing.followup.audit.test.".into(), RecordType::AAAA)
            ]
        );
    }
    assert_eq!(
        text(&axe("host", &["-tNS", ".", server])),
        format!("{host_header}. name server .\n")
    );
    assert_eq!(
        text(&axe("nslookup", &["-type=SOA", ".", server])),
        format!(
            "{ns_header}.\n\torigin = .\n\tmail addr = foo\\.\n\tserial = 1\n\trefresh = 3600\n\tretry = 600\n\texpire = 86400\n\tminimum = 60\n\n"
        )
    );
    assert_eq!(
        text(&axe("nslookup", &["-type=A", ".", server])),
        format!("{ns_header}*** Can't find .: No answer\n\n")
    );
    let verbose_root = text(&axe("host", &["-v", "-tA", ".", server]));
    assert!(verbose_root.starts_with("Trying \".\"\n"), "{verbose_root}");
    assert_eq!(
        text(&axe("host", &["-tA", r"foo\..", server])),
        format!("{host_header}foo\\. has address 192.0.2.42\n")
    );
    assert_eq!(
        text(&axe("nslookup", &["-type=A", r"foo\..", server])),
        format!("{ns_header}Name:\tfoo\\.\nAddress: 192.0.2.42\n\n")
    );
    assert_eq!(
        text(&axe("nslookup", &["-type=AAAA", r"foo\..", server])),
        format!("{ns_header}*** Can't find foo\\.: No answer\n\n")
    );
}
