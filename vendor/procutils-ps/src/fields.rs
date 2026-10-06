//! Output-field registry for `ps -o`.
//!
//! Each [`Field`] describes one column the user can request: its name
//! (and any procps-ng aliases), the default header, alignment, and a
//! pure function that turns a [`ProcessContext`] into the cell string.
//! [`lookup`] resolves a user-supplied name (case-insensitive) to a
//! `&'static Field`.
//!
//! When extending: add a new entry to [`FIELDS`]. The `compute`
//! function should never panic — fall back to `"-"` or an empty
//! string for missing data.

use crate::format_helpers::{
    format_bsdtime, format_cpu_percent, format_cputime, format_cputimes, format_elapsed,
    format_etimes, format_lstart, format_percent_tenths, format_start_compact, format_stat,
    tty_name,
};
use procfs::process::{Stat, Status};
use procutils_common::uid::UidCache;
use std::{cell::OnceCell, collections::HashMap, io::Read, os::unix::fs::MetadataExt};

/// Column alignment.
#[derive(Copy, Clone)]
pub enum Align {
    Left,
    Right,
}

#[derive(Copy, Clone)]
pub(super) enum RowKind {
    Process,
    Thread,
    ProcessSummary,
    ThreadDetail,
}

/// Raw process/task data and borrowed thread-group data for one row.
/// Requested display fields and sort keys share the optional acquisition caches.
pub struct ProcessContext<'a> {
    pub stat: &'a Stat,
    pub status: &'a Status,
    pub group_stat: &'a Stat,
    pub group_status: &'a Status,
    pub(super) kind: RowKind,
    pub(super) fds_cache: OnceCell<u64>,
    pub(super) wchan_cache: OnceCell<String>,
    pub cmdline: &'a str,
    pub uid_cache: &'a mut UidCache,
    pub gid_cache: &'a mut HashMap<u32, String>,
    pub boot_time: u64,
    pub uptime_secs: f64,
    pub tps: u64,
    pub total_mem_kb: u64,
}

impl ProcessContext<'_> {
    pub(super) fn elapsed_ticks(&self) -> u64 {
        let now_ticks = (self.uptime_secs * self.tps as f64) as u64;
        now_ticks.saturating_sub(self.stat.starttime)
    }

    fn elapsed_secs(&self) -> u64 {
        self.elapsed_ticks() / self.tps.max(1)
    }

    pub(super) fn cpu_pct(&self) -> f64 {
        let elapsed = self.elapsed_ticks();
        if elapsed > 0 {
            let total = self.stat.utime + self.stat.stime;
            // libproc2 UTILIZATION multiplies ticks in single precision,
            // then divides by the elapsed boot ticks in double precision.
            f64::from(total as f32 * 100.0) / elapsed as f64
        } else {
            0.0
        }
    }

    pub(super) fn rss_kb(&self) -> u64 {
        // procfs Status::vmrss is already in KiB. Procps reports zero when
        // status has no VmRSS (including zombies and kernel threads).
        self.group_status.vmrss.unwrap_or(0)
    }

    fn mem_tenths(&self) -> u64 {
        if self.total_mem_kb > 0 {
            ((u128::from(self.rss_kb()) * 1000 / u128::from(self.total_mem_kb)).min(999)) as u64
        } else {
            0
        }
    }

    pub(super) fn pending_signals(&self) -> u64 {
        match self.kind {
            RowKind::Process | RowKind::ProcessSummary => self.status.shdpnd,
            RowKind::Thread | RowKind::ThreadDetail => self.status.sigpnd,
        }
    }

    fn proc_path(&self, file: &str) -> String {
        match self.kind {
            RowKind::Process | RowKind::ProcessSummary => {
                format!("/proc/{}/{file}", self.status.tgid)
            }
            RowKind::Thread | RowKind::ThreadDetail => {
                format!("/proc/{}/task/{}/{file}", self.status.tgid, self.stat.pid)
            }
        }
    }

    pub(super) fn fds(&self) -> u64 {
        *self.fds_cache.get_or_init(|| {
            std::fs::metadata(self.proc_path("fd"))
                .map(|metadata| metadata.size())
                .unwrap_or(0)
        })
    }

    pub(super) fn wchan(&self) -> &str {
        self.wchan_cache.get_or_init(|| {
            let Ok(file) = std::fs::File::open(self.proc_path("wchan")) else {
                return "?".into();
            };
            let mut symbol = String::new();
            if file.take(63).read_to_string(&mut symbol).is_err() {
                return "?".into();
            }
            if symbol.is_empty() {
                return "?".into();
            }
            if symbol == "0" {
                return "-".into();
            }
            let normalized = symbol
                .strip_prefix('.')
                .unwrap_or(&symbol)
                .trim_start_matches('_');
            let prefix = symbol.len() - normalized.len();
            symbol.drain(..prefix);
            symbol
        })
    }

    /// Cache names once per run; rendered and sorted fields borrow the same lookup.
    pub(super) fn gid_name(&mut self, gid: u32) -> &str {
        self.gid_cache.entry(gid).or_insert_with(|| {
            std::fs::read_to_string("/etc/group")
                .ok()
                .and_then(|contents| {
                    contents.lines().find_map(|line| {
                        let mut fields = line.split(':');
                        let name = fields.next()?;
                        fields.next()?;
                        (fields.next()?.parse::<u32>().ok()? == gid).then(|| name.to_string())
                    })
                })
                .unwrap_or_else(|| gid.to_string())
        })
    }
}

/// Single registry entry.
pub struct Field {
    /// Canonical name as it appears in `-o` specs.
    pub name: &'static str,
    /// Procps-ng aliases. Lookup is case-insensitive across all of these.
    pub aliases: &'static [&'static str],
    /// Default header text. Overridden by a `=label` suffix on the spec.
    pub header: &'static str,
    pub align: Align,
    pub compute: fn(&mut ProcessContext) -> String,
}

impl Field {
    pub(super) fn render(&self, context: &mut ProcessContext<'_>) -> String {
        // procps output.c's PO/TO applicability applies only when m/-m
        // displays both summaries and tasks. Flat task modes print all fields.
        let process_only = matches!(
            self.name,
            "pid"
                | "tgid"
                | "ppid"
                | "pgid"
                | "sid"
                | "tpgid"
                | "comm"
                | "args"
                | "nlwp"
                | "vsz"
                | "rss"
                | "%mem"
                | "tty"
                | "fds"
        );
        let thread_only = matches!(
            self.name,
            "tid"
                | "lwp"
                | "spid"
                | "state"
                | "stat"
                | "nice"
                | "pri"
                | "psr"
                | "wchan"
                | "cls"
                | "blocked"
                | "ignored"
                | "caught"
        );
        if matches!(context.kind, RowKind::ProcessSummary) && thread_only
            || matches!(context.kind, RowKind::ThreadDetail) && process_only
        {
            "-".into()
        } else {
            (self.compute)(context)
        }
    }
}

/// Resolve a user-supplied column name to a registry entry. Match is
/// case-insensitive against the canonical name and every alias.
pub fn lookup(name: &str) -> Option<&'static Field> {
    let needle = name.to_ascii_lowercase();
    FIELDS.iter().find(|f| {
        f.name.eq_ignore_ascii_case(&needle)
            || f.aliases.iter().any(|a| a.eq_ignore_ascii_case(&needle))
    })
}

/// AIX/SVR4 single-character format codes (the `%X` syntax accepted by
/// `ps -o`). Procps-ng's `ps(1)` documents this table; we mirror it
/// exactly. Codes are case-sensitive: `%P` (ppid) and `%p` (pid) are
/// distinct.
const AIX_CODES: &[(&str, &str)] = &[
    ("C", "%cpu"),
    ("G", "group"),
    ("P", "ppid"),
    ("U", "user"),
    ("a", "args"),
    ("c", "comm"),
    ("g", "rgroup"),
    ("n", "nice"),
    ("p", "pid"),
    ("r", "pgid"),
    ("t", "etime"),
    ("u", "ruser"),
    ("x", "time"),
    ("y", "tty"),
    ("z", "vsz"),
];

/// Resolve an AIX-style single-character code to a registry entry.
/// Returns `None` for unknown codes; case-sensitive match.
pub fn lookup_aix(code: &str) -> Option<&'static Field> {
    AIX_CODES
        .iter()
        .find(|(c, _)| *c == code)
        .and_then(|(_, name)| lookup(name))
}

/// All known field names (canonical + aliases), used for error
/// messages and shell-completion eventually.
#[allow(dead_code)]
pub fn all_names() -> Vec<&'static str> {
    let mut v = Vec::with_capacity(FIELDS.len() * 2);
    for f in FIELDS {
        v.push(f.name);
        v.extend_from_slice(f.aliases);
    }
    v.sort_unstable();
    v
}

/// Static field table. Order is the order they appear in `--help`-style
/// listings; doesn't affect lookup.
pub static FIELDS: &[Field] = &[
    // ---- Identifiers ------------------------------------------------
    Field {
        name: "pid",
        aliases: &[],
        header: "PID",
        align: Align::Right,
        compute: |c| c.status.tgid.to_string(),
    },
    Field {
        name: "tgid",
        aliases: &[],
        header: "TGID",
        align: Align::Right,
        compute: |c| c.status.tgid.to_string(),
    },
    Field {
        name: "tid",
        aliases: &[],
        header: "TID",
        align: Align::Right,
        compute: |c| c.stat.pid.to_string(),
    },
    Field {
        name: "lwp",
        aliases: &[],
        header: "LWP",
        align: Align::Right,
        compute: |c| c.stat.pid.to_string(),
    },
    Field {
        name: "spid",
        aliases: &[],
        header: "SPID",
        align: Align::Right,
        compute: |c| c.stat.pid.to_string(),
    },
    Field {
        name: "ppid",
        aliases: &[],
        header: "PPID",
        align: Align::Right,
        compute: |c| c.stat.ppid.to_string(),
    },
    Field {
        name: "pgid",
        aliases: &["pgrp"],
        header: "PGID",
        align: Align::Right,
        compute: |c| c.stat.pgrp.to_string(),
    },
    Field {
        name: "sid",
        aliases: &["sess"],
        header: "SID",
        align: Align::Right,
        compute: |c| c.stat.session.to_string(),
    },
    Field {
        name: "tpgid",
        aliases: &[],
        header: "TPGID",
        align: Align::Right,
        compute: |c| c.stat.tpgid.to_string(),
    },
    // ---- User / group IDs -------------------------------------------
    Field {
        name: "uid",
        aliases: &["euid"],
        header: "UID",
        align: Align::Right,
        compute: |c| c.status.euid.to_string(),
    },
    Field {
        name: "ruid",
        aliases: &[],
        header: "RUID",
        align: Align::Right,
        compute: |c| c.status.ruid.to_string(),
    },
    Field {
        name: "suid",
        aliases: &[],
        header: "SUID",
        align: Align::Right,
        compute: |c| c.status.suid.to_string(),
    },
    Field {
        name: "gid",
        aliases: &["egid"],
        header: "GID",
        align: Align::Right,
        compute: |c| c.status.egid.to_string(),
    },
    Field {
        name: "rgid",
        aliases: &[],
        header: "RGID",
        align: Align::Right,
        compute: |c| c.status.rgid.to_string(),
    },
    Field {
        name: "user",
        aliases: &["uname", "euser"],
        header: "USER",
        align: Align::Left,
        compute: |c| c.uid_cache.get(c.status.euid).to_string(),
    },
    Field {
        name: "ruser",
        aliases: &[],
        header: "RUSER",
        align: Align::Left,
        compute: |c| c.uid_cache.get(c.status.ruid).to_string(),
    },
    Field {
        name: "group",
        aliases: &["egroup"],
        header: "GROUP",
        align: Align::Left,
        compute: |c| c.gid_name(c.status.egid).to_string(),
    },
    Field {
        name: "rgroup",
        aliases: &[],
        header: "RGROUP",
        align: Align::Left,
        compute: |c| c.gid_name(c.status.rgid).to_string(),
    },
    // ---- Command ----------------------------------------------------
    Field {
        name: "comm",
        aliases: &["ucmd", "ucomm"],
        header: "COMMAND",
        align: Align::Left,
        compute: |c| c.stat.comm.clone(),
    },
    Field {
        name: "args",
        aliases: &["cmd", "command"],
        header: "COMMAND",
        align: Align::Left,
        compute: |c| {
            if c.cmdline.is_empty() {
                format!("[{}]", c.stat.comm)
            } else {
                c.cmdline.to_string()
            }
        },
    },
    // ---- State / scheduling -----------------------------------------
    Field {
        name: "state",
        aliases: &["s"],
        header: "S",
        align: Align::Left,
        compute: |c| c.stat.state.to_string(),
    },
    Field {
        name: "stat",
        aliases: &[],
        header: "STAT",
        align: Align::Left,
        compute: |c| format_stat(c.stat.state, c.stat, c.status.tgid),
    },
    Field {
        name: "nice",
        aliases: &["ni"],
        header: "NI",
        align: Align::Right,
        compute: |c| c.stat.nice.to_string(),
    },
    Field {
        name: "pri",
        aliases: &[],
        header: "PRI",
        align: Align::Right,
        compute: |c| (39 - c.stat.priority).to_string(),
    },
    Field {
        name: "psr",
        aliases: &["processor"],
        header: "PSR",
        align: Align::Right,
        compute: |c| c.stat.processor.map_or("?".into(), |p| p.to_string()),
    },
    Field {
        name: "nlwp",
        aliases: &["thcount"],
        header: "NLWP",
        align: Align::Right,
        compute: |c| c.group_stat.num_threads.to_string(),
    },
    // ---- Memory -----------------------------------------------------
    Field {
        name: "vsz",
        aliases: &["vsize"],
        header: "VSZ",
        align: Align::Right,
        compute: |c| (c.group_stat.vsize / 1024).to_string(),
    },
    Field {
        name: "rss",
        aliases: &["rsz"],
        header: "RSS",
        align: Align::Right,
        compute: |c| c.rss_kb().to_string(),
    },
    Field {
        name: "%mem",
        aliases: &["pmem"],
        header: "%MEM",
        align: Align::Right,
        compute: |c| format_percent_tenths(c.mem_tenths()),
    },
    // ---- CPU --------------------------------------------------------
    Field {
        name: "%cpu",
        aliases: &["pcpu"],
        header: "%CPU",
        align: Align::Right,
        compute: |c| format_cpu_percent(c.cpu_pct()),
    },
    Field {
        name: "c",
        aliases: &[],
        header: "C",
        align: Align::Right,
        compute: |c| (c.cpu_pct() as u64).min(99).to_string(),
    },
    Field {
        name: "time",
        aliases: &["cputime"],
        header: "TIME",
        align: Align::Right,
        compute: |c| format_cputime(c.stat.utime + c.stat.stime, c.tps),
    },
    Field {
        name: "cputimes",
        aliases: &["times"],
        header: "TIME",
        align: Align::Right,
        compute: |c| format_cputimes(c.stat.utime + c.stat.stime, c.tps),
    },
    Field {
        name: "bsdtime",
        aliases: &[],
        header: "TIME",
        align: Align::Right,
        compute: |c| format_bsdtime(c.stat.utime + c.stat.stime, c.tps),
    },
    Field {
        name: "etime",
        aliases: &[],
        header: "ELAPSED",
        align: Align::Right,
        compute: |c| format_elapsed(c.elapsed_secs()),
    },
    Field {
        name: "etimes",
        aliases: &[],
        header: "ELAPSED",
        align: Align::Right,
        compute: |c| format_etimes(c.elapsed_secs()),
    },
    Field {
        name: "start",
        aliases: &["start_time", "stime", "bsdstart"],
        header: "START",
        align: Align::Left,
        compute: |c| format_start_compact(c.stat.starttime, c.boot_time, c.tps),
    },
    Field {
        name: "lstart",
        aliases: &[],
        header: "STARTED",
        align: Align::Left,
        compute: |c| format_lstart(c.stat.starttime, c.boot_time, c.tps),
    },
    // ---- TTY --------------------------------------------------------
    Field {
        name: "tty",
        aliases: &["tt", "tname"],
        header: "TTY",
        align: Align::Left,
        compute: |c| tty_name(c.stat.tty_nr),
    },
    // ---- Other ------------------------------------------------------
    Field {
        name: "wchan",
        aliases: &[],
        header: "WCHAN",
        align: Align::Left,
        compute: |c| c.wchan().to_string(),
    },
    Field {
        name: "flags",
        aliases: &["f"],
        header: "F",
        align: Align::Right,
        // Unix98 flags are the three legacy flag bits, displayed in octal.
        compute: |c| format!("{:o}", (c.stat.flags >> 6) & 7),
    },
    // ---- Scheduling class ------------------------------------------
    Field {
        name: "cls",
        aliases: &["class", "policy"],
        header: "CLS",
        align: Align::Right,
        compute: |c| sched_class_str(c.stat.policy).into(),
    },
    // ---- Open file descriptors -------------------------------------
    Field {
        name: "fds",
        aliases: &[],
        header: "FDS",
        align: Align::Right,
        compute: |c| c.fds().to_string(),
    },
    // ---- Signal masks (16-char hex, like procps) -------------------
    Field {
        name: "pending",
        aliases: &["sig", "sig_pend"],
        header: "PENDING",
        align: Align::Left,
        compute: |c| format!("{:016x}", c.pending_signals()),
    },
    Field {
        name: "blocked",
        aliases: &["sig_block", "sigmask"],
        header: "BLOCKED",
        align: Align::Left,
        compute: |c| format!("{:016x}", c.status.sigblk),
    },
    Field {
        name: "ignored",
        aliases: &["sig_ignore", "sigignore"],
        header: "IGNORED",
        align: Align::Left,
        compute: |c| format!("{:016x}", c.status.sigign),
    },
    Field {
        name: "caught",
        aliases: &["sig_catch", "sigcatch"],
        header: "CAUGHT",
        align: Align::Left,
        compute: |c| format!("{:016x}", c.status.sigcgt),
    },
];

/// Map a Linux scheduler policy number to procps's two/three-letter
/// abbreviation. Returns `"-"` when policy isn't reported by the
/// kernel (older /proc/[pid]/stat formats), `"?"` for unknown values.
fn sched_class_str(policy: Option<u32>) -> &'static str {
    match policy {
        None => "-",
        Some(0) => "TS",
        Some(1) => "FF",
        Some(2) => "RR",
        Some(3) => "B",
        Some(4) => "ISO",
        Some(5) => "IDL",
        Some(6) => "DLN",
        Some(_) => "?",
    }
}
