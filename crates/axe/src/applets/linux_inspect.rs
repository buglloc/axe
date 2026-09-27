use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use flate2::read::GzDecoder;
use lzma_rust2::XzReader;
use object::{Object, ObjectSection};
use vzik::procfs;

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
        println!(
            "{:<19} {:>8}  {} {}",
            String::from_utf8_lossy(module.name),
            String::from_utf8_lossy(module.size),
            String::from_utf8_lossy(module.instances),
            String::from_utf8_lossy(module.dependencies)
        );
    }
    Ok(())
}

pub fn ipcs(args: Vec<OsString>) -> i32 {
    report("ipcs", run_ipcs(args))
}

fn run_ipcs(args: Vec<OsString>) -> Result<(), String> {
    #[derive(Clone, Copy)]
    enum ReportKind {
        Default,
        Times,
        Pids,
        Creators,
        Limits,
        Summary,
    }

    let values = utf8_args(args)?;
    let mut shm = false;
    let mut sem = false;
    let mut msg = false;
    let mut report = ReportKind::Default;
    let mut id = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-m" | "--shmems" => shm = true,
            "-s" | "--semaphores" => sem = true,
            "-q" | "--queues" => msg = true,
            "-a" | "--all" => {
                shm = true;
                sem = true;
                msg = true;
            }
            "-i" | "--id" => {
                id = Some(
                    iter.next()
                        .ok_or("-i requires an ID")?
                        .parse::<i64>()
                        .map_err(|_| "invalid IPC ID")?,
                );
            }
            "-t" | "--time" => report = ReportKind::Times,
            "-p" | "--pid" => report = ReportKind::Pids,
            "-c" | "--creator" => report = ReportKind::Creators,
            "-l" | "--limits" => report = ReportKind::Limits,
            "-u" | "--summary" => report = ReportKind::Summary,
            "-h" | "--help" => {
                println!("Usage: ipcs [-m|-q|-s] [-a] [-t|-p|-c|-l|-u] [-i ID]");
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ => return Err(format!("unexpected operand '{value}'")),
        }
    }
    if !shm && !sem && !msg {
        shm = true;
        sem = true;
        msg = true;
    }

    let selections = [
        (msg, "Message Queues", "/proc/sysvipc/msg", "msqid"),
        (shm, "Shared Memory Segments", "/proc/sysvipc/shm", "shmid"),
        (sem, "Semaphore Arrays", "/proc/sysvipc/sem", "semid"),
    ];
    if matches!(report, ReportKind::Limits) {
        return print_ipc_limits(msg, shm, sem);
    }
    for (selected, title, path, id_column) in selections {
        if !selected {
            continue;
        }
        let table = IpcTable::read(path)?;
        if matches!(report, ReportKind::Summary) {
            print_ipc_summary(title, &table);
        } else if let Some(id) = id {
            print_ipc_id(title, &table, id_column, id)?;
        } else {
            let columns: &[&str] = match report {
                ReportKind::Default => &[],
                ReportKind::Times => match id_column {
                    "msqid" => &["msqid", "uid", "stime", "rtime", "ctime"],
                    "shmid" => &["shmid", "uid", "atime", "dtime", "ctime"],
                    _ => &["semid", "uid", "otime", "ctime"],
                },
                ReportKind::Pids => match id_column {
                    "msqid" => &["msqid", "uid", "lspid", "lrpid"],
                    "shmid" => &["shmid", "uid", "cpid", "lpid"],
                    _ => &["semid", "uid"],
                },
                ReportKind::Creators => &[id_column, "perms", "cuid", "cgid", "uid", "gid"],
                ReportKind::Limits | ReportKind::Summary => unreachable!(),
            };
            print_ipc_table(title, &table, columns);
        }
    }
    Ok(())
}

struct IpcTable {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl IpcTable {
    fn read(path: &str) -> Result<Self, String> {
        let content = fs::read_to_string(path).map_err(|error| error.to_string())?;
        let mut lines = content.lines();
        let headers = lines
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let rows = lines
            .map(|line| line.split_whitespace().map(str::to_owned).collect())
            .collect();
        Ok(Self { headers, rows })
    }

    fn column(&self, name: &str) -> Option<usize> {
        self.headers.iter().position(|header| header == name)
    }
}

fn print_ipc_table(title: &str, table: &IpcTable, columns: &[&str]) {
    println!("\n------ {title} --------");
    let selected = if columns.is_empty() {
        (0..table.headers.len()).collect::<Vec<_>>()
    } else {
        columns
            .iter()
            .filter_map(|column| table.column(column))
            .collect()
    };
    println!(
        "{}",
        selected
            .iter()
            .map(|index| table.headers[*index].as_str())
            .collect::<Vec<_>>()
            .join("\t")
    );
    for row in &table.rows {
        println!(
            "{}",
            selected
                .iter()
                .filter_map(|index| row.get(*index).map(String::as_str))
                .collect::<Vec<_>>()
                .join("\t")
        );
    }
}

fn print_ipc_id(title: &str, table: &IpcTable, id_column: &str, id: i64) -> Result<(), String> {
    let index = table.column(id_column).ok_or("missing IPC ID column")?;
    let row = table
        .rows
        .iter()
        .find(|row| row.get(index).is_some_and(|value| value == &id.to_string()))
        .ok_or_else(|| format!("id {id} not found"))?;
    println!("\n{title}:");
    for (header, value) in table.headers.iter().zip(row) {
        println!("{header} = {value}");
    }
    Ok(())
}

fn print_ipc_summary(title: &str, table: &IpcTable) {
    println!("{title}: {} allocated", table.rows.len());
}

fn print_ipc_limits(msg: bool, shm: bool, sem: bool) -> Result<(), String> {
    println!("------ IPC Limits --------");
    let groups: &[(&str, &[(&str, &str)])] = &[
        (
            "Messages",
            &[
                ("max queues system wide", "/proc/sys/kernel/msgmni"),
                ("max size of message", "/proc/sys/kernel/msgmax"),
                ("default max size of queue", "/proc/sys/kernel/msgmnb"),
            ],
        ),
        (
            "Shared Memory",
            &[
                ("max number of segments", "/proc/sys/kernel/shmmni"),
                ("max seg size", "/proc/sys/kernel/shmmax"),
                ("max total shared memory", "/proc/sys/kernel/shmall"),
            ],
        ),
        (
            "Semaphores",
            &[(
                "limits (SEMMSL SEMMNS SEMOPM SEMMNI)",
                "/proc/sys/kernel/sem",
            )],
        ),
    ];
    for (selected, (title, values)) in [msg, shm, sem].into_iter().zip(groups) {
        if !selected {
            continue;
        }
        println!("{title}:");
        for (label, path) in *values {
            let value = fs::read_to_string(path).map_err(|error| error.to_string())?;
            println!("  {label} = {}", value.trim());
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
}

fn run_iostat(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut options = IoOptions::default();
    let mut numbers = Vec::new();
    for value in values.into_iter().skip(1) {
        match value.as_str() {
            "-c" => options.cpu = true,
            "-d" => options.disk = true,
            "-x" => options.extended = true,
            "-m" => options.megabytes = true,
            "-k" => options.megabytes = false,
            "-z" => options.omit_zero = true,
            "-y" => options.omit_first = true,
            "-h" => options.human = true,
            "--help" => {
                println!("Usage: iostat [-cdxhmzy] [INTERVAL [COUNT]]");
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ => numbers.push(
                value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid interval/count '{value}'"))?,
            ),
        }
    }
    if !options.cpu && !options.disk {
        options.cpu = true;
        options.disk = true;
    }
    options.interval = numbers.first().copied();
    options.count = numbers.get(1).copied();
    if numbers.len() > 2 {
        return Err("too many arguments".into());
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
    cpu: [u64; 8],
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
                    io_ms: parse(12),
                    weighted_ms: parse(13),
                },
            );
        }
        Ok(Self { cpu, disks })
    }
}

fn print_iostat(current: &IoSnapshot, previous: Option<&IoSnapshot>, options: &IoOptions) {
    let seconds = options.interval.unwrap_or_else(|| uptime_seconds().max(1));
    if options.cpu {
        let cpu = delta_array(current.cpu, previous.map(|value| value.cpu));
        let total = cpu.iter().sum::<u64>().max(1) as f64;
        let pct = |value: u64| value as f64 * 100.0 / total;
        println!("avg-cpu:  %user   %nice %system %iowait  %steal   %idle");
        println!(
            "         {:6.2} {:6.2} {:7.2} {:7.2} {:7.2} {:7.2}\n",
            pct(cpu[0]),
            pct(cpu[1]),
            pct(cpu[2]),
            pct(cpu[4]),
            pct(cpu[7]),
            pct(cpu[3])
        );
    }
    if options.disk {
        let unit = if options.human {
            "bytes"
        } else if options.megabytes {
            "MB"
        } else {
            "kB"
        };
        println!(
            "Device            tps    {unit}_read/s    {unit}_wrtn/s    {unit}_read    {unit}_wrtn"
        );
        for (name, stat) in &current.disks {
            let old = previous.and_then(|snapshot| snapshot.disks.get(name));
            let delta = disk_delta(stat, old);
            if options.omit_zero && delta.reads + delta.writes == 0 {
                continue;
            }
            let rate = seconds.max(1) as f64;
            if options.human {
                let read_rate = human_io_size(delta.sectors_read as f64 * 512.0 / rate);
                let write_rate = human_io_size(delta.sectors_written as f64 * 512.0 / rate);
                let read_total = human_io_size(delta.sectors_read as f64 * 512.0);
                let write_total = human_io_size(delta.sectors_written as f64 * 512.0);
                println!(
                    "{name:<12} {:8.2} {:>12} {:>12} {:>11} {:>11}",
                    (delta.reads + delta.writes) as f64 / rate,
                    read_rate,
                    write_rate,
                    read_total,
                    write_total
                );
            } else {
                let divisor = if options.megabytes { 2048.0 } else { 2.0 };
                println!(
                    "{name:<12} {:8.2} {:12.2} {:12.2} {:11.0} {:11.0}",
                    (delta.reads + delta.writes) as f64 / rate,
                    delta.sectors_read as f64 / divisor / rate,
                    delta.sectors_written as f64 / divisor / rate,
                    delta.sectors_read as f64 / divisor,
                    delta.sectors_written as f64 / divisor
                );
            }
            if options.extended {
                let await_ms = if delta.reads + delta.writes == 0 {
                    0.0
                } else {
                    (delta.read_ms + delta.write_ms) as f64 / (delta.reads + delta.writes) as f64
                };
                println!(
                    "             await={await_ms:.2} aqu-sz={:.2} %util={:.2}",
                    delta.weighted_ms as f64 / 1000.0 / rate,
                    delta.io_ms as f64 / 10.0 / rate
                );
            }
        }
        println!();
    }
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
    let old = old.cloned().unwrap_or_default();
    DiskStat {
        reads: current.reads.saturating_sub(old.reads),
        sectors_read: current.sectors_read.saturating_sub(old.sectors_read),
        read_ms: current.read_ms.saturating_sub(old.read_ms),
        writes: current.writes.saturating_sub(old.writes),
        sectors_written: current.sectors_written.saturating_sub(old.sectors_written),
        write_ms: current.write_ms.saturating_sub(old.write_ms),
        io_ms: current.io_ms.saturating_sub(old.io_ms),
        weighted_ms: current.weighted_ms.saturating_sub(old.weighted_ms),
    }
}

fn uptime_seconds() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|value| value.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(1.0) as u64
}

pub fn lsof(args: Vec<OsString>) -> i32 {
    report("lsof", run_lsof(args))
}

fn run_lsof(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let names = NameDatabase::load();
    let mut pids = BTreeSet::new();
    let mut user_filter = BTreeSet::new();
    let mut command_filter = None;
    let mut terse = false;
    let mut numeric_hosts = false;
    let mut numeric_ports = false;
    let mut and_selection = false;
    let mut command_width = 9;
    let mut paths = Vec::new();
    let mut recursive_directories = Vec::new();
    let mut directories = Vec::new();
    let mut networks = Vec::new();
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-p" => parse_pid_list(&iter.next().ok_or("-p requires a PID")?, &mut pids)?,
            "-c" => command_filter = Some(iter.next().ok_or("-c requires a command")?),
            "-u" => parse_user_list(
                &iter.next().ok_or("-u requires a user")?,
                &names,
                &mut user_filter,
            )?,
            "-t" => terse = true,
            "-n" => numeric_hosts = true,
            "-P" => numeric_ports = true,
            "-a" => and_selection = true,
            "-i" => networks.push(NetworkFilter::default()),
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
                    "Usage: lsof [-nP] [-a] [-p PID] [-c COMMAND] [-u USER] [-i[46][TCP|UDP][:PORT]] [-t] [+d DIR] [+D DIR] [PATH ...]"
                );
                return Ok(());
            }
            value if value.starts_with("-p") && value.len() > 2 => {
                parse_pid_list(&value[2..], &mut pids)?;
            }
            value if value.starts_with("-c") && value.len() > 2 => {
                command_filter = Some(value[2..].to_owned());
            }
            value if value.starts_with("-u") && value.len() > 2 => {
                parse_user_list(&value[2..], &names, &mut user_filter)?;
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
        !pids.is_empty() || command_filter.is_some() || !user_filter.is_empty();
    let has_file_filters = !paths.is_empty()
        || !recursive_directories.is_empty()
        || !directories.is_empty()
        || !networks.is_empty();
    let mut processes = fs::read_dir("/proc")
        .map_err(|error| error.to_string())?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .collect::<Vec<_>>();
    processes.sort_unstable();
    if !terse {
        println!("COMMAND     PID USER   FD      TYPE DEVICE  SIZE/OFF       NODE NAME");
    }
    let mut emitted_pids = BTreeSet::new();
    for pid in processes {
        let base = Path::new("/proc").join(pid.to_string());
        let command = fs::read_to_string(base.join("comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let uid = process_uid(&base).unwrap_or_default();
        let matches = [
            pids.is_empty() || pids.contains(&pid),
            command_filter
                .as_ref()
                .is_none_or(|filter| command.starts_with(filter)),
            user_filter.is_empty() || user_filter.contains(&uid),
        ];
        let process_selected = if and_selection {
            matches.into_iter().all(|matched| matched)
        } else {
            (!pids.is_empty() && pids.contains(&pid))
                || command_filter
                    .as_ref()
                    .is_some_and(|filter| command.starts_with(filter))
                || (!user_filter.is_empty() && user_filter.contains(&uid))
                || (!has_process_filters && !has_file_filters)
        };
        if and_selection && !process_selected {
            continue;
        }

        let mut emitter = OpenEmitter {
            command: &command,
            command_width,
            pid,
            uid,
            terse,
            process_selected,
            and_selection,
            has_process_filters,
            has_file_filters,
            paths: &paths,
            recursive_directories: &recursive_directories,
            directories: &directories,
            networks: &networks,
            sockets: &sockets,
            names: &names,
            numeric_hosts,
            numeric_ports,
            emitted_pids: &mut emitted_pids,
        };
        for (fd, path) in [
            ("cwd", base.join("cwd")),
            ("rtd", base.join("root")),
            ("txt", base.join("exe")),
        ] {
            if let Ok(target) = fs::read_link(&path) {
                emitter.emit(fd, &target);
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
                emitter.emit(&fd, &target);
            }
        }
    }
    Ok(())
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
}

#[derive(Default)]
struct NetworkFilter {
    family: Option<SocketFamily>,
    protocol: Option<&'static str>,
    port: Option<u16>,
}

struct NameDatabase {
    users: HashMap<u32, String>,
    user_ids: HashMap<String, u32>,
    hosts: HashMap<IpAddr, String>,
    services: HashMap<(u16, String), String>,
    service_ports: HashMap<(String, String), u16>,
}

impl NameDatabase {
    fn load() -> Self {
        let mut database = Self {
            users: HashMap::new(),
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
}

struct OpenEmitter<'a> {
    command: &'a str,
    command_width: usize,
    pid: u32,
    uid: u32,
    terse: bool,
    process_selected: bool,
    and_selection: bool,
    has_process_filters: bool,
    has_file_filters: bool,
    paths: &'a [String],
    recursive_directories: &'a [String],
    directories: &'a [String],
    networks: &'a [NetworkFilter],
    sockets: &'a HashMap<u64, SocketInfo>,
    names: &'a NameDatabase,
    numeric_hosts: bool,
    numeric_ports: bool,
    emitted_pids: &'a mut BTreeSet<u32>,
}

impl OpenEmitter<'_> {
    fn emit(&mut self, fd: &str, target: &Path) {
        let target_name = target.to_string_lossy();
        let socket_inode = procfs::socket_inode(target.as_os_str().as_bytes());
        let socket = socket_inode.and_then(|inode| self.sockets.get(&inode));
        let path_match = self.paths.iter().any(|path| target_name == path.as_str())
            || self
                .recursive_directories
                .iter()
                .any(|directory| path_is_below(&target_name, directory, true))
            || self
                .directories
                .iter()
                .any(|directory| path_is_below(&target_name, directory, false));
        let network_match = socket.is_some_and(|socket| {
            self.networks
                .iter()
                .any(|filter| socket_matches(socket, filter))
        });
        let file_selected = if self.has_file_filters {
            path_match || network_match
        } else {
            true
        };
        let selected = if self.and_selection {
            self.process_selected && file_selected
        } else {
            (!self.has_process_filters && !self.has_file_filters)
                || self.process_selected
                || file_selected
        };
        if !selected {
            return;
        }
        if self.terse {
            if self.emitted_pids.insert(self.pid) {
                println!("{}", self.pid);
            }
            return;
        }

        let metadata = fs::metadata(target).ok();
        let (kind, name, inode) = if let Some(socket) = socket {
            (
                match socket.family {
                    SocketFamily::Inet4 => "IPv4",
                    SocketFamily::Inet6 => "IPv6",
                    SocketFamily::Unix => "unix",
                },
                format_socket(socket, self.names, self.numeric_hosts, self.numeric_ports),
                socket_inode
                    .map(|inode| inode.to_string())
                    .unwrap_or_default(),
            )
        } else {
            (
                if target_name.starts_with("pipe:") {
                    "FIFO"
                } else if metadata.as_ref().is_some_and(|value| value.is_dir()) {
                    "DIR"
                } else {
                    "REG"
                },
                target_name.into_owned(),
                metadata
                    .as_ref()
                    .map(|value| value.ino().to_string())
                    .unwrap_or_default(),
            )
        };
        let device = metadata
            .as_ref()
            .map(|value| format!("{},{}", libc::major(value.dev()), libc::minor(value.dev())))
            .unwrap_or_default();
        let size = metadata
            .as_ref()
            .map(|value| value.len().to_string())
            .unwrap_or_default();
        let user = self
            .names
            .users
            .get(&self.uid)
            .cloned()
            .unwrap_or_else(|| self.uid.to_string());
        println!(
            "{:<width$} {:>6} {:>5} {:>4} {:>9} {:>7} {:>9} {:>10} {}",
            truncate(self.command, self.command_width),
            self.pid,
            user,
            fd,
            kind,
            device,
            size,
            inode,
            name,
            width = self.command_width
        );
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
                    state: procfs::tcp_state(socket.state),
                    unix_path: None,
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

fn path_is_below(target: &str, directory: &str, recursive: bool) -> bool {
    let target = Path::new(target);
    let directory = Path::new(directory);
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
) -> String {
    if socket.family == SocketFamily::Unix {
        return socket
            .unix_path
            .clone()
            .unwrap_or_else(|| "type=STREAM".into());
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
    let mut output = format!("{} {local}", socket.protocol);
    if let Some(remote) = remote {
        output.push_str("->");
        output.push_str(&remote);
    }
    if let Some(state) = socket.state {
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
    let status = fs::read_to_string(base.join("status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("Uid:")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

fn truncate(value: &str, length: usize) -> &str {
    value.get(..length).unwrap_or(value)
}

pub fn lspci(args: Vec<OsString>) -> i32 {
    report("lspci", run_lspci(args))
}

fn run_lspci(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut numeric = false;
    let mut show_kernel = false;
    let mut verbose = false;
    let mut slot_filter = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-n" | "-nn" => numeric = true,
            "-k" => show_kernel = true,
            "-v" | "-vv" | "-vvv" => verbose = true,
            "-s" => slot_filter = Some(iter.next().ok_or("-s requires a slot")?),
            "-h" | "--help" => {
                println!("Usage: lspci [-n] [-k] [-v] [-s SLOT]");
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unsupported option '{value}'")),
            _ => return Err(format!("unexpected operand '{value}'")),
        }
    }
    let mut entries = read_dir_optional(Path::new("/sys/bus/pci/devices"))?;
    entries.sort_unstable_by_key(|entry| entry.file_name());
    for entry in entries {
        let full = entry.file_name().to_string_lossy().into_owned();
        let slot = full.strip_prefix("0000:").unwrap_or(&full);
        if slot_filter
            .as_ref()
            .is_some_and(|filter| !slot.starts_with(filter))
        {
            continue;
        }
        let vendor = sys_hex(&entry.path().join("vendor"));
        let device = sys_hex(&entry.path().join("device"));
        let class = sys_hex(&entry.path().join("class"));
        let class_name = pci_class(&class);
        if numeric {
            println!("{slot} {class}: {vendor}:{device}");
        } else {
            println!("{slot} {class_name}: Device {vendor}:{device}");
        }
        if show_kernel && let Ok(driver) = fs::read_link(entry.path().join("driver")) {
            println!(
                "\tKernel driver in use: {}",
                driver.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        if verbose {
            println!(
                "\tSubsystem: {}:{}",
                sys_hex(&entry.path().join("subsystem_vendor")),
                sys_hex(&entry.path().join("subsystem_device"))
            );
            if let Ok(irq) = fs::read_to_string(entry.path().join("irq")) {
                println!("\tInterrupt: {}", irq.trim());
            }
        }
    }
    Ok(())
}

fn pci_class(value: &str) -> &'static str {
    match value.trim_start_matches("0x").get(..2).unwrap_or("") {
        "01" => "Mass storage controller",
        "02" => "Network controller",
        "03" => "Display controller",
        "04" => "Multimedia controller",
        "05" => "Memory controller",
        "06" => "Bridge",
        "07" => "Communication controller",
        "08" => "System peripheral",
        "0c" => "Serial bus controller",
        _ => "Unclassified device",
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
    let mut size = false;
    for value in values.into_iter().skip(1) {
        match value.as_str() {
            "-g" | "--generic" => generic = true,
            "-s" | "--size" => size = true,
            "-v" | "--verbose" => {}
            "-h" | "--help" => {
                println!("Usage: lsscsi [-g] [-s]");
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unsupported option '{value}'")),
            _ => return Err(format!("unexpected operand '{value}'")),
        }
    }
    let mut entries = read_dir_optional(Path::new("/sys/class/scsi_device"))?;
    entries.sort_unstable_by_key(|entry| entry.file_name());
    for entry in entries {
        let address = entry.file_name().to_string_lossy().into_owned();
        let device = entry.path().join("device");
        let kind = scsi_type(&sys_text(&device.join("type")));
        let vendor = sys_text(&device.join("vendor"));
        let model = sys_text(&device.join("model"));
        let revision = sys_text(&device.join("rev"));
        let block = fs::read_dir(device.join("block"))
            .ok()
            .and_then(|mut entries| entries.next()?.ok())
            .map(|entry| format!("/dev/{}", entry.file_name().to_string_lossy()))
            .unwrap_or("-".into());
        print!("[{address}]  {kind:<8} {vendor:<8} {model:<16} {revision:<4} {block}");
        if generic {
            let sg = fs::read_dir(device.join("scsi_generic"))
                .ok()
                .and_then(|mut entries| entries.next()?.ok())
                .map(|entry| format!(" /dev/{}", entry.file_name().to_string_lossy()))
                .unwrap_or_default();
            print!("{sg}");
        }
        if size && block != "-" {
            let name = Path::new(&block).file_name().unwrap_or_default();
            let sectors = sys_text(&Path::new("/sys/class/block").join(name).join("size"))
                .parse::<u64>()
                .unwrap_or_default();
            print!("  {}", human_size(sectors * 512));
        }
        println!();
    }
    Ok(())
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

fn human_size(bytes: u64) -> String {
    let units = ["B", "K", "M", "G", "T", "P"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < units.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.2}{}", units[unit])
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
            "-0" | "--null" => null = true,
            "-h" | "--help" => {
                println!("Usage: modinfo [-F FIELD] [-0] MODULE ...");
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
        let mut fields = data
            .split(|byte| *byte == 0)
            .filter_map(|entry| std::str::from_utf8(entry).ok())
            .filter_map(|entry| entry.split_once('='))
            .collect::<Vec<_>>();
        fields.sort_by_key(|(name, _)| *name);
        let separator = if null { "\0" } else { "\n" };
        match field.as_deref() {
            Some("filename") => print!("{}{separator}", path.display()),
            None => print!("filename:       {}{separator}", path.display()),
            Some(_) => {}
        }
        for (name, value) in fields {
            if field.as_deref().is_some_and(|field| field != name) {
                continue;
            }
            if field.is_some() {
                print!("{value}{separator}");
            } else {
                print!("{name:<15} {value}{separator}");
            }
        }
    }
    Ok(())
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

fn utf8_args(args: Vec<OsString>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|value| {
            value.into_string().map_err(|value| {
                format!("argument is not valid UTF-8: {}", value.to_string_lossy())
            })
        })
        .collect()
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
        assert_eq!(human_size(1_000_000_000), "1.00G");
    }
}
