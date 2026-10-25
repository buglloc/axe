use clap::Parser;
use cols::Cols;
use procfs::prelude::*;
use procutils_common::{MAX_TERM_WIDTH, man::ManContent};
use std::{collections::HashMap, process::ExitCode};

pub const MAN: ManContent = ManContent {
    description: Some(include_str!("../man/description.man")),
    extra_sections: &[
        (
            "FIELD DESCRIPTIONS",
            include_str!("../man/field_descriptions.man"),
        ),
        ("EXAMPLES", include_str!("../man/examples.man")),
        ("NOTES", include_str!("../man/notes.man")),
        ("DIVERGENCES", include_str!("../man/divergences.man")),
        ("SEE ALSO", include_str!("../man/see_also.man")),
    ],
};

/// Report virtual memory statistics.
#[derive(Parser)]
#[command(name = "vmstat", version, about, max_term_width = MAX_TERM_WIDTH)]
pub struct Args {
    /// Display active and inactive memory.
    #[arg(short = 'a', long)]
    active: bool,

    /// Display the number of forks since boot.
    #[arg(short, long)]
    forks: bool,

    /// Display slabinfo.
    #[arg(short = 'm', long)]
    slabs: bool,

    /// Display the header only once rather than periodically.
    #[arg(short = 'n', long)]
    one_header: bool,

    /// Display a table of various event counters and memory statistics.
    #[arg(short, long)]
    stats: bool,

    /// Report disk statistics.
    #[arg(short, long)]
    disk: bool,

    /// Report summary statistics about disk activity.
    #[arg(short = 'D', long)]
    disk_sum: bool,

    /// Switch output unit: k (1000), K (1024), m (1000000), M (1048576).
    #[arg(short = 'S', long, default_value = "K")]
    unit: char,

    /// Append timestamp to each line.
    #[arg(short, long)]
    timestamp: bool,

    /// Wide output mode.
    #[arg(short, long)]
    wide: bool,

    /// Omit first report with statistics since system boot.
    #[arg(short = 'y', long)]
    no_first: bool,

    /// Delay between updates in seconds.
    delay: Option<u64>,

    /// Number of updates.
    count: Option<u64>,
}

struct Snapshot {
    kernel: procfs::KernelStats,
    meminfo: procfs::Meminfo,
    vmstat: HashMap<String, i64>,
    total_interrupts: u64,
}

impl Snapshot {
    fn take() -> Result<Self, String> {
        let kernel = procfs::KernelStats::current()
            .map_err(|e| format!("failed to read /proc/stat: {e}"))?;
        let meminfo =
            procfs::Meminfo::current().map_err(|e| format!("failed to read /proc/meminfo: {e}"))?;
        let vmstat = procfs::vmstat().map_err(|e| format!("failed to read /proc/vmstat: {e}"))?;
        let total_interrupts = read_total_interrupts().unwrap_or(0);
        Ok(Self {
            kernel,
            meminfo,
            vmstat,
            total_interrupts,
        })
    }
}

struct StatRow {
    value: u64,
    label: String,
}

struct VmRow {
    r: String,
    b: String,
    swpd: String,
    free: String,
    buff: String,
    cache: String,
    inact: String,
    active: String,
    si: String,
    so: String,
    bi: String,
    bo: String,
    intr: String,
    cs: String,
    us: String,
    sy: String,
    id: String,
    wa: String,
    st: String,
    gu: String,
}

#[derive(Cols)]
struct DiskRow {
    #[column(header = "DISK")]
    name: String,
    #[column(right, header = "TOTAL")]
    reads_total: String,
    #[column(right, header = "MERGED")]
    reads_merged: String,
    #[column(right, header = "SECTORS")]
    read_sectors: String,
    #[column(right, header = "MS")]
    read_ms: String,
    #[column(right, header = "TOTAL")]
    writes_total: String,
    #[column(right, header = "MERGED")]
    writes_merged: String,
    #[column(right, header = "SECTORS")]
    written_sectors: String,
    #[column(right, header = "MS")]
    write_ms: String,
    #[column(right, header = "cur")]
    in_progress: String,
    #[column(right, header = "sec")]
    io_sec: String,
}

fn unit_divisor(unit: char) -> f64 {
    match unit {
        'k' => 1000.0,
        'K' => 1024.0,
        'm' => 1_000_000.0,
        'M' => 1_048_576.0,
        _ => 1024.0,
    }
}

fn to_unit(bytes: u64, divisor: f64) -> u64 {
    (bytes as f64 / divisor) as u64
}

pub fn run(args: Args) -> ExitCode {
    if args.forks {
        return run_forks();
    }
    if args.stats {
        return run_stats(&args);
    }
    if args.disk {
        return run_disk(&args);
    }
    if args.disk_sum {
        return run_disk_sum();
    }
    if args.slabs {
        return run_slabs();
    }
    run_vm(&args)
}

fn print_table<T: Cols>(rows: &[T]) {
    let table = T::to_table(rows);
    cols::print_table(&table, &mut std::io::stdout().lock()).unwrap();
}

fn print_stats(rows: &[StatRow]) -> ExitCode {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let result = (|| -> std::io::Result<()> {
        for row in rows {
            writeln!(out, "{:>13} {}", row.value, row.label)?;
        }
        out.flush()
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vmstat: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_forks() -> ExitCode {
    match procfs::KernelStats::current() {
        Ok(k) => print_stats(&[StatRow {
            value: k.processes,
            label: "forks".into(),
        }]),
        Err(e) => {
            eprintln!("vmstat: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_stats(args: &Args) -> ExitCode {
    let snap = match Snapshot::take() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vmstat: {e}");
            return ExitCode::FAILURE;
        }
    };

    let div = unit_divisor(args.unit);
    let u = match args.unit {
        'k' | 'K' => "K",
        'm' | 'M' => "M",
        _ => "K",
    };
    let m = &snap.meminfo;
    let cpu = &snap.kernel.total;
    let v = &snap.vmstat;

    let s = |val: u64, label: &str| StatRow {
        value: val,
        label: label.into(),
    };

    let rows = vec![
        s(to_unit(m.mem_total, div), &format!("{u} total memory")),
        s(
            to_unit(
                m.mem_total
                    .saturating_sub(m.mem_available.unwrap_or_else(|| {
                        m.mem_free
                            .saturating_add(m.buffers)
                            .saturating_add(m.cached)
                            .saturating_add(m.s_reclaimable.unwrap_or(0))
                    })),
                div,
            ),
            &format!("{u} used memory"),
        ),
        s(to_unit(m.active, div), &format!("{u} active memory")),
        s(to_unit(m.inactive, div), &format!("{u} inactive memory")),
        s(to_unit(m.mem_free, div), &format!("{u} free memory")),
        s(to_unit(m.buffers, div), &format!("{u} buffer memory")),
        s(
            to_unit(m.cached + m.s_reclaimable.unwrap_or(0), div),
            &format!("{u} swap cache"),
        ),
        s(to_unit(m.swap_total, div), &format!("{u} total swap")),
        s(
            to_unit(m.swap_total - m.swap_free, div),
            &format!("{u} used swap"),
        ),
        s(to_unit(m.swap_free, div), &format!("{u} free swap")),
        s(cpu.user, "non-nice user cpu ticks"),
        s(cpu.nice, "nice user cpu ticks"),
        s(cpu.system, "system cpu ticks"),
        s(cpu.idle, "idle cpu ticks"),
        s(cpu.iowait.unwrap_or(0), "IO-wait cpu ticks"),
        s(cpu.irq.unwrap_or(0), "IRQ cpu ticks"),
        s(cpu.softirq.unwrap_or(0), "softirq cpu ticks"),
        s(cpu.steal.unwrap_or(0), "stolen cpu ticks"),
        s(cpu.guest.unwrap_or(0), "non-nice guest cpu ticks"),
        s(cpu.guest_nice.unwrap_or(0), "nice guest cpu ticks"),
        s(v.get("pgpgin").copied().unwrap_or(0) as u64, "K paged in"),
        s(v.get("pgpgout").copied().unwrap_or(0) as u64, "K paged out"),
        s(
            v.get("pswpin").copied().unwrap_or(0) as u64,
            "pages swapped in",
        ),
        s(
            v.get("pswpout").copied().unwrap_or(0) as u64,
            "pages swapped out",
        ),
        s(
            v.get("pgalloc_dma").copied().unwrap_or(0) as u64,
            "pages alloc in dma",
        ),
        s(
            v.get("pgalloc_dma32").copied().unwrap_or(0) as u64,
            "pages alloc in dma32",
        ),
        s(
            v.get("pgalloc_high").copied().unwrap_or(0) as u64,
            "pages alloc in high",
        ),
        s(
            v.get("pgalloc_movable").copied().unwrap_or(0) as u64,
            "pages alloc in movable",
        ),
        s(
            v.get("pgalloc_normal").copied().unwrap_or(0) as u64,
            "pages alloc in normal",
        ),
        s(v.get("pgfree").copied().unwrap_or(0) as u64, "pages free"),
        s(snap.total_interrupts, "interrupts"),
        s(snap.kernel.ctxt, "CPU context switches"),
        s(snap.kernel.btime, "boot time"),
        s(snap.kernel.processes, "forks"),
    ];

    print_stats(&rows)
}

fn run_disk(args: &Args) -> ExitCode {
    let iterations = match (args.delay, args.count) {
        (None, None) => 1,
        (Some(_), None) => u64::MAX,
        (None, Some(c)) => c,
        (Some(_), Some(c)) => c,
    };
    let interval = args.delay.map(std::time::Duration::from_secs);

    for i in 0..iterations {
        if i > 0
            && let Some(d) = interval
        {
            std::thread::sleep(d);
        }

        let disks = match procfs::diskstats() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("vmstat: {e}");
                return ExitCode::FAILURE;
            }
        };

        let rows: Vec<DiskRow> = disks
            .iter()
            .map(|d| DiskRow {
                name: d.name.clone(),
                reads_total: d.reads.to_string(),
                reads_merged: d.merged.to_string(),
                read_sectors: d.sectors_read.to_string(),
                read_ms: d.time_reading.to_string(),
                writes_total: d.writes.to_string(),
                writes_merged: d.writes_merged.to_string(),
                written_sectors: d.sectors_written.to_string(),
                write_ms: d.time_writing.to_string(),
                in_progress: d.in_progress.to_string(),
                io_sec: (d.time_in_progress / 1000).to_string(),
            })
            .collect();

        if i == 0 || !args.one_header {
            print_table(&rows);
        } else {
            // Print without header by building a manual table
            let table = DiskRow::to_table(&rows);
            // cols always prints headers, so just use print_table
            // For one_header after first iteration, we still need to show data.
            // cols doesn't support headerless mode easily, so just print the table.
            cols::print_table(&table, &mut std::io::stdout().lock()).unwrap();
        }
    }

    ExitCode::SUCCESS
}

fn run_disk_sum() -> ExitCode {
    let disks = match procfs::diskstats() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("vmstat: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut total_reads = 0u64;
    let mut merged_reads = 0u64;
    let mut read_sectors = 0u64;
    let mut read_ms = 0u64;
    let mut total_writes = 0u64;
    let mut merged_writes = 0u64;
    let mut written_sectors = 0u64;
    let mut write_ms = 0u64;
    let mut in_progress = 0u64;
    let mut io_ms = 0u64;
    let mut weighted_ms = 0u64;
    let mut disk_count = 0u64;
    let mut partition_count = 0u64;

    for d in &disks {
        if d.minor == 0 || !d.name.chars().last().is_some_and(|c| c.is_ascii_digit()) {
            disk_count += 1;
        } else {
            partition_count += 1;
        }
        total_reads += d.reads;
        merged_reads += d.merged;
        read_sectors += d.sectors_read;
        read_ms += d.time_reading;
        total_writes += d.writes;
        merged_writes += d.writes_merged;
        written_sectors += d.sectors_written;
        write_ms += d.time_writing;
        in_progress += d.in_progress;
        io_ms += d.time_in_progress;
        weighted_ms += d.weighted_time_in_progress;
    }

    let s = |val: u64, label: &str| StatRow {
        value: val,
        label: label.into(),
    };

    let rows = vec![
        s(disk_count, "disks"),
        s(partition_count, "partitions"),
        s(total_reads, "total reads"),
        s(merged_reads, "merged reads"),
        s(read_sectors, "read sectors"),
        s(read_ms, "milli reading"),
        s(total_writes, "writes"),
        s(merged_writes, "merged writes"),
        s(written_sectors, "written sectors"),
        s(write_ms, "milli writing"),
        s(in_progress, "inprogress IO"),
        s(io_ms, "milli spent IO"),
        s(weighted_ms, "milli weighted IO"),
    ];

    print_stats(&rows)
}

#[derive(Cols)]
struct SlabRow {
    #[column(header = "CACHE")]
    cache: String,
    #[column(right, header = "NUM")]
    num: String,
    #[column(right, header = "TOTAL")]
    total: String,
    #[column(right, header = "SIZE")]
    size: String,
    #[column(right, header = "PAGES")]
    pages: String,
}

fn run_slabs() -> ExitCode {
    let content = match std::fs::read_to_string("/proc/slabinfo") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("vmstat: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut rows: Vec<SlabRow> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("slabinfo") {
            continue;
        }
        // Format: NAME N1 N2 N3 N4 N5 : tunables A B C : slabdata X Y Z
        let segments: Vec<&str> = line.split(" : ").collect();
        if segments.len() < 3 {
            continue;
        }
        let primary: Vec<&str> = segments[0].split_whitespace().collect();
        if primary.len() < 6 {
            continue;
        }
        let slabdata: Vec<&str> = segments[2].split_whitespace().collect();
        // Skipping the leading "slabdata" word, we want num_slabs at index 2.
        if slabdata.len() < 3 {
            continue;
        }
        rows.push(SlabRow {
            cache: primary[0].into(),
            num: primary[1].into(),
            total: primary[2].into(),
            size: primary[3].into(),
            pages: slabdata[2].into(),
        });
    }

    print_table(&rows);
    ExitCode::SUCCESS
}

fn print_vm_row(row: &VmRow, args: &Args, show_header: bool) -> std::io::Result<()> {
    use std::io::Write;
    let fields = [
        ("r", &row.r, 2, 4),
        ("b", &row.b, 2, 4),
        ("swpd", &row.swpd, 6, 12),
        ("free", &row.free, 6, 12),
        if args.active {
            ("inact", &row.inact, 6, 12)
        } else {
            ("buff", &row.buff, 6, 12)
        },
        if args.active {
            ("active", &row.active, 6, 12)
        } else {
            ("cache", &row.cache, 6, 12)
        },
        ("si", &row.si, 4, 4),
        ("so", &row.so, 4, 4),
        ("bi", &row.bi, 5, 5),
        ("bo", &row.bo, 5, 5),
        ("in", &row.intr, 4, 4),
        ("cs", &row.cs, 4, 4),
        ("us", &row.us, 2, 3),
        ("sy", &row.sy, 2, 3),
        ("id", &row.id, 2, 3),
        ("wa", &row.wa, 2, 3),
        ("st", &row.st, 2, 3),
        ("gu", &row.gu, 2, 3),
    ];
    let mut out = std::io::stdout().lock();
    if show_header {
        writeln!(
            out,
            "{}",
            if args.wide {
                "--procs-- -----------------------memory---------------------- ---swap-- -----io---- -system-- ----------cpu----------"
            } else {
                "procs -----------memory---------- ---swap-- -----io---- -system-- -------cpu-------"
            }
        )?;
        for (i, (name, _, normal, wide)) in fields.iter().enumerate() {
            let width = if args.wide { *wide } else { *normal };
            write!(out, "{}{name:>width$}", if i == 0 { "" } else { " " })?;
        }
        writeln!(out)?;
    }
    let mut skew = 0_usize;
    for (i, (_, value, normal, wide)) in fields.iter().enumerate() {
        let mut width = if args.wide { *wide } else { *normal };
        let recovered = skew.min(width.saturating_sub(value.len()));
        width -= recovered;
        skew -= recovered;
        write!(out, "{}{value:>width$}", if i == 0 { "" } else { " " })?;
        skew += value.len().saturating_sub(width);
    }
    writeln!(out)?;
    out.flush()
}

fn run_vm(args: &Args) -> ExitCode {
    let iterations = match (args.delay, args.count) {
        (None, None) => 1,
        (Some(_), None) => u64::MAX,
        (None, Some(c)) => c,
        (Some(_), Some(c)) => c,
    };
    let interval = args.delay.map(std::time::Duration::from_secs);
    let div = unit_divisor(args.unit);
    let swap_scale = procfs::page_size() as f64 / div;

    let mut prev: Option<Snapshot> = None;
    let header_interval = std::io::IsTerminal::is_terminal(&std::io::stdout()).then(|| {
        std::env::var("LINES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|&lines| lines > 2)
            .unwrap_or(24)
            - 2
    });
    let mut printed_rows = 0;

    for i in 0..iterations {
        if i > 0
            && let Some(d) = interval
        {
            std::thread::sleep(d);
        }

        if args.no_first && i == 0 {
            prev = Snapshot::take().ok();
            continue;
        }

        let snap = match Snapshot::take() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("vmstat: {e}");
                return ExitCode::FAILURE;
            }
        };

        let elapsed_secs = args.delay.unwrap_or(1).max(1) as f64;

        let (si, so, bi, bo) = if let Some(ref p) = prev {
            let pv = &p.vmstat;
            let cv = &snap.vmstat;
            let dsi = (cv.get("pswpin").unwrap_or(&0) - pv.get("pswpin").unwrap_or(&0)) as f64
                / elapsed_secs;
            let dso = (cv.get("pswpout").unwrap_or(&0) - pv.get("pswpout").unwrap_or(&0)) as f64
                / elapsed_secs;
            let dbi = (cv.get("pgpgin").unwrap_or(&0) - pv.get("pgpgin").unwrap_or(&0)) as f64
                / elapsed_secs;
            let dbo = (cv.get("pgpgout").unwrap_or(&0) - pv.get("pgpgout").unwrap_or(&0)) as f64
                / elapsed_secs;
            (
                (dsi * swap_scale) as u64,
                (dso * swap_scale) as u64,
                dbi as u64,
                dbo as u64,
            )
        } else {
            let uptime = procfs::Uptime::current()
                .map(|u| u.uptime)
                .unwrap_or(1.0)
                .max(1.0);
            let v = &snap.vmstat;
            let si = *v.get("pswpin").unwrap_or(&0) as f64 / uptime;
            let so = *v.get("pswpout").unwrap_or(&0) as f64 / uptime;
            let bi = *v.get("pgpgin").unwrap_or(&0) as f64 / uptime;
            let bo = *v.get("pgpgout").unwrap_or(&0) as f64 / uptime;
            (
                (si * swap_scale) as u64,
                (so * swap_scale) as u64,
                bi as u64,
                bo as u64,
            )
        };

        let (intr_s, cs_s) = if let Some(ref p) = prev {
            let di = snap.total_interrupts.saturating_sub(p.total_interrupts) as f64 / elapsed_secs;
            let dcs = (snap.kernel.ctxt - p.kernel.ctxt) as f64 / elapsed_secs;
            (di as u64, dcs as u64)
        } else {
            let uptime = procfs::Uptime::current()
                .map(|u| u.uptime)
                .unwrap_or(1.0)
                .max(1.0);
            let intr = snap.total_interrupts as f64 / uptime;
            let cs = snap.kernel.ctxt as f64 / uptime;
            (intr as u64, cs as u64)
        };

        let cpu = &snap.kernel.total;
        let (us, sy, id, wa, st, gu) = if let Some(ref p) = prev {
            cpu_pct(&p.kernel.total, cpu)
        } else {
            cpu_pct_total(cpu)
        };

        let m = &snap.meminfo;

        let row = VmRow {
            r: snap.kernel.procs_running.unwrap_or(0).to_string(),
            b: snap.kernel.procs_blocked.unwrap_or(0).to_string(),
            swpd: to_unit(m.swap_total - m.swap_free, div).to_string(),
            free: to_unit(m.mem_free, div).to_string(),
            buff: to_unit(m.buffers, div).to_string(),
            cache: to_unit(m.cached + m.s_reclaimable.unwrap_or(0), div).to_string(),
            inact: to_unit(m.inactive, div).to_string(),
            active: to_unit(m.active, div).to_string(),
            si: si.to_string(),
            so: so.to_string(),
            bi: bi.to_string(),
            bo: bo.to_string(),
            intr: intr_s.to_string(),
            cs: cs_s.to_string(),
            us: us.to_string(),
            sy: sy.to_string(),
            id: id.to_string(),
            wa: wa.to_string(),
            st: st.to_string(),
            gu: gu.to_string(),
        };

        let should_print_header = printed_rows == 0
            || (!args.one_header && header_interval.is_some_and(|lines| printed_rows % lines == 0));
        if let Err(error) = print_vm_row(&row, args, should_print_header) {
            eprintln!("vmstat: {error}");
            return ExitCode::FAILURE;
        }
        printed_rows += 1;

        prev = Some(snap);
    }

    ExitCode::SUCCESS
}

fn cpu_pct(prev: &procfs::CpuTime, cur: &procfs::CpuTime) -> (u64, u64, u64, u64, u64, u64) {
    let d = |c: u64, p: u64| c.saturating_sub(p);
    let user = d(cur.user, prev.user) + d(cur.nice, prev.nice);
    let system = d(cur.system, prev.system)
        + d(cur.irq.unwrap_or(0), prev.irq.unwrap_or(0))
        + d(cur.softirq.unwrap_or(0), prev.softirq.unwrap_or(0));
    let idle = d(cur.idle, prev.idle);
    let iowait = d(cur.iowait.unwrap_or(0), prev.iowait.unwrap_or(0));
    let steal = d(cur.steal.unwrap_or(0), prev.steal.unwrap_or(0));
    let guest = d(cur.guest.unwrap_or(0), prev.guest.unwrap_or(0))
        + d(cur.guest_nice.unwrap_or(0), prev.guest_nice.unwrap_or(0));

    let total = user + system + idle + iowait + steal + guest;
    if total == 0 {
        return (0, 0, 100, 0, 0, 0);
    }

    let pct = |v: u64| (v * 100 + total / 2) / total;
    (
        pct(user),
        pct(system),
        pct(idle),
        pct(iowait),
        pct(steal),
        pct(guest),
    )
}

fn cpu_pct_total(cpu: &procfs::CpuTime) -> (u64, u64, u64, u64, u64, u64) {
    let user = cpu.user + cpu.nice;
    let system = cpu.system + cpu.irq.unwrap_or(0) + cpu.softirq.unwrap_or(0);
    let idle = cpu.idle;
    let iowait = cpu.iowait.unwrap_or(0);
    let steal = cpu.steal.unwrap_or(0);
    let guest = cpu.guest.unwrap_or(0) + cpu.guest_nice.unwrap_or(0);

    let total = user + system + idle + iowait + steal + guest;
    if total == 0 {
        return (0, 0, 100, 0, 0, 0);
    }

    let pct = |v: u64| (v * 100 + total / 2) / total;
    (
        pct(user),
        pct(system),
        pct(idle),
        pct(iowait),
        pct(steal),
        pct(guest),
    )
}

fn read_total_interrupts() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/stat").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("intr ") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}
