use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseEventKind};
use procfs::prelude::*;
use procutils_common::{MAX_TERM_WIDTH, man::ManContent};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
use std::{borrow::Cow, collections::HashMap, io::Write as _, process::ExitCode, time::Duration};

pub const MAN: ManContent = ManContent {
    description: Some(include_str!("../man/description.man")),
    extra_sections: &[
        ("KEY COMMANDS", include_str!("../man/key_commands.man")),
        (
            "FIELD DESCRIPTIONS",
            include_str!("../man/field_descriptions.man"),
        ),
        ("EXAMPLES", include_str!("../man/examples.man")),
        ("NOTES", include_str!("../man/notes.man")),
        ("SEE ALSO", include_str!("../man/see_also.man")),
    ],
};

/// Display Linux processes.
#[derive(Parser)]
#[command(name = "top", version, about, max_term_width = MAX_TERM_WIDTH)]
pub struct Args {
    /// Update delay in seconds.
    #[arg(short, long, default_value = "3.0")]
    delay: f64,

    /// Exit after this many display updates.
    #[arg(short = 'n', long)]
    iterations: Option<u64>,

    /// Batch mode (non-interactive, for piping).
    #[arg(short, long)]
    batch: bool,

    /// Monitor only these PIDs (comma-separated).
    #[arg(short, long, value_delimiter = ',')]
    pid: Option<Vec<i32>>,

    /// Show only processes for this user.
    #[arg(short, long)]
    user: Option<String>,

    /// Start with per-CPU display.
    #[arg(short = '1')]
    per_cpu: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum SortField {
    Cpu,
    Mem,
    Pid,
    Time,
}

struct ProcessInfo {
    pid: i32,
    user: String,
    priority: i64,
    nice: i64,
    virt_kb: u64,
    res_kb: u64,
    shr_kb: u64,
    state: char,
    cpu_pct: f64,
    mem_pct: f64,
    time_ticks: u64,
    command: String,
    cmdline: String,
}

use procutils_common::{fmt::format_kb, uid::UidCache, utmp};

fn format_time_plus(ticks: u64, tps: u64) -> String {
    if tps == 0 {
        return "0:00.00".to_string();
    }
    let total_secs = ticks / tps;
    let hundredths = (ticks % tps) * 100 / tps;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins}:{secs:02}.{hundredths:02}")
}

fn format_task_kb(kb: u64, width: usize) -> String {
    const LIMITS: [f64; 8] = [
        1.0,
        10.0,
        100.0,
        1_000.0,
        10_000.0,
        100_000.0,
        1_000_000.0,
        10_000_000.0,
    ];
    let mut value = kb as f32;
    if f64::from(value).round_ties_even() < LIMITS[width] {
        return format!("{value:.0}");
    }
    for suffix in ['m', 'g', 't', 'p'] {
        value /= 1024.0;
        if width >= 4 && (f64::from(value) * 10.0).round_ties_even() < LIMITS[width - 3] * 10.0 {
            return format!("{value:.1}{suffix}");
        }
    }
    "?".into()
}

fn format_user(user: &str, width: usize) -> Cow<'_, str> {
    if width == 0 {
        return Cow::Borrowed("");
    }
    let mut chars = user.char_indices();
    let marker = chars.nth(width - 1).map(|(offset, _)| offset);
    if chars.next().is_none() {
        Cow::Borrowed(user)
    } else {
        Cow::Owned(format!("{}+", &user[..marker.unwrap()]))
    }
}

fn format_uptime(secs: f64) -> String {
    let total = secs as u64;
    let days = total / 86400;
    let hours = (total % 86400) / 3600;
    let mins = (total % 3600) / 60;

    if days > 0 {
        format!(
            "up {days} day{}, {hours:2}:{mins:02}",
            if days == 1 { "" } else { "s" }
        )
    } else if hours > 0 {
        format!("up {hours:2}:{mins:02}")
    } else {
        format!("up {mins} min")
    }
}

fn count_users() -> usize {
    utmp::read(utmp::DEFAULT_UTMP_PATH)
        .map(|entries| utmp::count_user_processes(&entries))
        .unwrap_or(0)
}

struct CpuDelta {
    user: f64,
    nice: f64,
    system: f64,
    idle: f64,
    iowait: f64,
    irq: f64,
    softirq: f64,
    steal: f64,
}

fn cpu_delta(prev: &procfs::CpuTime, cur: &procfs::CpuTime) -> CpuDelta {
    let d = |c: u64, p: u64| c.saturating_sub(p) as f64;
    let user = d(cur.user, prev.user);
    let nice = d(cur.nice, prev.nice);
    let system = d(cur.system, prev.system);
    let idle = d(cur.idle, prev.idle);
    let iowait = d(cur.iowait.unwrap_or(0), prev.iowait.unwrap_or(0));
    let irq = d(cur.irq.unwrap_or(0), prev.irq.unwrap_or(0));
    let softirq = d(cur.softirq.unwrap_or(0), prev.softirq.unwrap_or(0));
    let steal = d(cur.steal.unwrap_or(0), prev.steal.unwrap_or(0));

    let total = user + nice + system + idle + iowait + irq + softirq + steal;
    if total == 0.0 {
        return CpuDelta {
            user: 0.0,
            nice: 0.0,
            system: 0.0,
            idle: 100.0,
            iowait: 0.0,
            irq: 0.0,
            softirq: 0.0,
            steal: 0.0,
        };
    }

    let pct = |v: f64| v / total * 100.0;
    CpuDelta {
        user: pct(user),
        nice: pct(nice),
        system: pct(system),
        idle: pct(idle),
        iowait: pct(iowait),
        irq: pct(irq),
        softirq: pct(softirq),
        steal: pct(steal),
    }
}

fn cpu_pct_cumulative(ct: &procfs::CpuTime) -> CpuDelta {
    let user = ct.user as f64;
    let nice = ct.nice as f64;
    let system = ct.system as f64;
    let idle = ct.idle as f64;
    let iowait = ct.iowait.unwrap_or(0) as f64;
    let irq = ct.irq.unwrap_or(0) as f64;
    let softirq = ct.softirq.unwrap_or(0) as f64;
    let steal = ct.steal.unwrap_or(0) as f64;

    let total = user + nice + system + idle + iowait + irq + softirq + steal;
    if total == 0.0 {
        return CpuDelta {
            user: 0.0,
            nice: 0.0,
            system: 0.0,
            idle: 100.0,
            iowait: 0.0,
            irq: 0.0,
            softirq: 0.0,
            steal: 0.0,
        };
    }

    let pct = |v: f64| v / total * 100.0;
    CpuDelta {
        user: pct(user),
        nice: pct(nice),
        system: pct(system),
        idle: pct(idle),
        iowait: pct(iowait),
        irq: pct(irq),
        softirq: pct(softirq),
        steal: pct(steal),
    }
}

fn cpu_time_total_ticks(ct: &procfs::CpuTime) -> u64 {
    ct.user
        + ct.nice
        + ct.system
        + ct.idle
        + ct.iowait.unwrap_or(0)
        + ct.irq.unwrap_or(0)
        + ct.softirq.unwrap_or(0)
        + ct.steal.unwrap_or(0)
}

struct App {
    processes: Vec<ProcessInfo>,
    sort_field: SortField,
    sort_reverse: bool,
    delay: Duration,
    table_state: TableState,
    show_full_cmd: bool,
    show_per_cpu: bool,

    prev_kernel: Option<procfs::KernelStats>,
    prev_proc_times: HashMap<i32, u64>,

    uid_cache: UidCache,
    total_mem_kb: u64,
    tps: u64,
    num_cpus: usize,

    pid_filter: Option<Vec<i32>>,
    uid_filter: Option<u32>,

    iterations_remaining: Option<u64>,

    // Cached summary data from latest refresh
    uptime_secs: f64,
    load_avg: [f64; 3],
    task_total: usize,
    task_running: usize,
    task_sleeping: usize,
    task_disk_sleep: usize,
    task_stopped: usize,
    task_zombie: usize,
    cpu_deltas: Vec<CpuDelta>,
    mem_total_kb: u64,
    mem_free_kb: u64,
    mem_used_kb: u64,
    mem_bufcache_kb: u64,
    swap_total_kb: u64,
    swap_free_kb: u64,
    swap_used_kb: u64,
    mem_avail_kb: u64,
}

impl App {
    fn new(args: &Args) -> Self {
        let tps = procfs::ticks_per_second();

        let uid_filter = args.user.as_ref().and_then(|u| {
            u.parse::<u32>()
                .ok()
                .or_else(|| procutils_common::uid::resolve_uid(u))
        });

        Self {
            processes: Vec::new(),
            sort_field: SortField::Cpu,
            sort_reverse: false,
            delay: Duration::from_secs_f64(args.delay.max(0.1)),
            table_state: TableState::default(),
            show_full_cmd: false,
            show_per_cpu: args.per_cpu,

            prev_kernel: None,
            prev_proc_times: HashMap::new(),

            uid_cache: UidCache::new(),
            total_mem_kb: 0,
            tps,
            num_cpus: 0,

            pid_filter: args.pid.clone(),
            uid_filter,

            iterations_remaining: args.iterations,

            uptime_secs: 0.0,
            load_avg: [0.0; 3],
            task_total: 0,
            task_running: 0,
            task_sleeping: 0,
            task_disk_sleep: 0,
            task_stopped: 0,
            task_zombie: 0,
            cpu_deltas: Vec::new(),
            mem_total_kb: 0,
            mem_free_kb: 0,
            mem_used_kb: 0,
            mem_bufcache_kb: 0,
            swap_total_kb: 0,
            swap_free_kb: 0,
            swap_used_kb: 0,
            mem_avail_kb: 0,
        }
    }

    fn refresh(&mut self) {
        // Read system-wide data
        let kernel = match procfs::KernelStats::current() {
            Ok(k) => k,
            Err(_) => return,
        };

        let meminfo = match procfs::Meminfo::current() {
            Ok(m) => m,
            Err(_) => return,
        };

        if let Ok(la) = procfs::LoadAverage::current() {
            self.load_avg = [la.one as f64, la.five as f64, la.fifteen as f64];
        }

        if let Ok(up) = procfs::Uptime::current() {
            self.uptime_secs = up.uptime;
        }

        // Memory info (procfs returns bytes)
        self.mem_total_kb = meminfo.mem_total / 1024;
        self.total_mem_kb = self.mem_total_kb;
        self.mem_free_kb = meminfo.mem_free / 1024;
        let buffers_kb = meminfo.buffers / 1024;
        let cached_kb = meminfo.cached / 1024;
        let sreclaimable_kb = meminfo.s_reclaimable.unwrap_or(0) / 1024;
        self.mem_bufcache_kb = buffers_kb + cached_kb + sreclaimable_kb;
        self.mem_used_kb = self.mem_total_kb.saturating_sub(
            meminfo
                .mem_available
                .map(|bytes| bytes / 1024)
                .unwrap_or(self.mem_free_kb.saturating_add(self.mem_bufcache_kb)),
        );
        self.swap_total_kb = meminfo.swap_total / 1024;
        self.swap_free_kb = meminfo.swap_free / 1024;
        self.swap_used_kb = self.swap_total_kb - self.swap_free_kb;
        self.mem_avail_kb = meminfo.mem_available.unwrap_or(meminfo.mem_free) / 1024;

        // CPU deltas
        self.num_cpus = kernel.cpu_time.len();
        self.cpu_deltas.clear();

        if let Some(ref prev) = self.prev_kernel {
            // Total CPU line
            self.cpu_deltas.push(cpu_delta(&prev.total, &kernel.total));
            // Per-CPU lines
            if self.show_per_cpu {
                for (i, cur_cpu) in kernel.cpu_time.iter().enumerate() {
                    if let Some(prev_cpu) = prev.cpu_time.get(i) {
                        self.cpu_deltas.push(cpu_delta(prev_cpu, cur_cpu));
                    }
                }
            }
        } else {
            // First sample: show cumulative percentages
            self.cpu_deltas.push(cpu_pct_cumulative(&kernel.total));
            if self.show_per_cpu {
                for cur_cpu in &kernel.cpu_time {
                    self.cpu_deltas.push(cpu_pct_cumulative(cur_cpu));
                }
            }
        }

        // Compute total CPU ticks delta for per-process %CPU
        let total_ticks_delta = if let Some(ref prev) = self.prev_kernel {
            cpu_time_total_ticks(&kernel.total).saturating_sub(cpu_time_total_ticks(&prev.total))
        } else {
            cpu_time_total_ticks(&kernel.total)
        };

        // Read processes
        let all_procs = match procfs::process::all_processes() {
            Ok(iter) => iter,
            Err(_) => {
                self.prev_kernel = Some(kernel);
                return;
            }
        };

        let mut new_processes = Vec::new();
        let mut new_proc_times = HashMap::new();
        let mut task_running = 0usize;
        let mut task_sleeping = 0usize;
        let mut task_disk_sleep = 0usize;
        let mut task_stopped = 0usize;
        let mut task_zombie = 0usize;

        for proc_result in all_procs {
            let proc = match proc_result {
                Ok(p) => p,
                Err(_) => continue,
            };

            let stat = match proc.stat() {
                Ok(s) => s,
                Err(_) => continue,
            };

            // PID filter
            if let Some(ref pids) = self.pid_filter
                && !pids.contains(&stat.pid)
            {
                continue;
            }

            let status = match proc.status() {
                Ok(s) => s,
                Err(_) => continue,
            };

            // User filter
            if let Some(uid) = self.uid_filter
                && status.euid != uid
            {
                continue;
            }

            // Count task states
            match stat.state {
                'R' => task_running += 1,
                'S' | 'I' => task_sleeping += 1,
                'D' => task_disk_sleep += 1,
                'T' | 't' => task_stopped += 1,
                'Z' => task_zombie += 1,
                _ => task_sleeping += 1,
            }

            let proc_ticks = stat.utime + stat.stime;
            new_proc_times.insert(stat.pid, proc_ticks);

            // Compute per-process CPU%
            let cpu_pct = if total_ticks_delta > 0 {
                let prev_ticks = self.prev_proc_times.get(&stat.pid).copied().unwrap_or(0);
                let delta = proc_ticks.saturating_sub(prev_ticks);
                // Scale by num_cpus so a single busy core = ~100%
                delta as f64 / total_ticks_delta as f64 * self.num_cpus.max(1) as f64 * 100.0
            } else {
                0.0
            };

            // Unlike Meminfo, Status reports these values in KiB.
            let res_kb = status.vmrss.unwrap_or(0);
            let shr_kb = status.rssfile.unwrap_or(0) + status.rssshmem.unwrap_or(0);
            let mem_pct = if self.total_mem_kb > 0 {
                res_kb as f64 / self.total_mem_kb as f64 * 100.0
            } else {
                0.0
            };

            let cmdline = proc.cmdline().ok().map(|v| v.join(" ")).unwrap_or_default();

            new_processes.push(ProcessInfo {
                pid: stat.pid,
                user: self.uid_cache.get(status.euid).to_string(),
                priority: stat.priority,
                nice: stat.nice,
                virt_kb: stat.vsize / 1024,
                res_kb,
                shr_kb,
                state: stat.state,
                cpu_pct,
                mem_pct,
                time_ticks: proc_ticks,
                command: stat.comm.clone(),
                cmdline,
            });
        }

        self.task_total = new_processes.len();
        self.task_running = task_running;
        self.task_sleeping = task_sleeping;
        self.task_disk_sleep = task_disk_sleep;
        self.task_stopped = task_stopped;
        self.task_zombie = task_zombie;

        // Sort
        sort_processes(&mut new_processes, self.sort_field, self.sort_reverse);

        self.processes = new_processes;
        self.prev_kernel = Some(kernel);
        self.prev_proc_times = new_proc_times;
    }

    fn scroll_up(&mut self, n: u16) {
        let offset = self.table_state.offset_mut();
        *offset = offset.saturating_sub(n as usize);
    }

    fn scroll_down(&mut self, n: u16) {
        let offset = self.table_state.offset_mut();
        *offset = offset.saturating_add(n as usize);
    }

    fn scroll_home(&mut self) {
        *self.table_state.offset_mut() = 0;
    }

    fn scroll_end(&mut self) {
        *self.table_state.offset_mut() = self.processes.len().saturating_sub(1);
    }

    fn summary_height(&self) -> u16 {
        // Line 1: uptime, Line 2: tasks, Line 3+: CPU, Line N-1: Mem, Line N: Swap
        // Plus 1 for the border
        let cpu_lines = if self.show_per_cpu {
            self.num_cpus.max(1) as u16
        } else {
            1
        };
        cpu_lines + 5
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let summary_h = self.summary_height();

        let chunks =
            Layout::vertical([Constraint::Length(summary_h), Constraint::Min(3)]).split(area);

        self.draw_summary(frame, chunks[0]);
        self.draw_table(frame, chunks[1]);
    }

    fn draw_summary(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let label = Style::default().fg(Color::Cyan);
        let mut lines = Vec::new();

        // Line 1: top - HH:MM:SS up X days, HH:MM, N users, load average: ...
        let now = chrono_free_time();
        let users = count_users();
        lines.push(Line::from(vec![
            Span::styled(" top - ", label),
            Span::raw(format!(
                "{now}  {}, {users} user{}, load average: {:.2}, {:.2}, {:.2}",
                format_uptime(self.uptime_secs),
                if users == 1 { "" } else { "s" },
                self.load_avg[0],
                self.load_avg[1],
                self.load_avg[2],
            )),
        ]));

        // Line 2: Tasks
        lines.push(Line::from(vec![
            Span::styled(" Tasks: ", label),
            Span::raw(format!(
                "{} total, {} running, {} sleep, {} d-sleep, {} stopped, {} zombie",
                self.task_total,
                self.task_running,
                self.task_sleeping,
                self.task_disk_sleep,
                self.task_stopped,
                self.task_zombie,
            )),
        ]));

        // CPU lines
        for (i, d) in self.cpu_deltas.iter().enumerate() {
            let cpu_label = if i == 0 && !self.show_per_cpu {
                " %Cpu(s): ".to_string()
            } else if i == 0 && self.show_per_cpu {
                // Skip the aggregate line when showing per-cpu
                continue;
            } else {
                format!(" %Cpu{:>2}: ", i - 1)
            };

            lines.push(Line::from(vec![
                Span::styled(cpu_label, label),
                Span::raw(format!(
                    "{:5.1} us, {:5.1} sy, {:5.1} ni, {:5.1} id, {:5.1} wa, {:5.1} hi, {:5.1} si, {:5.1} st",
                    d.user, d.system, d.nice, d.idle, d.iowait, d.irq, d.softirq, d.steal,
                )),
            ]));
        }

        // When per_cpu is on but we have no deltas yet, show a placeholder
        if self.cpu_deltas.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(" %Cpu(s): ", label),
                Span::raw("  0.0 us,   0.0 sy,   0.0 ni, 100.0 id,   0.0 wa,   0.0 hi,   0.0 si,   0.0 st"),
            ]));
        }

        // Memory line
        let mib = |kb: u64| kb as f64 / 1024.0;
        lines.push(Line::from(vec![
            Span::styled(" MiB Mem: ", label),
            Span::raw(format!(
                "{:10.1} total, {:10.1} free, {:10.1} used, {:10.1} buff/cache",
                mib(self.mem_total_kb),
                mib(self.mem_free_kb),
                mib(self.mem_used_kb),
                mib(self.mem_bufcache_kb),
            )),
        ]));

        // Swap line
        lines.push(Line::from(vec![
            Span::styled(" MiB Swap:", label),
            Span::raw(format!(
                "{:10.1} total, {:10.1} free, {:10.1} used. {:10.1} avail Mem",
                mib(self.swap_total_kb),
                mib(self.swap_free_kb),
                mib(self.swap_used_kb),
                mib(self.mem_avail_kb),
            )),
        ]));

        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::BOTTOM)),
            area,
        );
    }

    fn draw_table(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let header_style = Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED);

        let sort_indicator = |field: SortField| {
            if self.sort_field == field {
                if self.sort_reverse { " ^" } else { " v" }
            } else {
                ""
            }
        };

        let header = Row::new(vec![
            Cell::from(format!("  PID{}", sort_indicator(SortField::Pid))),
            Cell::from("USER"),
            Cell::from(" PR"),
            Cell::from("  NI"),
            Cell::from("    VIRT"),
            Cell::from("     RES"),
            Cell::from("     SHR"),
            Cell::from("S"),
            Cell::from(format!(" %CPU{}", sort_indicator(SortField::Cpu))),
            Cell::from(format!(" %MEM{}", sort_indicator(SortField::Mem))),
            Cell::from(format!("    TIME+{}", sort_indicator(SortField::Time))),
            Cell::from("COMMAND"),
        ])
        .style(header_style);

        let rows: Vec<Row> = self
            .processes
            .iter()
            .map(|p| {
                let cmd = if p.cmdline.is_empty() {
                    // Kernel thread — only comm is available
                    &p.command
                } else if self.show_full_cmd {
                    &p.cmdline
                } else {
                    &p.command
                };

                let state_color = match p.state {
                    'R' => Color::Green,
                    'Z' => Color::Red,
                    'T' | 't' => Color::Yellow,
                    _ => Color::default(),
                };

                Row::new(vec![
                    Cell::from(format!("{:>5}", p.pid)),
                    Cell::from(format!("{:<8}", format_user(&p.user, 8))),
                    Cell::from(format!(
                        "{:>3}",
                        if p.priority < -99 {
                            "rt".to_string()
                        } else {
                            p.priority.to_string()
                        }
                    )),
                    Cell::from(format!("{:>4}", p.nice)),
                    Cell::from(format!("{:>8}", format_kb(p.virt_kb))),
                    Cell::from(format!("{:>8}", format_kb(p.res_kb))),
                    Cell::from(format!("{:>8}", format_kb(p.shr_kb))),
                    Cell::from(format!("{}", p.state)).style(Style::default().fg(state_color)),
                    Cell::from(format!("{:>5.1}", p.cpu_pct)),
                    Cell::from(format!("{:>5.1}", p.mem_pct)),
                    Cell::from(format!("{:>9}", format_time_plus(p.time_ticks, self.tps))),
                    Cell::from(cmd.to_string()),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(5),
            Constraint::Length(8),
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(1),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(9),
            Constraint::Fill(1),
        ];

        let table = Table::new(rows, widths).header(header).column_spacing(1);

        frame.render_stateful_widget(table, area, &mut self.table_state);
    }
}

fn sort_processes(procs: &mut [ProcessInfo], sort_field: SortField, sort_reverse: bool) {
    let cmp = |a: &ProcessInfo, b: &ProcessInfo| -> std::cmp::Ordering {
        match sort_field {
            SortField::Cpu => b
                .cpu_pct
                .partial_cmp(&a.cpu_pct)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortField::Mem => b
                .mem_pct
                .partial_cmp(&a.mem_pct)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortField::Pid => a.pid.cmp(&b.pid),
            SortField::Time => b.time_ticks.cmp(&a.time_ticks),
        }
    };

    if sort_reverse {
        procs.sort_by(|a, b| cmp(a, b).reverse());
    } else {
        procs.sort_by(cmp);
    }
}

fn chrono_free_time() -> String {
    // Get current time without chrono dependency
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple UTC-to-local is hard without libc, so read /etc/localtime offset
    // from procfs. For simplicity, just format as UTC-based local using libc-free approach.
    // Actually, read from /proc/self: use the same trick as coreutils.
    // Simplest: use the elapsed seconds and compute HH:MM:SS.
    let secs_in_day = ts % 86400;

    // Try to get timezone offset from TZ or default to reading /etc/localtime
    // For simplicity, use a basic approach:
    let offset = local_tz_offset();
    let local_secs = (secs_in_day as i64 + offset).rem_euclid(86400) as u64;

    let h = local_secs / 3600;
    let m = (local_secs % 3600) / 60;
    let s = local_secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn local_tz_offset() -> i64 {
    // Try TZ environment variable first, then /etc/localtime
    // For a simple implementation, parse /etc/timezone or use a fixed heuristic
    // This is a best-effort approach — top doesn't need perfect timezone handling
    if let Ok(tz) = std::env::var("TZ")
        && let Some(offset) = parse_posix_tz_offset(&tz)
    {
        return offset;
    }

    // Read the TZif file at /etc/localtime
    if let Ok(data) = std::fs::read("/etc/localtime")
        && let Some(offset) = parse_tzif_current_offset(&data)
    {
        return offset;
    }

    0 // fallback to UTC
}

fn parse_posix_tz_offset(tz: &str) -> Option<i64> {
    // Handle simple POSIX TZ like "EST5EDT" or "UTC" or "EST-5"
    // Skip alphabetic prefix
    let rest = tz.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    if rest.is_empty() {
        return Some(0);
    }
    // Parse offset hours (note: POSIX TZ offset sign is inverted)
    let hours: i64 = rest
        .split(|c: char| !c.is_ascii_digit() && c != '-' && c != '+')
        .next()?
        .parse()
        .ok()?;
    Some(-hours * 3600)
}

fn parse_tzif_current_offset(data: &[u8]) -> Option<i64> {
    // Minimal TZif parser — just get the last transition's UTC offset
    if data.len() < 44 || &data[0..4] != b"TZif" {
        return None;
    }

    // Check for TZif2/3 (version byte at offset 4)
    let version = data[4];

    if version == b'2' || version == b'3' {
        // Skip v1 data block to find v2/v3 header
        let tzh_ttisutcnt = u32::from_be_bytes(data[20..24].try_into().ok()?) as usize;
        let tzh_ttisstdcnt = u32::from_be_bytes(data[24..28].try_into().ok()?) as usize;
        let tzh_leapcnt = u32::from_be_bytes(data[28..32].try_into().ok()?) as usize;
        let tzh_timecnt = u32::from_be_bytes(data[32..36].try_into().ok()?) as usize;
        let tzh_typecnt = u32::from_be_bytes(data[36..40].try_into().ok()?) as usize;
        let tzh_charcnt = u32::from_be_bytes(data[40..44].try_into().ok()?) as usize;

        let v1_datablock_size = tzh_timecnt * 4
            + tzh_timecnt
            + tzh_typecnt * 6
            + tzh_charcnt
            + tzh_leapcnt * 8
            + tzh_ttisstdcnt
            + tzh_ttisutcnt;

        let v2_header_start = 44 + v1_datablock_size;
        if data.len() < v2_header_start + 44 {
            return None;
        }

        return parse_tzif_v2(data, v2_header_start);
    }

    // v1: parse directly
    parse_tzif_v1(data)
}

fn parse_tzif_v1(data: &[u8]) -> Option<i64> {
    let tzh_timecnt = u32::from_be_bytes(data[32..36].try_into().ok()?) as usize;
    let tzh_typecnt = u32::from_be_bytes(data[36..40].try_into().ok()?) as usize;

    if tzh_typecnt == 0 {
        return None;
    }

    let times_start = 44;
    let types_start = times_start + tzh_timecnt * 4;
    let ttinfos_start = types_start + tzh_timecnt;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Find the most recent transition before now
    let mut type_idx = 0u8;
    for i in (0..tzh_timecnt).rev() {
        let offset = times_start + i * 4;
        if data.len() < offset + 4 {
            continue;
        }
        let trans_time = i32::from_be_bytes(data[offset..offset + 4].try_into().ok()?) as i64;
        if trans_time <= now {
            type_idx = data[types_start + i];
            break;
        }
    }

    // Read the ttinfo for this type
    let ttinfo_offset = ttinfos_start + type_idx as usize * 6;
    if data.len() < ttinfo_offset + 6 {
        return None;
    }
    let utoff = i32::from_be_bytes(data[ttinfo_offset..ttinfo_offset + 4].try_into().ok()?);
    Some(utoff as i64)
}

fn parse_tzif_v2(data: &[u8], header_start: usize) -> Option<i64> {
    let h = header_start;
    if data.len() < h + 44 || &data[h..h + 4] != b"TZif" {
        return None;
    }

    let _tzh_leapcnt = u32::from_be_bytes(data[h + 28..h + 32].try_into().ok()?) as usize;
    let tzh_timecnt = u32::from_be_bytes(data[h + 32..h + 36].try_into().ok()?) as usize;
    let tzh_typecnt = u32::from_be_bytes(data[h + 36..h + 40].try_into().ok()?) as usize;

    if tzh_typecnt == 0 {
        return None;
    }

    let times_start = h + 44;
    let types_start = times_start + tzh_timecnt * 8; // v2 uses 8-byte timestamps
    let ttinfos_start = types_start + tzh_timecnt;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let mut type_idx = 0u8;
    for i in (0..tzh_timecnt).rev() {
        let offset = times_start + i * 8;
        if data.len() < offset + 8 {
            continue;
        }
        let trans_time = i64::from_be_bytes(data[offset..offset + 8].try_into().ok()?);
        if trans_time <= now {
            type_idx = data[types_start + i];
            break;
        }
    }

    let ttinfo_offset = ttinfos_start + type_idx as usize * 6;
    if data.len() < ttinfo_offset + 6 {
        return None;
    }
    let utoff = i32::from_be_bytes(data[ttinfo_offset..ttinfo_offset + 4].try_into().ok()?);
    Some(utoff as i64)
}

fn print_batch_summary(app: &App) {
    let now = chrono_free_time();
    let users = count_users();

    println!(
        "top - {} {}, {:>2} user{},  load average: {:.2}, {:.2}, {:.2}",
        now,
        format_uptime(app.uptime_secs),
        users,
        if users == 1 { "" } else { "s" },
        app.load_avg[0],
        app.load_avg[1],
        app.load_avg[2],
    );

    println!(
        "Tasks: {} total, {} running, {} sleep, {} d-sleep, {} stopped, {} zombie",
        app.task_total,
        app.task_running,
        app.task_sleeping,
        app.task_disk_sleep,
        app.task_stopped,
        app.task_zombie,
    );

    for (i, d) in app.cpu_deltas.iter().enumerate() {
        let label = if i == 0 && !app.show_per_cpu {
            "%Cpu(s):".to_string()
        } else if i == 0 && app.show_per_cpu {
            continue;
        } else {
            format!("%Cpu{:>2}:", i - 1)
        };
        println!(
            "{label} {:4.1} us, {:4.1} sy, {:4.1} ni, {:4.1} id, {:4.1} wa, {:4.1} hi, {:4.1} si, {:4.1} st ",
            d.user, d.system, d.nice, d.idle, d.iowait, d.irq, d.softirq, d.steal,
        );
    }

    let mib = |kb: u64| kb as f64 / 1024.0;
    println!(
        "MiB Mem : {:8.1} total, {:8.1} free, {:8.1} used, {:8.1} buff/cache     ",
        mib(app.mem_total_kb),
        mib(app.mem_free_kb),
        mib(app.mem_used_kb),
        mib(app.mem_bufcache_kb),
    );
    println!(
        "MiB Swap: {:8.1} total, {:8.1} free, {:8.1} used. {:8.1} avail Mem ",
        mib(app.swap_total_kb),
        mib(app.swap_free_kb),
        mib(app.swap_used_kb),
        mib(app.mem_avail_kb),
    );
    println!();

    println!("    PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND",);

    for p in &app.processes {
        let pr = if p.priority < -99 {
            "rt".to_string()
        } else {
            p.priority.to_string()
        };
        println!(
            "{:>7} {:<8} {:>3} {:>3} {:>7} {:>6} {:>6} {} {:>5.1} {:>5.1} {:>9} {}",
            p.pid,
            format_user(&p.user, 8),
            pr,
            p.nice,
            format_task_kb(p.virt_kb, 7),
            format_task_kb(p.res_kb, 6),
            format_task_kb(p.shr_kb, 6),
            p.state,
            p.cpu_pct,
            p.mem_pct,
            format_time_plus(p.time_ticks, app.tps),
            if p.cmdline.is_empty() {
                &p.command
            } else if app.show_full_cmd {
                &p.cmdline
            } else {
                &p.command
            },
        );
    }

    println!();
}

fn run_batch(args: &Args) -> ExitCode {
    let mut app = App::new(args);
    let mut iteration = 0u64;

    loop {
        if let Some(max) = app.iterations_remaining
            && iteration >= max
        {
            break;
        }

        if iteration > 0 {
            std::thread::sleep(app.delay);
        }

        app.refresh();
        print_batch_summary(&app);
        let _ = std::io::stdout().flush();

        iteration += 1;
    }

    ExitCode::SUCCESS
}

pub fn run(args: Args) -> ExitCode {
    if args.batch {
        return run_batch(&args);
    }

    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture).ok();

    let mut terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("top: failed to initialize terminal: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = run_app(&mut terminal, &args);

    ratatui::restore();
    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture).ok();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("top: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_app(terminal: &mut DefaultTerminal, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new(args);
    let mut iteration = 0u64;
    let mut needs_refresh = true;

    loop {
        if let Some(max) = app.iterations_remaining
            && iteration >= max
        {
            return Ok(());
        }

        if needs_refresh {
            app.refresh();
            iteration += 1;
            needs_refresh = false;
        }

        terminal.draw(|frame| app.draw(frame))?;

        if event::poll(app.delay)? {
            // Drain all pending events before redrawing, so queued
            // mouse moves don't each trigger an expensive /proc rescan.
            let mut needs_redraw = false;
            while event::poll(Duration::ZERO)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => {
                            return Ok(());
                        }
                        KeyCode::Char(' ') => {
                            needs_refresh = true;
                            break;
                        }
                        KeyCode::Char('P') => {
                            app.sort_field = SortField::Cpu;
                            needs_redraw = true;
                        }
                        KeyCode::Char('M') => {
                            app.sort_field = SortField::Mem;
                            needs_redraw = true;
                        }
                        KeyCode::Char('N') => {
                            app.sort_field = SortField::Pid;
                            needs_redraw = true;
                        }
                        KeyCode::Char('T') => {
                            app.sort_field = SortField::Time;
                            needs_redraw = true;
                        }
                        KeyCode::Char('R') => {
                            app.sort_reverse = !app.sort_reverse;
                            needs_redraw = true;
                        }
                        KeyCode::Char('c') => {
                            app.show_full_cmd = !app.show_full_cmd;
                            needs_redraw = true;
                        }
                        KeyCode::Char('1') => {
                            app.show_per_cpu = !app.show_per_cpu;
                            needs_refresh = true;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.scroll_up(1);
                            needs_redraw = true;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.scroll_down(1);
                            needs_redraw = true;
                        }
                        KeyCode::PageUp => {
                            app.scroll_up(20);
                            needs_redraw = true;
                        }
                        KeyCode::PageDown => {
                            app.scroll_down(20);
                            needs_redraw = true;
                        }
                        KeyCode::Home => {
                            app.scroll_home();
                            needs_redraw = true;
                        }
                        KeyCode::End => {
                            app.scroll_end();
                            needs_redraw = true;
                        }
                        _ => {}
                    },
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            app.scroll_up(3);
                            needs_redraw = true;
                        }
                        MouseEventKind::ScrollDown => {
                            app.scroll_down(3);
                            needs_redraw = true;
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }

            // Re-sort if sort changed, without a full /proc rescan
            if needs_redraw {
                let sort_field = app.sort_field;
                let sort_reverse = app.sort_reverse;
                sort_processes(&mut app.processes, sort_field, sort_reverse);
            }
        } else {
            // Timer expired — time for a fresh /proc read
            needs_refresh = true;
        }
    }
}
