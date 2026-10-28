use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{CStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use flate2::read::GzDecoder;
use lzma_rust2::XzReader;
use object::{Object, ObjectSection};
use vzik::procfs;

use super::proc_text::ProcText;
use super::utf8_args;

pub type AppletFn = fn(Vec<OsString>) -> i32;

pub fn commands() -> HashMap<String, AppletFn> {
    let mut commands = HashMap::new();
    commands.insert("iostat".into(), iostat as AppletFn);
    commands.insert("ipcs".into(), ipcs as AppletFn);
    commands.insert("lsmod".into(), lsmod as AppletFn);
    commands.insert("lsof".into(), lsof as AppletFn);
    commands.insert("lspci".into(), lspci as AppletFn);
    commands.insert("lsscsi".into(), lsscsi as AppletFn);
    commands.insert("lsusb".into(), lsusb as AppletFn);
    commands.insert("modinfo".into(), modinfo as AppletFn);
    commands
}

fn report(name: &str, result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{name}: {error}");
            1
        }
    }
}

pub fn lsmod(args: Vec<OsString>) -> i32 {
    report("lsmod", run_lsmod(args))
}

fn run_lsmod(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    if values
        .iter()
        .skip(1)
        .any(|value| value == "-h" || value == "--help")
    {
        println!("Usage: lsmod");
        return Ok(());
    }
    if values.len() != 1 {
        return Err("unexpected operand".into());
    }
    let modules = fs::read("/proc/modules").map_err(|error| error.to_string())?;
    println!("Module                  Size  Used by");
    for module in modules
        .split(|byte| *byte == b'\n')
        .filter_map(procfs::parse_module)
    {
        let dependencies = module
            .dependencies
            .strip_suffix(b",")
            .unwrap_or(module.dependencies);
        print!(
            "{:<19} {:>8}  {}",
            String::from_utf8_lossy(module.name),
            String::from_utf8_lossy(module.size),
            String::from_utf8_lossy(module.instances),
        );
        if !dependencies.is_empty() {
            print!(" {}", String::from_utf8_lossy(dependencies));
        }
        println!();
    }
    Ok(())
}

pub fn ipcs(args: Vec<OsString>) -> i32 {
    report("ipcs", run_ipcs(args))
}

#[derive(Clone, Copy)]
enum IpcReport {
    Default,
    Times,
    Pids,
    Creators,
    Limits,
    Summary,
}

#[derive(Clone, Copy, PartialEq)]
enum IpcKind {
    Messages,
    Memory,
    Semaphores,
}

impl IpcKind {
    fn id_column(self) -> &'static str {
        match self {
            Self::Messages => "msqid",
            Self::Memory => "shmid",
            Self::Semaphores => "semid",
        }
    }

    fn title(self, report: IpcReport) -> &'static str {
        match (self, report) {
            (Self::Messages, IpcReport::Default) => "Message Queues",
            (Self::Memory, IpcReport::Default) => "Shared Memory Segments",
            (Self::Semaphores, IpcReport::Default) => "Semaphore Arrays",
            (Self::Messages, IpcReport::Times) => "Message Queues Send/Recv/Change Times",
            (Self::Memory, IpcReport::Times) => "Shared Memory Attach/Detach/Change Times",
            (Self::Semaphores, IpcReport::Times) => "Semaphore Operation/Change Times",
            (Self::Messages, IpcReport::Pids) => "Message Queues PIDs",
            (Self::Memory, IpcReport::Pids) => "Shared Memory Creator/Last-op PIDs",
            (Self::Semaphores, IpcReport::Pids) => "",
            (Self::Messages, IpcReport::Creators) => "Message Queues Creators/Owners",
            (Self::Memory, IpcReport::Creators) => "Shared Memory Segment Creators/Owners",
            (Self::Semaphores, IpcReport::Creators) => "Semaphore Arrays Creators/Owners",
            (Self::Messages, IpcReport::Limits) => "Messages Limits",
            (Self::Memory, IpcReport::Limits) => "Shared Memory Limits",
            (Self::Semaphores, IpcReport::Limits) => "Semaphore Limits",
            (Self::Messages, IpcReport::Summary) => "Messages Status",
            (Self::Memory, IpcReport::Summary) => "Shared Memory Status",
            (Self::Semaphores, IpcReport::Summary) => "Semaphore Status",
        }
    }
}

fn run_ipcs(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut shm = false;
    let mut sem = false;
    let mut msg = false;
    let mut report = IpcReport::Default;
    let mut id = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        if value == "--help" || value == "-h" {
            println!("Usage: ipcs [-m|-q|-s] [-a] [-t|-p|-c|-l|-u] [-i ID]");
            return Ok(());
        }
        if value == "--id" || value == "-i" {
            id = Some(
                iter.next()
                    .ok_or("-i requires an ID")?
                    .parse::<i32>()
                    .map_err(|_| "invalid IPC ID")?,
            );
            continue;
        }
        let flags = match value.as_str() {
            "--shmems" => "m",
            "--semaphores" => "s",
            "--queues" => "q",
            "--all" => "a",
            "--time" => "t",
            "--pid" => "p",
            "--creator" => "c",
            "--limits" => "l",
            "--summary" => "u",
            _ => value
                .strip_prefix('-')
                .ok_or_else(|| format!("unexpected operand '{value}'"))?,
        };
        if flags.is_empty() {
            return Err(format!("unknown option '{value}'"));
        }
        for flag in flags.chars() {
            match flag {
                'm' => shm = true,
                's' => sem = true,
                'q' => msg = true,
                'a' => {
                    shm = true;
                    sem = true;
                    msg = true;
                }
                't' => report = IpcReport::Times,
                'p' => report = IpcReport::Pids,
                'c' => report = IpcReport::Creators,
                'l' => report = IpcReport::Limits,
                'u' => report = IpcReport::Summary,
                _ => return Err(format!("unknown option '{value}'")),
            }
        }
    }
    if id.is_some() && u8::from(shm) + u8::from(sem) + u8::from(msg) != 1 {
        return Err("when using an ID, a single resource must be specified".into());
    }
    if !shm && !sem && !msg {
        shm = true;
        sem = true;
        msg = true;
    }
    let names = std::cell::LazyCell::new(NameDatabase::load);
    if id.is_none() {
        println!();
    }
    for (selected, kind) in [
        (msg, IpcKind::Messages),
        (shm, IpcKind::Memory),
        (sem, IpcKind::Semaphores),
    ] {
        if !selected {
            continue;
        }
        if let Some(id) = id {
            let entries = read_ipc_entries(kind)?;
            if let Some(entry) = entries.iter().find(|entry| entry.id == id) {
                print_ipc_id(kind, entry)?;
            } else {
                eprintln!("ipcs: id {id} not found");
            }
            continue;
        }
        match report {
            IpcReport::Limits => print_ipc_limits(kind)?,
            IpcReport::Summary => print_ipc_summary(kind)?,
            _ => print_ipc_table(kind, report, &read_ipc_entries(kind)?, &names)?,
        }
        println!();
    }
    Ok(())
}

struct IpcEntry {
    key: u32,
    id: i32,
    mode: u32,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    size: u64,
    count: u64,
    first_pid: u64,
    last_pid: u64,
    times: [i64; 3],
}

fn read_ipc_entries(kind: IpcKind) -> Result<Vec<IpcEntry>, String> {
    let path = match kind {
        IpcKind::Messages => "/proc/sysvipc/msg",
        IpcKind::Memory => "/proc/sysvipc/shm",
        IpcKind::Semaphores => "/proc/sysvipc/sem",
    };
    let content = fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
    let mut lines = content.lines();
    let headers = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let row = line.split_whitespace().collect::<Vec<_>>();
        let text = |name: &str| {
            headers
                .iter()
                .position(|header| *header == name)
                .and_then(|index| row.get(index).copied())
                .ok_or_else(|| format!("{path}: missing {name} column"))
        };
        let number = |name: &str| {
            text(name)?
                .parse::<u64>()
                .map_err(|_| format!("{path}: invalid {name} value"))
        };
        let time = |name: &str| {
            text(name)?
                .parse::<i64>()
                .map_err(|_| format!("{path}: invalid {name} value"))
        };
        let (size, count, first_pid, last_pid, times) = match kind {
            IpcKind::Messages => (
                number("cbytes")?,
                number("qnum")?,
                number("lspid")?,
                number("lrpid")?,
                [time("stime")?, time("rtime")?, time("ctime")?],
            ),
            IpcKind::Memory => (
                number("size")?,
                number("nattch")?,
                number("cpid")?,
                number("lpid")?,
                [time("atime")?, time("dtime")?, time("ctime")?],
            ),
            IpcKind::Semaphores => (
                0,
                number("nsems")?,
                0,
                0,
                [time("otime")?, 0, time("ctime")?],
            ),
        };
        entries.push(IpcEntry {
            key: text("key")?.parse::<i64>().map_err(|_| "invalid IPC key")? as u32,
            id: text(kind.id_column())?
                .parse()
                .map_err(|_| "invalid IPC ID")?,
            mode: u32::from_str_radix(text("perms")?, 8).map_err(|_| "invalid IPC permissions")?,
            uid: number("uid")?.try_into().map_err(|_| "invalid IPC UID")?,
            gid: number("gid")?.try_into().map_err(|_| "invalid IPC GID")?,
            cuid: number("cuid")?
                .try_into()
                .map_err(|_| "invalid IPC creator UID")?,
            cgid: number("cgid")?
                .try_into()
                .map_err(|_| "invalid IPC creator GID")?,
            size,
            count,
            first_pid,
            last_pid,
            times,
        });
    }
    Ok(entries)
}

fn ipc_time(timestamp: i64, short: bool) -> Result<String, String> {
    use chrono::TimeZone;
    if timestamp == 0 {
        return Ok("Not set".into());
    }
    let time = chrono::Local
        .timestamp_opt(timestamp, 0)
        .single()
        .ok_or("IPC timestamp out of range")?;
    Ok(time
        .format(if short { "%b %e %T " } else { "%a %b %e %T %Y" })
        .to_string())
}

fn print_ipc_table(
    kind: IpcKind,
    report: IpcReport,
    entries: &[IpcEntry],
    names: &NameDatabase,
) -> Result<(), String> {
    if kind == IpcKind::Semaphores && matches!(report, IpcReport::Pids) {
        return Ok(());
    }
    println!("------ {} --------", kind.title(report));
    let id = kind.id_column();
    match report {
        IpcReport::Default => match kind {
            IpcKind::Messages => println!(
                "{:<10} {:<10} {:<10} {:<10} {:<12} {:<12}",
                "key", id, "owner", "perms", "used-bytes", "messages"
            ),
            IpcKind::Memory => println!(
                "{:<10} {:<10} {:<10} {:<10} {:<10} {:<10} {:<12}",
                "key", id, "owner", "perms", "bytes", "nattch", "status"
            ),
            IpcKind::Semaphores => println!(
                "{:<10} {:<10} {:<10} {:<10} {:<10}",
                "key", id, "owner", "perms", "nsems"
            ),
        },
        IpcReport::Creators => println!(
            "{id:<10} {:<10} {:<10} {:<10} {:<10} {:<10}",
            "perms", "cuid", "cgid", "uid", "gid"
        ),
        IpcReport::Pids => {
            let (first, last) = if kind == IpcKind::Memory {
                ("cpid", "lpid")
            } else {
                ("lspid", "lrpid")
            };
            println!("{id:<10} {:<10} {first:<10} {last:<10}", "owner");
        }
        IpcReport::Times => match kind {
            IpcKind::Memory => println!(
                "{id:<10} {:<10} {:<20} {:<20} {:<20}",
                "owner", "attached", "detached", "changed"
            ),
            IpcKind::Messages => println!(
                "{id:<8} {:<10} {:<20} {:<20} {:<20}",
                "owner", "send", "recv", "change"
            ),
            IpcKind::Semaphores => println!(
                "{id:<8} {:<10} {:<26} {:<26}",
                "owner", "last-op", "last-changed"
            ),
        },
        IpcReport::Limits | IpcReport::Summary => unreachable!(),
    }
    for entry in entries {
        let user = names.user(entry.uid);
        let owner = truncate(&user, 10);
        match report {
            IpcReport::Default => {
                print!(
                    "0x{:08x} {:<10} {owner:<10} {:<10o} ",
                    entry.key,
                    entry.id,
                    entry.mode & 0o777
                );
                match kind {
                    IpcKind::Messages => println!("{:<12} {:<12}", entry.size, entry.count),
                    IpcKind::Memory => println!(
                        "{:<10} {:<10} {:<6} {:<6}",
                        entry.size,
                        entry.count,
                        if entry.mode & 0o1000 != 0 {
                            "dest"
                        } else {
                            " "
                        },
                        if entry.mode & 0o2000 != 0 {
                            "locked"
                        } else {
                            " "
                        }
                    ),
                    IpcKind::Semaphores => println!("{:<10}", entry.count),
                }
            }
            IpcReport::Creators => println!(
                "{:<10} {:<10o} {:<10} {:<10} {:<10} {:<10}",
                entry.id,
                entry.mode & 0o777,
                truncate(&names.user(entry.cuid), 10),
                truncate(&names.group(entry.cgid), 10),
                owner,
                truncate(&names.group(entry.gid), 10)
            ),
            IpcReport::Pids => {
                if kind == IpcKind::Memory {
                    println!(
                        "{:<10} {owner:<10} {:<10} {:<10}",
                        entry.id, entry.first_pid, entry.last_pid
                    );
                } else {
                    println!(
                        "{:<8} {owner:<10}  {:>5}     {:>5}",
                        entry.id, entry.first_pid, entry.last_pid
                    );
                }
            }
            IpcReport::Times => {
                if kind == IpcKind::Semaphores {
                    println!(
                        "{:<8} {owner:<10}  {:<26} {:<26}",
                        entry.id,
                        ipc_time(entry.times[0], false)?,
                        ipc_time(entry.times[2], false)?
                    );
                } else {
                    let width = if kind == IpcKind::Memory { 10 } else { 8 };
                    println!(
                        "{:<width$} {owner:<10} {:<20} {:<20} {:<20}",
                        entry.id,
                        ipc_time(entry.times[0], true)?,
                        ipc_time(entry.times[1], true)?,
                        ipc_time(entry.times[2], true)?
                    );
                }
            }
            IpcReport::Limits | IpcReport::Summary => unreachable!(),
        }
    }
    Ok(())
}

fn ipc_mode(mode: u32) -> String {
    if mode == 0 {
        "0".into()
    } else {
        format!("0{mode:o}")
    }
}

fn print_ipc_id(kind: IpcKind, entry: &IpcEntry) -> Result<(), String> {
    match kind {
        IpcKind::Memory => {
            println!("\nShared memory Segment shmid={}", entry.id);
            println!(
                "uid={}\tgid={}\tcuid={}\tcgid={}",
                entry.uid, entry.gid, entry.cuid, entry.cgid
            );
            println!(
                "mode={}\taccess_perms={}",
                ipc_mode(entry.mode),
                ipc_mode(entry.mode & 0o777)
            );
            println!(
                "bytes={}\tlpid={}\tcpid={}\tnattch={}",
                entry.size, entry.last_pid, entry.first_pid, entry.count
            );
            println!(
                "att_time={:<26}\ndet_time={:<26}\nchange_time={:<26}",
                ipc_time(entry.times[0], false)?,
                ipc_time(entry.times[1], false)?,
                ipc_time(entry.times[2], false)?
            );
        }
        IpcKind::Messages => {
            let mut data = std::mem::MaybeUninit::<libc::msqid_ds>::uninit();
            // SAFETY: IPC_STAT initializes the complete, correctly sized msqid_ds.
            if unsafe { libc::msgctl(entry.id, libc::IPC_STAT, data.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            // SAFETY: the successful IPC_STAT call initialized data.
            let data = unsafe { data.assume_init() };
            println!("\nMessage Queue msqid={}", entry.id);
            println!(
                "uid={}\tgid={}\tcuid={}\tcgid={}\tmode={}",
                entry.uid,
                entry.gid,
                entry.cuid,
                entry.cgid,
                ipc_mode(entry.mode)
            );
            println!(
                "cbytes={}\tqbytes={}\tqnum={}\tlspid={}\tlrpid={}",
                entry.size, data.msg_qbytes, entry.count, entry.first_pid, entry.last_pid
            );
            println!(
                "send_time={:<26}\nrcv_time={:<26}\nchange_time={:<26}",
                ipc_time(entry.times[0], false)?,
                ipc_time(entry.times[1], false)?,
                ipc_time(entry.times[2], false)?
            );
        }
        IpcKind::Semaphores => {
            println!("\nSemaphore Array semid={}", entry.id);
            println!(
                "uid={}\t gid={}\t cuid={}\t cgid={}",
                entry.uid, entry.gid, entry.cuid, entry.cgid
            );
            println!(
                "mode={}, access_perms={}",
                ipc_mode(entry.mode),
                ipc_mode(entry.mode & 0o777)
            );
            println!("nsems = {}", entry.count);
            println!(
                "otime = {:<26}\nctime = {:<26}",
                ipc_time(entry.times[0], false)?,
                ipc_time(entry.times[2], false)?
            );
            println!(
                "{:<10} {:<10} {:<10} {:<10} {:<10}",
                "semnum", "value", "ncount", "zcount", "pid"
            );
            for index in 0..entry.count {
                let get = |command| {
                    // SAFETY: these commands only read a semaphore; the unused argument is zero.
                    let value = unsafe {
                        libc::semctl(entry.id, index as libc::c_int, command, 0 as libc::c_long)
                    };
                    if value < 0 {
                        Err(io::Error::last_os_error().to_string())
                    } else {
                        Ok(value)
                    }
                };
                println!(
                    "{index:<10} {:<10} {:<10} {:<10} {:<10}",
                    get(libc::GETVAL)?,
                    get(libc::GETNCNT)?,
                    get(libc::GETZCNT)?,
                    get(libc::GETPID)?
                );
            }
        }
    }
    println!();
    Ok(())
}

#[repr(C)]
#[derive(Default)]
struct IpcShmInfo {
    used_ids: libc::c_int,
    total: libc::c_ulong,
    resident: libc::c_ulong,
    swapped: libc::c_ulong,
    swap_attempts: libc::c_ulong,
    swap_successes: libc::c_ulong,
}

#[repr(C)]
#[derive(Default)]
struct IpcSemInfo {
    semmap: libc::c_int,
    semmni: libc::c_int,
    semmns: libc::c_int,
    semmnu: libc::c_int,
    semmsl: libc::c_int,
    semopm: libc::c_int,
    semume: libc::c_int,
    semusz: libc::c_int,
    semvmx: libc::c_int,
    semaem: libc::c_int,
}

#[repr(C)]
union IpcSemArg {
    info: *mut IpcSemInfo,
}

fn print_ipc_summary(kind: IpcKind) -> Result<(), String> {
    println!("------ {} --------", kind.title(IpcReport::Summary));
    match kind {
        IpcKind::Messages => {
            let mut info = std::mem::MaybeUninit::<libc::msginfo>::uninit();
            // SAFETY: MSG_INFO writes msginfo, despite the historical msgctl pointer type.
            if unsafe { libc::msgctl(0, libc::MSG_INFO, info.as_mut_ptr().cast()) } < 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            // SAFETY: MSG_INFO succeeded and initialized info.
            let info = unsafe { info.assume_init() };
            println!(
                "allocated queues = {}\nused headers = {}\nused space = {} bytes",
                info.msgpool, info.msgmap, info.msgtql
            );
        }
        IpcKind::Memory => {
            let mut info = IpcShmInfo::default();
            // SAFETY: Linux SHM_INFO (14) writes the repr(C) shm_info layout, not shmid_ds.
            if unsafe { libc::shmctl(0, 14, std::ptr::from_mut(&mut info).cast()) } < 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            println!(
                "segments allocated {}\npages allocated {}\npages resident  {}\npages swapped   {}\nSwap performance: {} attempts\t {} successes",
                info.used_ids,
                info.total,
                info.resident,
                info.swapped,
                info.swap_attempts,
                info.swap_successes
            );
        }
        IpcKind::Semaphores => {
            let mut info = IpcSemInfo::default();
            // SAFETY: SEM_INFO writes seminfo through the semun pointer member.
            if unsafe { libc::semctl(0, 0, libc::SEM_INFO, IpcSemArg { info: &mut info }) } < 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            println!(
                "used arrays = {}\nallocated semaphores = {}",
                info.semusz, info.semaem
            );
        }
    }
    Ok(())
}

fn print_ipc_limits(kind: IpcKind) -> Result<(), String> {
    let read = |name| {
        let path = format!("/proc/sys/kernel/{name}");
        fs::read_to_string(&path)
            .map_err(|error| format!("{path}: {error}"))?
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("{path}: invalid limit"))
    };
    println!("------ {} --------", kind.title(IpcReport::Limits));
    match kind {
        IpcKind::Messages => {
            println!("max queues system wide = {}", read("msgmni")?);
            println!("max size of message (bytes) = {}", read("msgmax")?);
            println!("default max size of queue (bytes) = {}", read("msgmnb")?);
        }
        IpcKind::Memory => {
            // SAFETY: sysconf has no pointer arguments or side effects.
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            let page_kib = u64::try_from(page_size).map_err(|_| "cannot read page size")? / 1024;
            let pages = read("shmall")?;
            let total = pages
                .checked_mul(page_kib)
                .unwrap_or(u64::MAX - u64::MAX % page_kib);
            println!("max number of segments = {}", read("shmmni")?);
            println!("max seg size (kbytes) = {}", read("shmmax")? / 1024);
            println!("max total shared memory (kbytes) = {total}");
            println!("min seg size (bytes) = 1");
        }
        IpcKind::Semaphores => {
            let content =
                fs::read_to_string("/proc/sys/kernel/sem").map_err(|error| error.to_string())?;
            let limits = content
                .split_whitespace()
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "invalid semaphore limits")?;
            let [per_array, total, operations, arrays] = limits.as_slice() else {
                return Err("invalid semaphore limits".into());
            };
            println!(
                "max number of arrays = {arrays}\nmax semaphores per array = {per_array}\nmax semaphores system wide = {total}\nmax ops per semop call = {operations}\nsemaphore max value = 32767"
            );
        }
    }
    Ok(())
}
pub fn iostat(args: Vec<OsString>) -> i32 {
    report("iostat", run_iostat(args))
}

#[derive(Default)]
struct IoOptions {
    cpu: bool,
    disk: bool,
    extended: bool,
    megabytes: bool,
    omit_zero: bool,
    interval: Option<u64>,
    count: Option<u64>,
    human: bool,
    omit_first: bool,
    devices: BTreeSet<String>,
}

fn run_iostat(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut options = IoOptions::default();
    let mut numbers = Vec::new();
    for value in values.into_iter().skip(1) {
        if value == "--help" {
            println!(
                "Usage: iostat [-cdxhmkzy] [DEVICE ...] [INTERVAL [COUNT]]\nThe first report is since boot; later reports cover the elapsed interval.\n-x adds await, aqu-sz and %util; full sysstat extended columns and -o JSON are unsupported."
            );
            return Ok(());
        }
        if let Some(flags) = value.strip_prefix('-') {
            if flags.is_empty() {
                return Err(format!("unknown option '{value}'"));
            }
            for flag in flags.chars() {
                match flag {
                    'c' => options.cpu = true,
                    'd' => options.disk = true,
                    'x' => options.extended = true,
                    'm' => options.megabytes = true,
                    'k' => options.megabytes = false,
                    'z' => options.omit_zero = true,
                    'y' => options.omit_first = true,
                    'h' => options.human = true,
                    _ => return Err(format!("unknown option '{value}'")),
                }
            }
        } else if value.bytes().all(|byte| byte.is_ascii_digit()) && !value.is_empty() {
            numbers.push(
                value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid interval/count '{value}'"))?,
            );
        } else if numbers.is_empty() {
            if value == "ALL" {
                options.devices.clear();
            } else {
                let device = value.strip_prefix("/dev/").unwrap_or(&value);
                options.devices.insert(device.to_owned());
            }
        } else {
            return Err(format!("unexpected device after interval/count '{value}'"));
        }
    }
    if !options.cpu && !options.disk {
        options.cpu = true;
        options.disk = true;
    }
    options.disk |= options.extended;
    options.interval = numbers.first().copied();
    options.count = numbers.get(1).copied();
    if numbers.len() > 2 {
        return Err("too many arguments".into());
    }
    if options.count == Some(0) {
        return Err("count must be greater than zero".into());
    }
    if options.interval == Some(0) {
        options.interval = None;
        options.count = Some(1);
    }

    let reports = options.count.unwrap_or(if options.interval.is_some() {
        u64::MAX
    } else {
        1
    });
    let mut previous = if options.omit_first && options.interval.is_some() {
        let snapshot = IoSnapshot::read()?;
        thread::sleep(Duration::from_secs(options.interval.unwrap_or(1)));
        Some(snapshot)
    } else {
        None
    };
    for index in 0..reports {
        let current = IoSnapshot::read()?;
        if index == 0 {
            print_iostat_banner(current.cpu_count)?;
        }
        print_iostat(&current, previous.as_ref(), &options);
        previous = Some(current);
        if index + 1 < reports {
            thread::sleep(Duration::from_secs(options.interval.unwrap_or(1)));
        }
    }
    Ok(())
}

#[derive(Clone)]
struct IoSnapshot {
    uptime: f64,
    sampled_at: Instant,
    cpu: [u64; 8],
    cpu_count: usize,
    disks: BTreeMap<String, DiskStat>,
}

#[derive(Clone, Default)]
struct DiskStat {
    reads: u64,
    sectors_read: u64,
    read_ms: u64,
    writes: u64,
    sectors_written: u64,
    write_ms: u64,
    discards: u64,
    sectors_discarded: u64,
    discard_ms: u64,
    io_ms: u64,
    weighted_ms: u64,
}

impl IoSnapshot {
    fn read() -> Result<Self, String> {
        let stat = fs::read_to_string("/proc/stat").map_err(|error| error.to_string())?;
        let cpu_line = stat
            .lines()
            .find(|line| line.starts_with("cpu "))
            .ok_or("missing aggregate CPU line")?;
        let mut cpu = [0u64; 8];
        for (slot, value) in cpu.iter_mut().zip(cpu_line.split_whitespace().skip(1)) {
            *slot = value.parse().map_err(|_| "invalid /proc/stat CPU value")?;
        }
        let mut cpu_count = stat
            .lines()
            .filter(|line| {
                line.as_bytes().get(3).is_some_and(u8::is_ascii_digit) && line.starts_with("cpu")
            })
            .count();
        if cpu_count == 0 {
            // SAFETY: sysconf reads the online CPU count without pointer arguments.
            cpu_count = usize::try_from(unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) })
                .ok()
                .filter(|count| *count != 0)
                .ok_or("cannot determine CPU count")?;
        }
        let diskstats = fs::read_to_string("/proc/diskstats").map_err(|error| error.to_string())?;
        let mut disks = BTreeMap::new();
        for line in diskstats.lines() {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 14 {
                continue;
            }
            let parse = |index: usize| fields[index].parse::<u64>().unwrap_or_default();
            disks.insert(
                fields[2].into(),
                DiskStat {
                    reads: parse(3),
                    sectors_read: parse(5),
                    read_ms: parse(6),
                    writes: parse(7),
                    sectors_written: parse(9),
                    write_ms: parse(10),
                    discards: fields
                        .get(14)
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                    sectors_discarded: fields
                        .get(16)
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                    discard_ms: fields
                        .get(17)
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                    io_ms: parse(12),
                    weighted_ms: parse(13),
                },
            );
        }
        Ok(Self {
            uptime: uptime_seconds()?,
            sampled_at: Instant::now(),
            cpu,
            cpu_count,
            disks,
        })
    }
}

fn print_iostat_banner(cpu_count: usize) -> Result<(), String> {
    let mut identity = std::mem::MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: uname initializes the complete utsname and NUL-terminates its string fields.
    if unsafe { libc::uname(identity.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    // SAFETY: the successful uname call initialized all fields.
    let identity = unsafe { identity.assume_init() };
    // SAFETY: these fields are NUL-terminated strings supplied by uname.
    let (system, release, host, machine) = unsafe {
        (
            CStr::from_ptr(identity.sysname.as_ptr()),
            CStr::from_ptr(identity.release.as_ptr()),
            CStr::from_ptr(identity.nodename.as_ptr()),
            CStr::from_ptr(identity.machine.as_ptr()),
        )
    };
    let format = if std::env::var("S_TIME_FORMAT").is_ok_and(|value| value == "ISO") {
        "%Y-%m-%d"
    } else {
        "%m/%d/%y"
    };
    let date = if std::env::var("S_TIME_DEF_TIME").is_ok_and(|value| value == "UTC") {
        chrono::Utc::now().format(format).to_string()
    } else {
        chrono::Local::now().format(format).to_string()
    };
    println!(
        "{} {} ({}) \t{date} \t_{}_\t({cpu_count} CPU)\n",
        system.to_string_lossy(),
        release.to_string_lossy(),
        host.to_string_lossy(),
        machine.to_string_lossy()
    );
    Ok(())
}

fn print_iostat(current: &IoSnapshot, previous: Option<&IoSnapshot>, options: &IoOptions) {
    let rate = previous
        .map_or(current.uptime, |old| {
            current
                .sampled_at
                .duration_since(old.sampled_at)
                .as_secs_f64()
        })
        .max(f64::EPSILON);
    if options.cpu {
        let cpu = delta_array(current.cpu, previous.map(|value| value.cpu));
        let total = cpu.iter().sum::<u64>().max(1) as f64;
        let pct = |value: u64| value as f64 * 100.0 / total;
        println!("avg-cpu:  %user   %nice %system %iowait  %steal   %idle");
        println!(
            "       {:8.2}{:8.2}{:8.2}{:8.2}{:8.2}{:8.2}\n",
            pct(cpu[0]),
            pct(cpu[1]),
            pct(cpu[2].saturating_add(cpu[5]).saturating_add(cpu[6])),
            pct(cpu[4]),
            pct(cpu[7]),
            pct(cpu[3])
        );
    }
    if options.disk {
        let headings = if options.human {
            [
                "bytes_read/s",
                "bytes_wrtn/s",
                "bytes_dscd/s",
                "bytes_read",
                "bytes_wrtn",
                "bytes_dscd",
            ]
        } else if options.megabytes {
            [
                "MB_read/s",
                "MB_wrtn/s",
                "MB_dscd/s",
                "MB_read",
                "MB_wrtn",
                "MB_dscd",
            ]
        } else {
            [
                "kB_read/s",
                "kB_wrtn/s",
                "kB_dscd/s",
                "kB_read",
                "kB_wrtn",
                "kB_dscd",
            ]
        };
        println!(
            "{:<13} {:>8} {:>12} {:>12} {:>12} {:>10} {:>10} {:>10}",
            "Device",
            "tps",
            headings[0],
            headings[1],
            headings[2],
            headings[3],
            headings[4],
            headings[5]
        );
        for (name, stat) in &current.disks {
            if !options.devices.is_empty() && !options.devices.contains(name) {
                continue;
            }
            let old = previous.and_then(|snapshot| snapshot.disks.get(name));
            let delta = disk_delta(stat, old);
            let operations = delta
                .reads
                .saturating_add(delta.writes)
                .saturating_add(delta.discards);
            if options.omit_zero && operations == 0 {
                continue;
            }
            if options.human {
                let read_rate = human_io_size(delta.sectors_read as f64 * 512.0 / rate);
                let write_rate = human_io_size(delta.sectors_written as f64 * 512.0 / rate);
                let discard_rate = human_io_size(delta.sectors_discarded as f64 * 512.0 / rate);
                let read_total = human_io_size(delta.sectors_read as f64 * 512.0);
                let write_total = human_io_size(delta.sectors_written as f64 * 512.0);
                let discard_total = human_io_size(delta.sectors_discarded as f64 * 512.0);
                println!(
                    "{name:<13} {:8.2} {:>12} {:>12} {:>12} {:>10} {:>10} {:>10}",
                    operations as f64 / rate,
                    read_rate,
                    write_rate,
                    discard_rate,
                    read_total,
                    write_total,
                    discard_total
                );
            } else {
                let divisor = if options.megabytes { 2048 } else { 2 };
                println!(
                    "{name:<13} {:8.2} {:12.2} {:12.2} {:12.2} {:10} {:10} {:10}",
                    operations as f64 / rate,
                    delta.sectors_read as f64 / divisor as f64 / rate,
                    delta.sectors_written as f64 / divisor as f64 / rate,
                    delta.sectors_discarded as f64 / divisor as f64 / rate,
                    delta.sectors_read / divisor,
                    delta.sectors_written / divisor,
                    delta.sectors_discarded / divisor
                );
            }
            if options.extended {
                let await_ms = if operations == 0 {
                    0.0
                } else {
                    (delta.read_ms + delta.write_ms + delta.discard_ms) as f64 / operations as f64
                };
                println!(
                    "             await={await_ms:.2} aqu-sz={:.2} %util={:.2}",
                    delta.weighted_ms as f64 / 1000.0 / rate,
                    delta.io_ms as f64 / 10.0 / rate
                );
            }
        }
    }
    println!();
    println!();
}

fn human_io_size(mut bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut unit = 0;
    while bytes >= 1024.0 && unit + 1 < UNITS.len() {
        bytes /= 1024.0;
        unit += 1;
    }
    if bytes >= 10.0 || unit == 0 {
        format!("{bytes:.0}{}", UNITS[unit])
    } else {
        format!("{bytes:.1}{}", UNITS[unit])
    }
}

fn delta_array(current: [u64; 8], old: Option<[u64; 8]>) -> [u64; 8] {
    let old = old.unwrap_or([0; 8]);
    std::array::from_fn(|index| current[index].saturating_sub(old[index]))
}

fn disk_delta(current: &DiskStat, old: Option<&DiskStat>) -> DiskStat {
    let zero = DiskStat::default();
    let old = old.unwrap_or(&zero);
    DiskStat {
        reads: current.reads.saturating_sub(old.reads),
        sectors_read: current.sectors_read.saturating_sub(old.sectors_read),
        read_ms: current.read_ms.saturating_sub(old.read_ms),
        writes: current.writes.saturating_sub(old.writes),
        sectors_written: current.sectors_written.saturating_sub(old.sectors_written),
        write_ms: current.write_ms.saturating_sub(old.write_ms),
        discards: current.discards.saturating_sub(old.discards),
        sectors_discarded: current
            .sectors_discarded
            .saturating_sub(old.sectors_discarded),
        discard_ms: current.discard_ms.saturating_sub(old.discard_ms),
        io_ms: current.io_ms.saturating_sub(old.io_ms),
        weighted_ms: current.weighted_ms.saturating_sub(old.weighted_ms),
    }
}

fn uptime_seconds() -> Result<f64, String> {
    let content = fs::read_to_string("/proc/uptime").map_err(|error| error.to_string())?;
    let uptime = content
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or("invalid /proc/uptime")?;
    Ok(uptime)
}

pub fn lsof(args: Vec<OsString>) -> i32 {
    match run_lsof(args) {
        Ok(matched) => i32::from(!matched),
        Err(error) => report("lsof", Err(error)),
    }
}

fn run_lsof(args: Vec<OsString>) -> Result<bool, String> {
    let values = utf8_args(args)?;
    let names = NameDatabase::load();
    let mut pids = BTreeSet::new();
    let mut user_filter = BTreeSet::new();
    let mut command_filters = Vec::new();
    let mut terse = false;
    let mut fields = None;
    let mut numeric_hosts = false;
    let mut numeric_ports = false;
    let mut and_selection = false;
    let mut command_width = 9;
    let mut paths = Vec::new();
    let mut recursive_directories = Vec::new();
    let mut directories = Vec::new();
    let mut networks = Vec::new();
    let mut iter = values.into_iter().skip(1).peekable();
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-p" => parse_pid_list(&iter.next().ok_or("-p requires a PID")?, &mut pids)?,
            "-c" => command_filters.push(iter.next().ok_or("-c requires a command")?),
            "-u" => parse_user_list(
                &iter.next().ok_or("-u requires a user")?,
                &names,
                &mut user_filter,
            )?,
            "-t" => terse = true,
            "-n" => numeric_hosts = true,
            "-P" => numeric_ports = true,
            "-a" => and_selection = true,
            "-F" => {
                let list = if iter
                    .peek()
                    .is_some_and(|value| !value.starts_with('-') && !value.starts_with('+'))
                {
                    iter.next().unwrap_or_default()
                } else {
                    String::new()
                };
                fields = Some(OpenFields::parse(&list)?);
            }
            "-i" => networks.push(NetworkFilter::default()),
            "--" => {
                paths.extend(iter);
                break;
            }
            "+D" => recursive_directories.push(iter.next().ok_or("+D requires a directory")?),
            "+d" => directories.push(iter.next().ok_or("+d requires a directory")?),
            "+c" => {
                command_width = iter
                    .next()
                    .ok_or("+c requires a width")?
                    .parse()
                    .map_err(|_| "invalid +c width")?;
            }
            "-h" | "--help" => {
                println!(
                    "Usage: lsof [-nP] [-a] [-p PID] [-c COMMAND] [-u USER] [-i[46][TCP|UDP][:PORT]] [-t] [-F[pcafntuLiDrsoP0]] [+d DIR] [+D DIR] [PATH ...]\n-F also accepts a separate field list; use -F -- before pathname operands.\nInventory: cwd, root, executable and descriptors visible in /proc; memory mappings are not listed."
                );
                return Ok(true);
            }
            value if value.starts_with("-p") && value.len() > 2 => {
                parse_pid_list(&value[2..], &mut pids)?;
            }
            value if value.starts_with("-c") && value.len() > 2 => {
                command_filters.push(value[2..].to_owned());
            }
            value if value.starts_with("-u") && value.len() > 2 => {
                parse_user_list(&value[2..], &names, &mut user_filter)?;
            }
            value if value.starts_with("-F") => {
                fields = Some(OpenFields::parse(&value[2..])?);
            }
            value if value.starts_with("-i") => {
                networks.push(parse_network_filter(&value[2..], &names)?);
            }
            value if value.starts_with("+D") && value.len() > 2 => {
                recursive_directories.push(value[2..].to_owned());
            }
            value if value.starts_with("+d") && value.len() > 2 => {
                directories.push(value[2..].to_owned());
            }
            value if value.starts_with("+c") && value.len() > 2 => {
                command_width = value[2..].parse().map_err(|_| "invalid +c width")?;
            }
            value
                if value.starts_with('-')
                    && value.len() > 2
                    && value[1..]
                        .chars()
                        .all(|flag| matches!(flag, 'n' | 'P' | 'a' | 't')) =>
            {
                for flag in value[1..].chars() {
                    match flag {
                        'n' => numeric_hosts = true,
                        'P' => numeric_ports = true,
                        'a' => and_selection = true,
                        't' => terse = true,
                        _ => unreachable!("validated lsof short option cluster"),
                    }
                }
            }
            value if value.starts_with('-') || value.starts_with('+') => {
                return Err(format!("unsupported option '{value}'"));
            }
            _ => paths.push(value),
        }
    }

    let sockets = read_socket_tables();
    let has_process_filters =
        !pids.is_empty() || !command_filters.is_empty() || !user_filter.is_empty();
    let has_path_filters =
        !paths.is_empty() || !recursive_directories.is_empty() || !directories.is_empty();
    let has_file_filters = has_path_filters || !networks.is_empty();
    let identities = paths
        .iter()
        .filter_map(|path| fs::metadata(path).ok())
        .map(|metadata| (metadata.dev(), metadata.ino()))
        .collect::<BTreeSet<_>>();
    let canonical_directories = |directories: Vec<String>| {
        directories
            .into_iter()
            .filter_map(|path| fs::canonicalize(path).ok())
            .collect::<Vec<_>>()
    };
    let recursive_directories = canonical_directories(recursive_directories);
    let directories = canonical_directories(directories);
    let mut processes = fs::read_dir("/proc")
        .map_err(|error| error.to_string())?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .collect::<Vec<_>>();
    processes.sort_unstable();
    let fields = fields.as_ref();
    let mut emitted_pids = BTreeSet::new();
    let mut table = OpenTable::default();
    for pid in processes {
        let base = Path::new("/proc").join(pid.to_string());
        let mut command = fs::read(base.join("comm")).unwrap_or_default();
        if command.last() == Some(&b'\n') {
            command.pop();
        }
        let uid = process_uid(&base).unwrap_or_default();
        let pid_match = pids.contains(&pid);
        let command_match = command_filters
            .iter()
            .any(|filter| command.starts_with(filter.as_bytes()));
        let user_match = user_filter.contains(&uid);
        let process_selected = if and_selection {
            (pids.is_empty() || pid_match)
                && (command_filters.is_empty() || command_match)
                && (user_filter.is_empty() || user_match)
        } else {
            pid_match || command_match || user_match
        };
        if !process_selected && (and_selection || (has_process_filters && !has_file_filters)) {
            continue;
        }

        let mut emitter = OpenEmitter {
            command,
            command_width,
            pid,
            uid,
            terse,
            fields,
            process_selected,
            and_selection,
            has_process_filters,
            has_file_filters,
            has_path_filters,
            identities: &identities,
            recursive_directories: &recursive_directories,
            directories: &directories,
            networks: &networks,
            sockets: &sockets,
            names: &names,
            numeric_hosts,
            numeric_ports,
            emitted_pids: &mut emitted_pids,
            table: &mut table,
            table_process: None,
        };
        for (fd, path) in [
            ("cwd", base.join("cwd")),
            ("rtd", base.join("root")),
            ("txt", base.join("exe")),
        ] {
            if let Ok(target) = fs::read_link(&path) {
                emitter.emit(Cow::Borrowed(fd), &path, &target);
            }
        }
        if let Ok(entries) = fs::read_dir(base.join("fd")) {
            let mut entries = entries.flatten().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|entry| entry.file_name());
            for entry in entries {
                let Ok(target) = fs::read_link(entry.path()) else {
                    continue;
                };
                let fd = entry.file_name().to_string_lossy().into_owned();
                emitter.emit(Cow::Owned(fd), &entry.path(), &target);
            }
        }
    }
    if !terse && fields.is_none() {
        table.print(&names);
    }
    Ok(!emitted_pids.is_empty())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SocketFamily {
    Inet4,
    Inet6,
    Unix,
}

struct SocketInfo {
    family: SocketFamily,
    protocol: &'static str,
    local: Option<SocketAddr>,
    remote: Option<SocketAddr>,
    state: Option<&'static str>,
    unix_path: Option<String>,
    unix_type: Option<&'static str>,
}

#[derive(Default)]
struct NetworkFilter {
    family: Option<SocketFamily>,
    protocol: Option<&'static str>,
    port: Option<u16>,
}

struct NameDatabase {
    users: HashMap<u32, String>,
    groups: HashMap<u32, String>,
    user_ids: HashMap<String, u32>,
    hosts: HashMap<IpAddr, String>,
    services: HashMap<(u16, String), String>,
    service_ports: HashMap<(String, String), u16>,
}

impl NameDatabase {
    fn load() -> Self {
        let mut database = Self {
            users: HashMap::new(),
            groups: HashMap::new(),
            user_ids: HashMap::new(),
            hosts: HashMap::new(),
            services: HashMap::new(),
            service_ports: HashMap::new(),
        };
        if let Ok(content) = fs::read_to_string("/etc/passwd") {
            for line in content.lines().filter(|line| !line.starts_with('#')) {
                let fields = line.split(':').collect::<Vec<_>>();
                if let Some(uid) = fields.get(2).and_then(|uid| uid.parse().ok()) {
                    let name = fields[0].to_owned();
                    database.users.insert(uid, name.clone());
                    database.user_ids.insert(name, uid);
                }
            }
        }
        if let Ok(content) = fs::read_to_string("/etc/group") {
            for line in content.lines().filter(|line| !line.starts_with('#')) {
                let fields = line.split(':').collect::<Vec<_>>();
                if let Some(gid) = fields.get(2).and_then(|gid| gid.parse().ok()) {
                    database.groups.insert(gid, fields[0].to_owned());
                }
            }
        }
        if let Ok(content) = fs::read_to_string("/etc/hosts") {
            for line in content.lines() {
                let mut fields = line.split_whitespace();
                if let (Some(address), Some(name)) = (fields.next(), fields.next())
                    && !address.starts_with('#')
                    && let Ok(address) = address.parse()
                {
                    database
                        .hosts
                        .entry(address)
                        .or_insert_with(|| name.to_owned());
                }
            }
        }
        if let Ok(content) = fs::read_to_string("/etc/services") {
            for line in content.lines().filter(|line| !line.starts_with('#')) {
                let mut fields = line.split_whitespace();
                let (Some(name), Some(port_protocol)) = (fields.next(), fields.next()) else {
                    continue;
                };
                let Some((port, protocol)) = port_protocol.split_once('/') else {
                    continue;
                };
                let Ok(port) = port.parse() else {
                    continue;
                };
                let protocol = protocol.to_ascii_uppercase();
                database
                    .services
                    .entry((port, protocol.clone()))
                    .or_insert_with(|| name.to_owned());
                database
                    .service_ports
                    .entry((name.to_owned(), protocol))
                    .or_insert(port);
            }
        }
        database
    }

    fn user(&self, uid: u32) -> Cow<'_, str> {
        self.users.get(&uid).map_or_else(
            || Cow::Owned(uid.to_string()),
            |name| Cow::Borrowed(name.as_str()),
        )
    }

    fn group(&self, gid: u32) -> Cow<'_, str> {
        self.groups.get(&gid).map_or_else(
            || Cow::Owned(gid.to_string()),
            |name| Cow::Borrowed(name.as_str()),
        )
    }
}

struct LsofText<'a>(&'a [u8]);

impl std::fmt::Display for LsofText<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::fmt::Write;
        let mut length = 0;
        for &byte in self.0 {
            let escape = match byte {
                b'\x08' => Some("\\b"),
                b'\x0c' => Some("\\f"),
                b'\n' => Some("\\n"),
                b'\r' => Some("\\r"),
                b'\t' => Some("\\t"),
                b'\\' => Some("\\\\"),
                _ => None,
            };
            if let Some(escape) = escape {
                formatter.write_str(escape)?;
                length += escape.len();
            } else if byte < 0x20 {
                formatter.write_char('^')?;
                formatter.write_char(char::from(byte + 0x40))?;
                length += 2;
            } else if byte >= 0x7f {
                write!(formatter, "\\x{byte:02x}")?;
                length += 4;
            } else {
                formatter.write_char(char::from(byte))?;
                length += 1;
            }
        }
        for _ in length..formatter.width().unwrap_or(0) {
            formatter.write_char(' ')?;
        }
        Ok(())
    }
}

struct OpenFields {
    selected: BTreeSet<char>,
    separator: &'static str,
}

impl OpenFields {
    fn parse(value: &str) -> Result<Self, String> {
        let selected = if value.is_empty() || value == "0" {
            "pcafntuLiDsoP".chars().chain(value.chars()).collect()
        } else {
            value.chars().collect::<BTreeSet<_>>()
        };
        if let Some(field) = selected
            .iter()
            .find(|field| !"pcafntuLiDrsoP0".contains(**field))
        {
            return Err(format!("unsupported -F field '{field}'"));
        }
        let separator = if value.contains('0') { "\0" } else { "\n" };
        Ok(Self {
            selected,
            separator,
        })
    }

    fn emit(&self, field: char, value: impl std::fmt::Display) -> bool {
        if field == 'p' || self.selected.contains(&field) {
            print!("{field}{value}{}", self.separator);
            true
        } else {
            false
        }
    }

    fn end_set(&self) {
        if self.separator == "\0" {
            println!();
        }
    }
}

struct OpenProcess {
    command: Vec<u8>,
    pid: u32,
    uid: u32,
}

struct OpenRow {
    process: usize,
    fd: Cow<'static, str>,
    access: char,
    kind: &'static str,
    device: String,
    size_or_offset: String,
    node: Cow<'static, str>,
    name: Vec<u8>,
}

struct OpenTable {
    processes: Vec<OpenProcess>,
    rows: Vec<OpenRow>,
    widths: [usize; 8],
}

impl Default for OpenTable {
    fn default() -> Self {
        Self {
            processes: Vec::new(),
            rows: Vec::new(),
            widths: [7, 3, 4, 2, 4, 6, 8, 4],
        }
    }
}

impl OpenTable {
    fn add_process(
        &mut self,
        mut command: Vec<u8>,
        limit: usize,
        pid: u32,
        uid: u32,
        names: &NameDatabase,
    ) -> usize {
        let (count, width) = ProcText(&command).prefix(limit);
        command.truncate(count);
        self.widths[0] = self.widths[0].max(width);
        self.widths[1] =
            self.widths[1].max(pid.checked_ilog10().map_or(1, |digits| digits as usize + 1));
        let user_width = names.users.get(&uid).map_or_else(
            || uid.checked_ilog10().map_or(1, |digits| digits as usize + 1),
            String::len,
        );
        self.widths[2] = self.widths[2].max(user_width);
        let index = self.processes.len();
        self.processes.push(OpenProcess { command, pid, uid });
        index
    }

    fn add_row(&mut self, row: OpenRow) {
        for (index, width) in [
            (3, row.fd.len() + 2),
            (4, row.kind.len()),
            (5, row.device.len()),
            (6, row.size_or_offset.len()),
            (7, row.node.len()),
        ] {
            self.widths[index] = self.widths[index].max(width);
        }
        self.rows.push(row);
    }

    fn print(&self, names: &NameDatabase) {
        if self.rows.is_empty() {
            return;
        }
        let [command, pid, user, fd, kind, device, size, node] = self.widths;
        let fd = fd.saturating_sub(2);
        println!(
            "{:<command$} {:>pid$} {:>user$} {:>fd$}   {:>kind$} {:>device$} {:>size$} {:>node$} NAME",
            "COMMAND", "PID", "USER", "FD", "TYPE", "DEVICE", "SIZE/OFF", "NODE"
        );
        for row in &self.rows {
            let process = &self.processes[row.process];
            print!(
                "{:<command$} {:>pid$} ",
                ProcText(&process.command),
                process.pid
            );
            if let Some(name) = names.users.get(&process.uid) {
                print!("{name:>user$}");
            } else {
                print!("{:>user$}", process.uid);
            }
            println!(
                " {:>fd$}{}  {:>kind$} {:>device$} {:>size$} {:>node$} {}",
                row.fd,
                row.access,
                row.kind,
                row.device,
                row.size_or_offset,
                row.node,
                LsofText(&row.name)
            );
        }
    }
}

#[derive(Default)]
struct FdInfo {
    access: Option<char>,
    offset: Option<u64>,
}

fn read_fd_info(link: &Path, fd: &str) -> FdInfo {
    if fd.is_empty() || !fd.bytes().all(|byte| byte.is_ascii_digit()) {
        return FdInfo::default();
    }
    let Some(base) = link.parent().and_then(Path::parent) else {
        return FdInfo::default();
    };
    let Ok(content) = fs::read_to_string(base.join("fdinfo").join(fd)) else {
        return FdInfo::default();
    };
    let mut info = FdInfo::default();
    for line in content.lines() {
        if let Some(position) = line.strip_prefix("pos:") {
            info.offset = position.trim().parse().ok();
        } else if let Some(flags) = line.strip_prefix("flags:") {
            info.access = u64::from_str_radix(flags.trim(), 8).ok().and_then(|flags| {
                match flags & libc::O_ACCMODE as u64 {
                    value if value == libc::O_RDONLY as u64 => Some('r'),
                    value if value == libc::O_WRONLY as u64 => Some('w'),
                    value if value == libc::O_RDWR as u64 => Some('u'),
                    _ => None,
                }
            });
        }
    }
    info
}

struct OpenEmitter<'a> {
    command: Vec<u8>,
    command_width: usize,
    pid: u32,
    uid: u32,
    terse: bool,
    fields: Option<&'a OpenFields>,
    process_selected: bool,
    and_selection: bool,
    has_process_filters: bool,
    has_file_filters: bool,
    has_path_filters: bool,
    identities: &'a BTreeSet<(u64, u64)>,
    recursive_directories: &'a [PathBuf],
    directories: &'a [PathBuf],
    networks: &'a [NetworkFilter],
    sockets: &'a HashMap<u64, SocketInfo>,
    names: &'a NameDatabase,
    numeric_hosts: bool,
    numeric_ports: bool,
    emitted_pids: &'a mut BTreeSet<u32>,
    table: &'a mut OpenTable,
    table_process: Option<usize>,
}

impl OpenEmitter<'_> {
    fn emit(&mut self, fd: Cow<'static, str>, link: &Path, target: &Path) {
        let metadata = if self.has_path_filters {
            fs::metadata(link).ok()
        } else {
            None
        };
        let socket_inode = procfs::socket_inode(target.as_os_str().as_bytes());
        let socket = socket_inode.and_then(|inode| self.sockets.get(&inode));
        let path_match = metadata
            .as_ref()
            .is_some_and(|metadata| self.identities.contains(&(metadata.dev(), metadata.ino())))
            || self
                .recursive_directories
                .iter()
                .any(|directory| path_is_below(target, directory, true))
            || self
                .directories
                .iter()
                .any(|directory| path_is_below(target, directory, false));
        let network_match = socket.is_some_and(|socket| {
            self.networks
                .iter()
                .any(|filter| socket_matches(socket, filter))
        });
        let file_selected = if self.and_selection {
            (!self.has_path_filters || path_match) && (self.networks.is_empty() || network_match)
        } else {
            path_match || network_match
        };
        let selected = if self.and_selection {
            self.process_selected && file_selected
        } else {
            (!self.has_process_filters && !self.has_file_filters)
                || (self.has_process_filters && self.process_selected)
                || (self.has_file_filters && file_selected)
        };
        if !selected {
            return;
        }
        let first_process_row = self.emitted_pids.insert(self.pid);
        if self.terse {
            if first_process_row {
                println!("{}", self.pid);
            }
            return;
        }

        let metadata = if self.has_path_filters {
            metadata
        } else {
            fs::metadata(link).ok()
        };
        let inet_socket = socket.is_some_and(|socket| socket.family != SocketFamily::Unix);
        let is_socket = socket_inode.is_some()
            || metadata
                .as_ref()
                .is_some_and(|value| value.file_type().is_socket());
        let kind = if let Some(socket) = socket {
            match socket.family {
                SocketFamily::Inet4 => "IPv4",
                SocketFamily::Inet6 => "IPv6",
                SocketFamily::Unix => "unix",
            }
        } else if let Some(metadata) = &metadata {
            let kind = metadata.file_type();
            if kind.is_dir() {
                "DIR"
            } else if kind.is_char_device() {
                "CHR"
            } else if kind.is_block_device() {
                "BLK"
            } else if kind.is_fifo() {
                "FIFO"
            } else if kind.is_socket() {
                "sock"
            } else if kind.is_file() {
                "REG"
            } else {
                "unknown"
            }
        } else if socket_inode.is_some() {
            "sock"
        } else {
            "unknown"
        };
        let name = if let Some(socket) = socket {
            Cow::Owned(
                format_socket(
                    socket,
                    self.names,
                    self.numeric_hosts,
                    self.numeric_ports,
                    self.fields.is_some(),
                )
                .into_bytes(),
            )
        } else if target.as_os_str().as_bytes().starts_with(b"pipe:[") {
            Cow::Borrowed(b"pipe".as_slice())
        } else {
            Cow::Borrowed(target.as_os_str().as_bytes())
        };
        let inode = if inet_socket {
            None
        } else {
            metadata.as_ref().map(MetadataExt::ino).or(socket_inode)
        };
        let raw_device = metadata
            .as_ref()
            .filter(|metadata| {
                metadata.file_type().is_char_device() || metadata.file_type().is_block_device()
            })
            .map(MetadataExt::rdev);
        let file_size = metadata
            .as_ref()
            .filter(|metadata| metadata.is_file() || metadata.is_dir())
            .map(fs::Metadata::len);
        let info = if self
            .fields
            .is_none_or(|fields| fields.selected.contains(&'a') || fields.selected.contains(&'o'))
        {
            read_fd_info(link, &fd)
        } else {
            FdInfo::default()
        };
        if let Some(fields) = self.fields {
            if first_process_row {
                fields.emit('p', self.pid);
                fields.emit('c', ProcText(&self.command));
                fields.emit('u', self.uid);
                if let Some(user) = self.names.users.get(&self.uid) {
                    fields.emit('L', LsofText(user.as_bytes()));
                }
                fields.end_set();
            }
            let mut emitted = fields.emit('f', fd.as_ref());
            if let Some(access) = info.access {
                emitted |= fields.emit('a', access);
            }
            emitted |= fields.emit('t', kind);
            if !is_socket && let Some(metadata) = &metadata {
                emitted |= fields.emit('D', format_args!("0x{:x}", metadata.dev()));
            }
            if let Some(device) = raw_device {
                emitted |= fields.emit('r', format_args!("0x{device:x}"));
            }
            if let Some(size) = file_size {
                emitted |= fields.emit('s', size);
            }
            if let Some(offset) = info.offset {
                emitted |= fields.emit('o', format_args!("0t{offset}"));
            }
            if let Some(inode) = inode {
                emitted |= fields.emit('i', inode);
            }
            if inet_socket && let Some(socket) = socket {
                emitted |= fields.emit('P', socket.protocol);
            }
            emitted |= fields.emit('n', LsofText(&name));
            if emitted {
                fields.end_set();
            }
            return;
        }
        let device = if inet_socket {
            socket_inode
                .map(|inode| inode.to_string())
                .unwrap_or_default()
        } else if is_socket {
            String::new()
        } else {
            raw_device
                .or_else(|| metadata.as_ref().map(MetadataExt::dev))
                .map(|device| format!("{},{}", libc::major(device), libc::minor(device)))
                .unwrap_or_default()
        };
        let size_or_offset = if let Some(size) = file_size {
            size.to_string()
        } else {
            info.offset
                .map(|offset| format!("0t{offset}"))
                .unwrap_or_default()
        };
        let node = if inet_socket {
            Cow::Borrowed(socket.map(|socket| socket.protocol).unwrap_or(""))
        } else {
            inode
                .map(|inode| Cow::Owned(inode.to_string()))
                .unwrap_or(Cow::Borrowed(""))
        };
        let process = match self.table_process {
            Some(index) => index,
            None => {
                let index = self.table.add_process(
                    std::mem::take(&mut self.command),
                    self.command_width,
                    self.pid,
                    self.uid,
                    self.names,
                );
                self.table_process = Some(index);
                index
            }
        };
        self.table.add_row(OpenRow {
            process,
            fd,
            access: info.access.unwrap_or(' '),
            kind,
            device,
            size_or_offset,
            node,
            name: name.into_owned(),
        });
    }
}

fn parse_pid_list(value: &str, output: &mut BTreeSet<u32>) -> Result<(), String> {
    for pid in value.split(',') {
        output.insert(
            pid.parse::<u32>()
                .map_err(|_| format!("invalid PID '{pid}'"))?,
        );
    }
    Ok(())
}

fn parse_user_list(
    value: &str,
    names: &NameDatabase,
    output: &mut BTreeSet<u32>,
) -> Result<(), String> {
    for user in value.split(',') {
        let uid = user
            .parse::<u32>()
            .ok()
            .or_else(|| names.user_ids.get(user).copied())
            .ok_or_else(|| format!("unknown user '{user}'"))?;
        output.insert(uid);
    }
    Ok(())
}

fn parse_network_filter(value: &str, names: &NameDatabase) -> Result<NetworkFilter, String> {
    let mut value = value;
    let mut filter = NetworkFilter::default();
    if let Some(rest) = value.strip_prefix('4') {
        filter.family = Some(SocketFamily::Inet4);
        value = rest;
    } else if let Some(rest) = value.strip_prefix('6') {
        filter.family = Some(SocketFamily::Inet6);
        value = rest;
    }
    let upper = value.to_ascii_uppercase();
    if upper.starts_with("TCP") {
        filter.protocol = Some("TCP");
        value = &value[3..];
    } else if upper.starts_with("UDP") {
        filter.protocol = Some("UDP");
        value = &value[3..];
    }
    if value.is_empty() {
        return Ok(filter);
    }
    let port = value
        .strip_prefix(':')
        .ok_or_else(|| format!("unsupported -i selector '{value}'"))?;
    let protocol = filter.protocol.unwrap_or("TCP");
    filter.port = Some(
        port.parse::<u16>()
            .ok()
            .or_else(|| {
                names
                    .service_ports
                    .get(&(port.to_owned(), protocol.to_owned()))
                    .copied()
            })
            .ok_or_else(|| format!("unknown port or service '{port}'"))?,
    );
    Ok(filter)
}

fn read_socket_tables() -> HashMap<u64, SocketInfo> {
    let mut sockets = HashMap::new();
    for (path, family, protocol) in [
        ("/proc/net/tcp", SocketFamily::Inet4, "TCP"),
        ("/proc/net/tcp6", SocketFamily::Inet6, "TCP"),
        ("/proc/net/udp", SocketFamily::Inet4, "UDP"),
        ("/proc/net/udp6", SocketFamily::Inet6, "UDP"),
    ] {
        let Ok(content) = fs::read(path) else {
            continue;
        };
        for line in content.split(|byte| *byte == b'\n').skip(1) {
            let Ok(socket) = procfs::parse_inet_socket(line, family == SocketFamily::Inet6) else {
                continue;
            };
            sockets.insert(
                socket.inode,
                SocketInfo {
                    family,
                    protocol,
                    local: Some(socket.local),
                    remote: Some(socket.remote),
                    state: if protocol == "TCP" {
                        procfs::tcp_state(socket.state)
                    } else {
                        None
                    },
                    unix_path: None,
                    unix_type: None,
                },
            );
        }
    }
    if let Ok(content) = fs::read("/proc/net/unix") {
        for socket in content
            .split(|byte| *byte == b'\n')
            .skip(1)
            .filter_map(procfs::parse_unix_socket)
        {
            sockets.insert(
                socket.inode,
                SocketInfo {
                    family: SocketFamily::Unix,
                    protocol: "UNIX",
                    local: None,
                    remote: None,
                    state: None,
                    unix_path: socket
                        .path
                        .map(|path| String::from_utf8_lossy(path).into_owned()),
                    unix_type: match socket.socket_type {
                        1 => Some("STREAM"),
                        2 => Some("DGRAM"),
                        5 => Some("SEQPACKET"),
                        _ => None,
                    },
                },
            );
        }
    }
    sockets
}

fn socket_matches(socket: &SocketInfo, filter: &NetworkFilter) -> bool {
    if socket.family == SocketFamily::Unix {
        return false;
    }
    if filter.family.is_some_and(|family| family != socket.family)
        || filter
            .protocol
            .is_some_and(|protocol| protocol != socket.protocol)
    {
        return false;
    }
    filter.port.is_none_or(|port| {
        socket.local.is_some_and(|address| address.port() == port)
            || socket.remote.is_some_and(|address| address.port() == port)
    })
}

fn path_is_below(target: &Path, directory: &Path, recursive: bool) -> bool {
    if target == directory {
        return true;
    }
    if recursive {
        target.starts_with(directory)
    } else {
        target.parent() == Some(directory)
    }
}

fn format_socket(
    socket: &SocketInfo,
    names: &NameDatabase,
    numeric_hosts: bool,
    numeric_ports: bool,
    field_output: bool,
) -> String {
    if socket.family == SocketFamily::Unix {
        return socket.unix_path.clone().unwrap_or_else(|| {
            socket
                .unix_type
                .map(|kind| format!("type={kind}"))
                .unwrap_or_default()
        });
    }
    let local = socket
        .local
        .map(|address| {
            format_endpoint(
                address,
                socket.protocol,
                names,
                numeric_hosts,
                numeric_ports,
            )
        })
        .unwrap_or_else(|| "*:*".into());
    let remote = socket
        .remote
        .filter(|address| !address.ip().is_unspecified() || address.port() != 0)
        .map(|address| {
            format_endpoint(
                address,
                socket.protocol,
                names,
                numeric_hosts,
                numeric_ports,
            )
        });
    let mut output = local;
    if let Some(remote) = remote {
        output.push_str("->");
        output.push_str(&remote);
    }
    if !field_output && let Some(state) = socket.state {
        output.push_str(" (");
        output.push_str(&state.to_ascii_uppercase());
        output.push(')');
    }
    output
}

fn format_endpoint(
    address: SocketAddr,
    protocol: &str,
    names: &NameDatabase,
    numeric_hosts: bool,
    numeric_ports: bool,
) -> String {
    let host = if address.ip().is_unspecified() {
        "*".to_owned()
    } else if !numeric_hosts {
        names
            .hosts
            .get(&address.ip())
            .cloned()
            .unwrap_or_else(|| address.ip().to_string())
    } else {
        address.ip().to_string()
    };
    let service = if !numeric_ports {
        names
            .services
            .get(&(address.port(), protocol.to_owned()))
            .cloned()
            .unwrap_or_else(|| address.port().to_string())
    } else {
        address.port().to_string()
    };
    if address.is_ipv6() {
        format!("[{host}]:{service}")
    } else {
        format!("{host}:{service}")
    }
}

fn process_uid(base: &Path) -> Option<u32> {
    procfs::status_uid(&fs::read(base.join("status")).ok()?)
}

fn truncate(value: &str, length: usize) -> &str {
    value.get(..length).unwrap_or(value)
}

pub fn lspci(args: Vec<OsString>) -> i32 {
    report("lspci", run_lspci(args))
}

fn run_lspci(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut numeric = 0u8;
    let mut show_kernel = false;
    let mut verbose = false;
    let mut show_domain = false;
    let mut slot_filter = None;
    let mut ids_path = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-n" => numeric = numeric.saturating_add(1),
            "-nn" => numeric = 2,
            "-D" => show_domain = true,
            "-k" => show_kernel = true,
            "-v" | "-vv" | "-vvv" => verbose = true,
            "-s" => {
                slot_filter = Some(PciSelector::parse(
                    &iter.next().ok_or("-s requires a slot")?,
                )?)
            }
            "-i" => ids_path = Some(iter.next().ok_or("-i requires a PCI ID database")?),
            "-h" | "--help" => {
                println!(
                    "Usage: lspci [-n|-nn] [-D] [-k] [-v] [-s [[DOMAIN:]BUS:]DEVICE[.FUNCTION]] [-i PCI.IDS]\nInventory uses /sys/bus/pci/devices; -v shows subsystem and IRQ only. Machine output (-mm) and proc/config fallback are unsupported."
                );
                return Ok(());
            }
            value if value.starts_with("-s") && value.len() > 2 => {
                slot_filter = Some(PciSelector::parse(&value[2..])?);
            }
            value if value.starts_with('-') => return Err(format!("unsupported option '{value}'")),
            _ => return Err(format!("unexpected operand '{value}'")),
        }
    }
    let names = if numeric == 1 {
        PciNames::default()
    } else {
        PciNames::load(ids_path.as_deref())?
    };
    let mut entries = read_dir_optional(Path::new("/sys/bus/pci/devices"))?
        .into_iter()
        .filter_map(|entry| {
            let selector = PciSelector::parse(entry.file_name().to_str()?).ok()?;
            Some((entry, selector))
        })
        .collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(entry, _)| entry.file_name());
    show_domain |= entries
        .iter()
        .any(|(_, slot)| slot.domain.is_some_and(|domain| domain != 0));
    for (entry, address) in entries {
        if slot_filter
            .as_ref()
            .is_some_and(|filter| !filter.matches(&address))
        {
            continue;
        }
        let full = entry.file_name().to_string_lossy().into_owned();
        let slot = if show_domain {
            full.as_str()
        } else {
            full.strip_prefix("0000:").unwrap_or(&full)
        };
        let hex = |name| {
            let value = sys_hex(&entry.path().join(name));
            u32::from_str_radix(&value, 16).map_err(|_| format!("{full}: invalid PCI {name}"))
        };
        let vendor = u16::try_from(hex("vendor")?).map_err(|_| "invalid PCI vendor ID")?;
        let device = u16::try_from(hex("device")?).map_err(|_| "invalid PCI device ID")?;
        let class = (hex("class")? >> 8) as u16;
        let revision = hex("revision")?;
        if numeric == 1 {
            print!("{slot} {class:04x}: {vendor:04x}:{device:04x}");
        } else {
            let class_name = names.subclasses.get(&class);
            let device_name = names.devices.get(&(vendor, device));
            let vendor_name = names.vendors.get(&vendor);
            print!("{slot} ");
            match (class_name, numeric >= 2) {
                (Some(name), true) => print!("{name} [{class:04x}]"),
                (Some(name), false) => print!("{name}"),
                (None, true) => print!("Class [{class:04x}]"),
                (None, false) => print!("Class {class:04x}"),
            }
            print!(": ");
            if let Some(name) = vendor_name {
                print!("{name} ");
            }
            if let Some(name) = device_name {
                print!("{name}");
            } else if numeric >= 2 {
                print!("Device");
            } else if vendor_name.is_some() {
                print!("Device {device:04x}");
            } else {
                print!("Device {vendor:04x}:{device:04x}");
            }
            if numeric >= 2 {
                print!(" [{vendor:04x}:{device:04x}]");
            }
        }
        if revision != 0 {
            print!(" (rev {revision:02x})");
        }
        println!();
        if verbose || show_kernel {
            let vendor = sys_hex(&entry.path().join("subsystem_vendor"));
            let device = sys_hex(&entry.path().join("subsystem_device"));
            if !vendor.is_empty() && !device.is_empty() {
                println!("\tSubsystem: {vendor}:{device}");
            }
        }
        if verbose && let Ok(irq) = fs::read_to_string(entry.path().join("irq")) {
            println!("\tFlags: IRQ {}", irq.trim());
        }
        if show_kernel && let Ok(driver) = fs::read_link(entry.path().join("driver")) {
            println!(
                "\tKernel driver in use: {}",
                driver.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        if verbose {
            println!();
        }
    }
    Ok(())
}

#[derive(Default)]
struct PciSelector {
    domain: Option<u32>,
    bus: Option<u32>,
    device: Option<u32>,
    function: Option<u32>,
}

impl PciSelector {
    fn parse(value: &str) -> Result<Self, String> {
        let invalid = || format!("invalid PCI slot '{value}'");
        let parse = |field: &str, maximum| {
            if field.is_empty() || field == "*" {
                Ok(None)
            } else {
                u32::from_str_radix(field, 16)
                    .ok()
                    .filter(|number| *number <= maximum)
                    .map(Some)
                    .ok_or_else(invalid)
            }
        };
        let (address, function) = value
            .split_once('.')
            .map_or((value, None), |(address, function)| {
                (address, Some(function))
            });
        let fields = address.split(':').collect::<Vec<_>>();
        let (domain, bus, device) = match fields.as_slice() {
            [device] => (None, None, *device),
            [bus, device] => (None, parse(bus, 0xff)?, *device),
            [domain, bus, device] => (parse(domain, 0x7fff_ffff)?, parse(bus, 0xff)?, *device),
            _ => return Err(invalid()),
        };
        Ok(Self {
            domain,
            bus,
            device: parse(device, 0x1f)?,
            function: function.map(|value| parse(value, 7)).transpose()?.flatten(),
        })
    }

    fn matches(&self, address: &Self) -> bool {
        [
            (self.domain, address.domain),
            (self.bus, address.bus),
            (self.device, address.device),
            (self.function, address.function),
        ]
        .into_iter()
        .all(|(filter, actual)| filter.is_none() || filter == actual)
    }
}

#[derive(Default)]
struct PciNames {
    vendors: HashMap<u16, String>,
    devices: HashMap<(u16, u16), String>,
    subclasses: HashMap<u16, String>,
}

impl PciNames {
    fn load(path: Option<&str>) -> Result<Self, String> {
        let content = if let Some(path) = path {
            fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?
        } else {
            [
                "/usr/share/hwdata/pci.ids",
                "/usr/share/misc/pci.ids",
                "/usr/share/pci.ids",
            ]
            .into_iter()
            .find_map(|path| fs::read_to_string(path).ok())
            .unwrap_or_default()
        };
        let mut names = Self::default();
        let mut vendor = None;
        let mut class = None;
        for line in content
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            if line.starts_with("\t\t") {
                continue;
            }
            if let Some(line) = line.strip_prefix("C ") {
                class = line
                    .split_once(char::is_whitespace)
                    .and_then(|(id, _)| u8::from_str_radix(id, 16).ok());
                vendor = None;
            } else if let Some(line) = line.strip_prefix('\t') {
                let Some((id, name)) = line.split_once(char::is_whitespace) else {
                    continue;
                };
                let Ok(id) = u16::from_str_radix(id, 16) else {
                    continue;
                };
                if let Some(class) = class {
                    if id <= 0xff {
                        names
                            .subclasses
                            .insert((u16::from(class) << 8) | id, name.trim().into());
                    }
                } else if let Some(vendor) = vendor {
                    names.devices.insert((vendor, id), name.trim().into());
                }
            } else {
                class = None;
                let Some((id, name)) = line.split_once(char::is_whitespace) else {
                    continue;
                };
                vendor = u16::from_str_radix(id, 16).ok();
                if let Some(vendor) = vendor {
                    names.vendors.insert(vendor, name.trim().into());
                }
            }
        }
        Ok(names)
    }
}

pub fn lsusb(args: Vec<OsString>) -> i32 {
    report("lsusb", run_lsusb(args))
}

fn run_lsusb(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut verbose = false;
    let mut tree = false;
    let mut filter = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-v" | "--verbose" => verbose = true,
            "-t" | "--tree" => tree = true,
            "-d" => filter = Some(iter.next().ok_or("-d requires vendor:product")?),
            "-h" | "--help" => {
                println!("Usage: lsusb [-v] [-t] [-d VENDOR:PRODUCT]");
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unsupported option '{value}'")),
            _ => return Err(format!("unexpected operand '{value}'")),
        }
    }
    let mut devices = read_dir_optional(Path::new("/sys/bus/usb/devices"))?
        .into_iter()
        .filter(|entry| entry.path().join("idVendor").exists())
        .collect::<Vec<_>>();
    devices.sort_unstable_by_key(|entry| entry.file_name());
    for entry in devices {
        let path = entry.path();
        let vendor = sys_text(&path.join("idVendor"));
        let product = sys_text(&path.join("idProduct"));
        let id = format!("{vendor}:{product}");
        if filter.as_ref().is_some_and(|filter| filter != &id) {
            continue;
        }
        let bus = sys_text(&path.join("busnum"))
            .parse::<u16>()
            .unwrap_or_default();
        let device = sys_text(&path.join("devnum"))
            .parse::<u16>()
            .unwrap_or_default();
        let manufacturer = sys_text(&path.join("manufacturer"));
        let description = sys_text(&path.join("product"));
        if tree {
            println!(
                "/ {:03}/{:03} {id} {} {}",
                bus, device, manufacturer, description
            );
        } else {
            println!(
                "Bus {bus:03} Device {device:03}: ID {id} {} {}",
                manufacturer, description
            );
        }
        if verbose {
            println!("  bDeviceClass {}", sys_text(&path.join("bDeviceClass")));
            println!("  bcdUSB {}", sys_text(&path.join("version")));
            let serial = sys_text(&path.join("serial"));
            if !serial.is_empty() {
                println!("  iSerial {serial}");
            }
        }
    }
    Ok(())
}

pub fn lsscsi(args: Vec<OsString>) -> i32 {
    report("lsscsi", run_lsscsi(args))
}

fn run_lsscsi(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut generic = false;
    let mut size = 0u8;
    let mut kernel_names = false;
    let mut verbose = false;
    for value in values.into_iter().skip(1) {
        let flags = match value.as_str() {
            "--generic" => "g",
            "--size" => "s",
            "--kname" => "k",
            "--verbose" => "v",
            "-h" | "--help" => {
                println!(
                    "Usage: lsscsi [-g] [-s|-ss|-sss|-ssss] [-k] [-v]\nInventory uses /sys/class/scsi_device. Default names are matching device nodes in /dev, or '-'; -k explicitly uses kernel names.\nHost mode (-H), address filters, NVMe inventory and transport modes are unsupported."
                );
                return Ok(());
            }
            value if value.starts_with('-') => &value[1..],
            _ => return Err(format!("unexpected operand '{value}'")),
        };
        if flags.is_empty() {
            return Err(format!("unsupported option '{value}'"));
        }
        for flag in flags.chars() {
            match flag {
                'g' => generic = true,
                's' => size = size.saturating_add(1),
                'k' => kernel_names = true,
                'v' => verbose = true,
                _ => return Err(format!("unsupported option '{value}'")),
            }
        }
    }
    let first_child = |path: &Path| -> Result<Option<PathBuf>, String> {
        let mut entries = read_dir_optional(path)?;
        entries.sort_unstable_by_key(|entry| entry.file_name());
        Ok(entries.first().map(|entry| entry.path()))
    };
    let mut entries = read_dir_optional(Path::new("/sys/class/scsi_device"))?;
    entries.sort_unstable_by_key(|entry| entry.file_name());
    let nodes = if kernel_names || entries.is_empty() {
        ScsiNodes {
            nodes: HashMap::new(),
        }
    } else {
        ScsiNodes::read()?
    };
    for entry in entries {
        let address = format!("[{}]", entry.file_name().to_string_lossy());
        let device = entry.path().join("device");
        let kind = scsi_type(&sys_text(&device.join("type")));
        let vendor = sys_text(&device.join("vendor"));
        let model = sys_text(&device.join("model"));
        let revision = sys_text(&device.join("rev"));
        let block = first_child(&device.join("block"))?;
        let tape = if block.is_none() {
            first_child(&device.join("scsi_tape"))?
        } else {
            None
        };
        let primary = block.as_deref().or(tape.as_deref());
        let node = nodes.lookup(primary, block.is_some(), kernel_names);
        if address.len() >= 13 {
            print!("{address} ");
        } else {
            print!("{address:<13}");
        }
        print!("{kind:<8}{vendor:<8} {model:<16} {revision:<4}  {node:<9}");
        if generic {
            let sg = first_child(&device.join("scsi_generic"))?;
            print!("  {:<9}", nodes.lookup(sg.as_deref(), false, kernel_names));
        }
        if size != 0 {
            let sectors = block
                .as_ref()
                .and_then(|block| sys_text(&block.join("size")).parse::<u64>().ok());
            if let Some(sectors) = sectors.filter(|sectors| *sectors != 0) {
                if size <= 2 {
                    print!("  {:>6}", human_size(u128::from(sectors) * 512, size == 2));
                } else {
                    let block = block.as_ref().expect("size came from a block device");
                    let block_size = sys_text(&block.join("queue/logical_block_size"))
                        .parse::<u64>()
                        .ok()
                        .filter(|value| *value > 0)
                        .unwrap_or(512);
                    let blocks = u128::from(sectors) * 512 / u128::from(block_size);
                    print!("  {blocks:>12}");
                    if size > 3 {
                        print!(",{block_size}");
                    }
                }
            } else {
                print!("  {:>6}", "-");
            }
        }
        println!();
        if verbose {
            let resolved = fs::canonicalize(&device).ok();
            println!(
                "  dir: {}  [{}]",
                device.display(),
                resolved
                    .as_deref()
                    .map_or(Cow::Borrowed("?"), Path::to_string_lossy)
            );
        }
    }
    Ok(())
}

struct ScsiNodes {
    nodes: HashMap<(bool, u64), (i64, i64, PathBuf)>,
}

impl ScsiNodes {
    fn read() -> Result<Self, String> {
        let mut nodes = HashMap::new();
        for entry in read_dir_optional(Path::new("/dev"))? {
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            let kind = metadata.file_type();
            if !kind.is_block_device() && !kind.is_char_device() {
                continue;
            }
            let key = (kind.is_block_device(), metadata.rdev());
            let timestamp = (metadata.mtime(), metadata.mtime_nsec());
            let node = nodes
                .entry(key)
                .or_insert_with(|| (timestamp.0, timestamp.1, entry.path()));
            if timestamp > (node.0, node.1) {
                *node = (timestamp.0, timestamp.1, entry.path());
            }
        }
        Ok(Self { nodes })
    }

    fn lookup(&self, path: Option<&Path>, block: bool, kernel_names: bool) -> Cow<'_, str> {
        let Some(path) = path else {
            return Cow::Borrowed("-");
        };
        if kernel_names {
            return Cow::Owned(format!(
                "/dev/{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        let device = sys_text(&path.join("dev"));
        let Some((major, minor)) = device.split_once(':') else {
            return Cow::Borrowed("-");
        };
        let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
            return Cow::Borrowed("-");
        };
        self.nodes
            .get(&(block, libc::makedev(major, minor)))
            .map_or(Cow::Borrowed("-"), |(_, _, path)| path.to_string_lossy())
    }
}

fn scsi_type(value: &str) -> &'static str {
    match value {
        "0" => "disk",
        "1" => "tape",
        "5" => "cd/dvd",
        "7" => "optical",
        "13" => "enclosu",
        "14" => "rbc",
        _ => "unknown",
    }
}

fn human_size(mut bytes: u128, binary: bool) -> String {
    let units = if binary {
        ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"]
    } else {
        ["B", "kB", "MB", "GB", "TB", "PB", "EB"]
    };
    let divisor = if binary { 1024 } else { 1000 };
    let mut unit = 0;
    let mut remainder = 0;
    while bytes >= divisor && unit + 1 < units.len() {
        remainder = bytes % divisor;
        bytes /= divisor;
        unit += 1;
    }
    let digits = if unit == 0 || bytes >= 100 {
        0
    } else if bytes >= 10 {
        1
    } else {
        2
    };
    if digits == 0 {
        format!("{bytes}{}", units[unit])
    } else {
        let scale = 10u128.pow(digits);
        let fraction = remainder * scale / divisor;
        format!(
            "{bytes}.{fraction:0width$}{}",
            units[unit],
            width = digits as usize
        )
    }
}

pub fn modinfo(args: Vec<OsString>) -> i32 {
    report("modinfo", run_modinfo(args))
}

fn run_modinfo(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut field = None;
    let mut null = false;
    let mut modules = Vec::new();
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-F" | "--field" => field = Some(iter.next().ok_or("-F requires a field")?),
            "-l" | "--license" => field = Some("license".into()),
            "-a" | "--author" => field = Some("author".into()),
            "-d" | "--description" => field = Some("description".into()),
            "-p" | "--parameters" => field = Some("parm".into()),
            "-n" | "--filename" => field = Some("filename".into()),
            "-0" | "--null" => null = true,
            "-h" | "--help" => {
                println!("Usage: modinfo [-F FIELD|-l|-a|-d|-p|-n] [-0] MODULE ...");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("kmod version axe");
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unsupported option '{value}'")),
            _ => modules.push(value),
        }
    }
    if modules.is_empty() {
        return Err("missing module name".into());
    }
    for module in modules {
        let path = if Path::new(&module).exists() {
            PathBuf::from(&module)
        } else {
            find_module(&module)?
        };
        let bytes = read_module(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let object = object::File::parse(&*bytes).map_err(|error| error.to_string())?;
        let section = object
            .section_by_name(".modinfo")
            .ok_or_else(|| format!("{}: no .modinfo section", path.display()))?;
        let data = section.data().map_err(|error| error.to_string())?;
        let fields = data
            .split(|byte| *byte == 0)
            .filter_map(|entry| std::str::from_utf8(entry).ok())
            .filter_map(|entry| entry.split_once('='));
        let separator = if null { "\0" } else { "\n" };
        match field.as_deref() {
            Some("filename") => print!("{}{separator}", path.display()),
            None => print!("filename:       {}{separator}", path.display()),
            Some(_) => {}
        }
        let mut parameters: Vec<ModuleParameter<'_>> = Vec::new();
        for (name, value) in fields {
            if matches!(name, "parm" | "parmtype")
                && (field.is_none() || field.as_deref() == Some("parm"))
            {
                let Some((parameter, value)) = value.split_once(':') else {
                    return Err(format!("invalid {name} field: missing ':'"));
                };
                let index = parameters
                    .iter()
                    .position(|entry| entry.name == parameter)
                    .unwrap_or_else(|| {
                        parameters.push(ModuleParameter {
                            name: parameter,
                            description: None,
                            kind: None,
                        });
                        parameters.len() - 1
                    });
                if name == "parm" {
                    parameters[index].description = Some(value);
                } else {
                    parameters[index].kind = Some(value);
                }
                continue;
            }
            if field.as_deref().is_some_and(|field| field != name) {
                continue;
            }
            if field.is_some() {
                print!("{value}{separator}");
            } else if null {
                print!("{name}={value}{separator}");
            } else {
                print!(
                    "{name}:{:width$}{value}{separator}",
                    "",
                    width = 15usize.saturating_sub(name.len())
                );
            }
        }
        for parameter in parameters.iter().rev() {
            if field.is_none() {
                print!("parm:           ");
            }
            print!("{}:", parameter.name);
            if let Some(description) = parameter.description {
                print!("{description}");
            }
            if let Some(kind) = parameter.kind {
                if parameter.description.is_some() || field.is_some() {
                    print!(" ({kind})");
                } else {
                    print!("{kind}");
                }
            }
            print!("{separator}");
        }
    }
    Ok(())
}

struct ModuleParameter<'a> {
    name: &'a str,
    description: Option<&'a str>,
    kind: Option<&'a str>,
}

fn find_module(name: &str) -> Result<PathBuf, String> {
    let release =
        fs::read_to_string("/proc/sys/kernel/osrelease").map_err(|error| error.to_string())?;
    let root = Path::new("/lib/modules").join(release.trim());
    let normalized = name.replace('-', "_");
    let mut stack = vec![root];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let filename = entry.file_name().to_string_lossy().into_owned();
            let stem = filename
                .split(".ko")
                .next()
                .unwrap_or(&filename)
                .replace('-', "_");
            if stem == normalized {
                return Ok(path);
            }
        }
    }
    Err(format!("module {name} not found"))
}

fn read_module(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    match path.extension().and_then(|value| value.to_str()) {
        Some("xz") => {
            XzReader::new(file, true).read_to_end(&mut bytes)?;
        }
        Some("zst") => {
            zstd::stream::read::Decoder::new(file)?.read_to_end(&mut bytes)?;
        }
        Some("gz") => {
            GzDecoder::new(file).read_to_end(&mut bytes)?;
        }
        _ => {
            let mut file = file;
            file.read_to_end(&mut bytes)?;
        }
    };
    Ok(bytes)
}

fn sys_hex(path: &Path) -> String {
    sys_text(path).trim_start_matches("0x").to_string()
}

fn sys_text(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn read_dir_optional(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(entries.flatten().collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_deltas_saturate_after_counter_reset() {
        assert_eq!(delta_array([1; 8], Some([2; 8])), [0; 8]);
    }

    #[test]
    fn human_sizes_use_decimal_units_like_lsscsi() {
        assert_eq!(human_size(1_000_000_000, false), "1.00GB");
        assert_eq!(human_size(1_048_576, false), "1.04MB");
        assert_eq!(human_size(1_048_576, true), "1.00MiB");
    }
}
