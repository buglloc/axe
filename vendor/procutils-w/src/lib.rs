use chrono::{DateTime, Local};
use clap::Parser;
use procfs::prelude::*;
use procutils_common::{MAX_TERM_WIDTH, man::ManContent, utmp};
use std::{
    collections::HashMap,
    os::linux::fs::MetadataExt as _,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, SystemTime},
};

pub const MAN: ManContent = ManContent {
    description: Some(include_str!("../man/description.man")),
    extra_sections: &[
        (
            "FIELD DESCRIPTIONS",
            include_str!("../man/field_descriptions.man"),
        ),
        ("EXAMPLES", include_str!("../man/examples.man")),
        ("ENVIRONMENT", include_str!("../man/environment.man")),
        ("NOTES", include_str!("../man/notes.man")),
        ("SEE ALSO", include_str!("../man/see_also.man")),
    ],
};

/// Show who is logged on and what they are doing.
#[derive(Parser)]
#[command(name = "w", version, about, disable_help_flag = true, max_term_width = MAX_TERM_WIDTH)]
pub struct Args {
    /// Show information for the specified user only.
    user: Option<String>,

    /// Print help.
    #[arg(long, action = clap::ArgAction::HelpLong)]
    help: Option<bool>,

    /// Don't print the header line.
    #[arg(short = 'h', long = "no-header")]
    no_header: bool,

    /// Don't try to identify the foreground process — only consult the
    /// utmp entry's own pid. Matches `w(1)`.
    #[arg(short = 'u', long = "no-current")]
    no_current: bool,

    /// Short format: omit LOGIN@, JCPU and PCPU columns.
    #[arg(short, long)]
    short: bool,

    /// Show the FROM (remote host or display) column.
    #[arg(short, long)]
    from: bool,

    /// Show the IP address rather than the hostname in the FROM column.
    #[arg(short = 'i', long = "ip-addr")]
    ip_addr: bool,

    /// Show PIDs in the WHAT column as `loginpid/whatpid`.
    #[arg(short, long)]
    pids: bool,

    /// Old-style output: blank IDLE for less than one minute.
    #[arg(short, long)]
    old_style: bool,
}

pub fn run(args: Args) -> ExitCode {
    let entries = match utmp::read(utmp::DEFAULT_UTMP_PATH) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!("w: cannot read {}: {e}", utmp::DEFAULT_UTMP_PATH);
            return ExitCode::FAILURE;
        }
    };

    let users: Vec<_> = entries
        .into_iter()
        .filter(|e| e.ut_type == utmp::USER_PROCESS)
        .filter(|e| match &args.user {
            Some(u) => e.user == *u,
            None => true,
        })
        .collect();

    if !args.no_header {
        print_header(users.len());
        print_column_header(&args);
    }

    if users.is_empty() {
        return ExitCode::SUCCESS;
    }

    let proc_index = ProcIndex::build();

    for entry in &users {
        print_user_row(entry, &args, &proc_index);
    }

    ExitCode::SUCCESS
}

fn print_header(n_users: usize) {
    let now = Local::now().format("%H:%M:%S");
    let uptime_secs = procfs::Uptime::current().map(|u| u.uptime).unwrap_or(0.0);
    let load = procfs::LoadAverage::current()
        .map(|l| (l.one as f64, l.five as f64, l.fifteen as f64))
        .unwrap_or((0.0, 0.0, 0.0));

    println!(
        " {now} up {uptime},  {n_users} user{plural},  load average: {:.2}, {:.2}, {:.2}",
        load.0,
        load.1,
        load.2,
        uptime = format_uptime(uptime_secs),
        plural = if n_users == 1 { "" } else { "s" },
    );
}

fn format_uptime(secs: f64) -> String {
    let total = secs as u64;
    let days = total / 86400;
    let hours = (total % 86400) / 3600;
    let mins = (total % 3600) / 60;
    if days > 0 {
        format!(
            "{} {}, {:2}:{:02}",
            days,
            if days == 1 { "day" } else { "days" },
            hours,
            mins,
        )
    } else if hours > 0 {
        format!("{hours}:{mins:02}")
    } else {
        format!("{mins} min")
    }
}

fn print_column_header(args: &Args) {
    let user_w = userlen();
    let from_w = fromlen();
    if args.from {
        if args.short {
            println!(
                "{:<user_w$} TTY      {:<from_w$}   IDLE WHAT",
                "USER", "FROM",
            );
        } else {
            println!(
                "{:<user_w$} TTY      {:<from_w$} LOGIN@   IDLE   JCPU   PCPU  WHAT",
                "USER", "FROM",
            );
        }
    } else if args.short {
        println!("{:<user_w$} TTY         IDLE WHAT", "USER");
    } else {
        println!(
            "{:<user_w$} TTY       LOGIN@   IDLE   JCPU   PCPU  WHAT",
            "USER",
        );
    }
}

fn userlen() -> usize {
    std::env::var("PROCPS_USERLEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8)
}

fn fromlen() -> usize {
    std::env::var("PROCPS_FROMLEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(16)
}

/// Index of /proc snapshots by tty device number and by pid, so we can
/// look up jcpu / foreground process / login pid info without rescanning
/// /proc per utmp entry.
struct ProcIndex {
    by_tty: HashMap<i32, Vec<ProcInfo>>,
    by_pid: HashMap<i32, ProcInfo>,
    tps: u64,
}

#[derive(Clone)]
struct ProcInfo {
    pid: i32,
    tpgid: i32,
    cpu_ticks: u64,
    comm: String,
    cmdline: String,
}

impl ProcIndex {
    fn build() -> Self {
        let tps = procfs::ticks_per_second();
        let mut by_tty: HashMap<i32, Vec<ProcInfo>> = HashMap::new();
        let mut by_pid: HashMap<i32, ProcInfo> = HashMap::new();

        if let Ok(iter) = procfs::process::all_processes() {
            for proc in iter.flatten() {
                let stat = match proc.stat() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let cmdline = proc.cmdline().ok().map(|v| v.join(" ")).unwrap_or_default();

                let info = ProcInfo {
                    pid: stat.pid,
                    tpgid: stat.tpgid,
                    cpu_ticks: stat.utime + stat.stime,
                    comm: stat.comm.clone(),
                    cmdline,
                };

                if stat.tty_nr != 0 {
                    by_tty.entry(stat.tty_nr).or_default().push(info.clone());
                }
                by_pid.insert(stat.pid, info);
            }
        }

        Self {
            by_tty,
            by_pid,
            tps,
        }
    }
}

fn print_user_row(entry: &utmp::UtmpEntry, args: &Args, idx: &ProcIndex) {
    let user_w = userlen();
    let from_w = fromlen();

    let tty_path: Option<PathBuf> = if entry.line.is_empty() {
        None
    } else {
        Some(PathBuf::from("/dev").join(&entry.line))
    };

    // Resolve the controlling tty's tty_nr (matches /proc/[pid]/stat tty_nr).
    let tty_nr = tty_path.as_ref().and_then(|p| {
        std::fs::metadata(p).ok().map(|m| {
            let rdev = m.st_rdev();
            // tty_nr in /proc/.../stat uses the "new_encode_dev" formula:
            //   ((major & 0xfff) << 8) | (minor & 0xff) | ((minor & 0xffffff00) << 12)
            let major = ((rdev >> 8) & 0xfff) as i32;
            let minor_lo = (rdev & 0xff) as i32;
            let minor_hi = ((rdev >> 12) & 0xffffff00) as i32;
            (major << 8) | minor_lo | minor_hi
        })
    });

    // Pick the "current" foreground process on the tty.
    let (jcpu_ticks, pcpu_proc) = match tty_nr.and_then(|n| idx.by_tty.get(&n)) {
        Some(procs) => {
            let jcpu: u64 = procs.iter().map(|p| p.cpu_ticks).sum();
            let tpgid = procs.iter().map(|p| p.tpgid).max().unwrap_or(-1);
            let fg = if args.no_current {
                idx.by_pid.get(&entry.pid).cloned()
            } else {
                procs
                    .iter()
                    .find(|p| p.pid == tpgid)
                    .cloned()
                    .or_else(|| idx.by_pid.get(&entry.pid).cloned())
            };
            (jcpu, fg)
        }
        None => {
            let p = idx.by_pid.get(&entry.pid).cloned();
            let jcpu = p.as_ref().map(|p| p.cpu_ticks).unwrap_or(0);
            (jcpu, p)
        }
    };

    let idle = compute_idle(tty_path.as_deref());
    let login_at = format_login_time(entry.login_time);
    let jcpu_str = format_cpu_ticks(jcpu_ticks, idx.tps);
    let pcpu_str = match &pcpu_proc {
        Some(p) => format_cpu_ticks(p.cpu_ticks, idx.tps),
        None => "?".to_string(),
    };
    let idle_str = format_idle(idle, args.old_style);
    let what = format_what(entry, &pcpu_proc, args.pids);
    let from = format_from(&entry.host, args.ip_addr);

    if args.from {
        if args.short {
            println!(
                "{:<user_w$} {:<8} {:<from_w$} {:>6} {what}",
                truncate(&entry.user, user_w),
                truncate(&entry.line, 8),
                truncate(&from, from_w),
                idle_str,
            );
        } else {
            println!(
                "{:<user_w$} {:<8} {:<from_w$} {:<7} {:>6} {:>6} {:>6} {what}",
                truncate(&entry.user, user_w),
                truncate(&entry.line, 8),
                truncate(&from, from_w),
                login_at,
                idle_str,
                jcpu_str,
                pcpu_str,
            );
        }
    } else if args.short {
        println!(
            "{:<user_w$} {:<8}  {:>6} {what}",
            truncate(&entry.user, user_w),
            truncate(&entry.line, 8),
            idle_str,
        );
    } else {
        println!(
            "{:<user_w$} {:<8}  {:<7} {:>6} {:>6} {:>6} {what}",
            truncate(&entry.user, user_w),
            truncate(&entry.line, 8),
            login_at,
            idle_str,
            jcpu_str,
            pcpu_str,
        );
    }
}

fn compute_idle(tty_path: Option<&std::path::Path>) -> Option<Duration> {
    let path = tty_path?;
    let meta = std::fs::metadata(path).ok()?;
    let atime = meta.accessed().ok()?;
    let mtime = meta.modified().ok()?;
    let last = atime.max(mtime);
    SystemTime::now().duration_since(last).ok()
}

fn format_login_time(t: SystemTime) -> String {
    let dt: DateTime<Local> = t.into();
    let now = Local::now();
    let age_secs = now.signed_duration_since(dt).num_seconds().max(0);

    if dt.date_naive() == now.date_naive() {
        dt.format("%H:%M").to_string()
    } else if age_secs < 7 * 86400 {
        dt.format("%a%H").to_string()
    } else {
        dt.format("%d%b%y").to_string()
    }
}

fn format_idle(d: Option<Duration>, old_style: bool) -> String {
    let Some(d) = d else {
        return "?".to_string();
    };
    if old_style && d.as_secs() < 60 {
        return String::new();
    }
    format_duration_short(d)
}

fn format_cpu_ticks(ticks: u64, tps: u64) -> String {
    if tps == 0 {
        return "0.00s".to_string();
    }
    let total_secs = ticks as f64 / tps as f64;
    format_duration_short(Duration::from_secs_f64(total_secs))
}

fn format_duration_short(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        let hundredths = d.subsec_millis() / 10;
        format!("{}.{:02}s", secs, hundredths)
    } else if secs < 3600 {
        format!("{}:{:02}", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}:{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}days", secs / 86400)
    }
}

fn format_what(entry: &utmp::UtmpEntry, fg: &Option<ProcInfo>, show_pids: bool) -> String {
    let (pid, label) = match fg {
        Some(p) => {
            let label = if p.cmdline.is_empty() {
                p.comm.clone()
            } else {
                p.cmdline.clone()
            };
            (p.pid, label)
        }
        None => (entry.pid, "-".to_string()),
    };
    if show_pids {
        format!("{}/{} {}", entry.pid, pid, label)
    } else {
        label
    }
}

fn format_from(host: &str, ip_only: bool) -> String {
    if ip_only {
        // If host is already a numeric IPv4/IPv6, return as-is. Otherwise
        // try a DNS lookup; this is best-effort and falls back to the host
        // string when resolution fails.
        if host.is_empty() || looks_like_ip(host) {
            host.to_string()
        } else {
            resolve_ip(host).unwrap_or_else(|| host.to_string())
        }
    } else if host.is_empty() {
        "-".to_string()
    } else {
        host.to_string()
    }
}

fn looks_like_ip(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}

fn resolve_ip(host: &str) -> Option<String> {
    use std::net::ToSocketAddrs as _;
    (host, 0)
        .to_socket_addrs()
        .ok()?
        .next()
        .map(|sa| sa.ip().to_string())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}
