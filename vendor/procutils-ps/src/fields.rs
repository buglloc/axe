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
    format_bsdtime, format_cputime, format_cputimes, format_elapsed, format_etimes, format_lstart,
    format_start_compact, format_stat, tty_name,
};
use procfs::process::{Stat, Status};
use procutils_common::uid::UidCache;
use std::collections::HashMap;

/// Column alignment.
#[derive(Copy, Clone)]
pub enum Align {
    Left,
    Right,
}

/// Everything a `compute` function might need. Built once per process,
/// reused across every requested field for that process.
pub struct ProcessContext<'a> {
    pub stat: &'a Stat,
    pub status: &'a Status,
    pub cmdline: &'a str,
    pub uid_cache: &'a mut UidCache,
    pub gid_cache: &'a mut HashMap<u32, String>,
    pub boot_time: u64,
    pub uptime_secs: f64,
    pub tps: u64,
    pub total_mem_kb: u64,
    pub page_size: u64,
}

impl ProcessContext<'_> {
    fn elapsed_ticks(&self) -> u64 {
        let now_ticks = (self.uptime_secs * self.tps as f64) as u64;
        now_ticks.saturating_sub(self.stat.starttime)
    }

    fn cpu_pct(&self) -> f64 {
        let total = self.stat.utime + self.stat.stime;
        let elapsed = self.elapsed_ticks();
        if elapsed > 0 {
            (total as f64 / elapsed as f64) * 100.0
        } else {
            0.0
        }
    }

    fn rss_bytes(&self) -> u64 {
        self.stat.rss * self.page_size
    }

    fn mem_pct(&self) -> f64 {
        if self.total_mem_kb > 0 {
            (self.rss_bytes() as f64 / 1024.0 / self.total_mem_kb as f64) * 100.0
        } else {
            0.0
        }
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
        aliases: &["tid", "lwp"],
        header: "PID",
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
        compute: |c| gid_name(c, c.status.egid),
    },
    Field {
        name: "rgroup",
        aliases: &[],
        header: "RGROUP",
        align: Align::Left,
        compute: |c| gid_name(c, c.status.rgid),
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
        compute: |c| format_stat(c.stat.state, c.stat),
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
        compute: |c| c.stat.priority.to_string(),
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
        compute: |c| c.stat.num_threads.to_string(),
    },
    // ---- Memory -----------------------------------------------------
    Field {
        name: "vsz",
        aliases: &["vsize"],
        header: "VSZ",
        align: Align::Right,
        compute: |c| (c.stat.vsize / 1024).to_string(),
    },
    Field {
        name: "rss",
        aliases: &["rsz"],
        header: "RSS",
        align: Align::Right,
        compute: |c| (c.rss_bytes() / 1024).to_string(),
    },
    Field {
        name: "%mem",
        aliases: &["pmem"],
        header: "%MEM",
        align: Align::Right,
        compute: |c| format!("{:.1}", c.mem_pct()),
    },
    // ---- CPU --------------------------------------------------------
    Field {
        name: "%cpu",
        aliases: &["pcpu"],
        header: "%CPU",
        align: Align::Right,
        compute: |c| format!("{:.1}", c.cpu_pct()),
    },
    Field {
        name: "c",
        aliases: &[],
        header: "C",
        align: Align::Right,
        compute: |c| format!("{}", (c.cpu_pct() as u64).min(99)),
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
        compute: |c| {
            let elapsed_secs = c.uptime_secs as i64 - (c.stat.starttime / c.tps) as i64;
            format_elapsed(elapsed_secs.max(0) as u64)
        },
    },
    Field {
        name: "etimes",
        aliases: &[],
        header: "ELAPSED",
        align: Align::Right,
        compute: |c| {
            let elapsed_secs = c.uptime_secs as i64 - (c.stat.starttime / c.tps) as i64;
            format_etimes(elapsed_secs.max(0) as u64)
        },
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
        compute: |c| {
            std::fs::read_to_string(format!("/proc/{}/wchan", c.stat.pid))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "-".into())
        },
    },
    Field {
        name: "flags",
        aliases: &["f"],
        header: "F",
        align: Align::Right,
        // procps reports the flag word divided by 64 for legacy compat.
        compute: |c| format!("{}", c.stat.flags >> 6),
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
        compute: |c| {
            std::fs::read_dir(format!("/proc/{}/fd", c.stat.pid))
                .map(|d| d.count().to_string())
                .unwrap_or_else(|_| "?".into())
        },
    },
    // ---- Signal masks (16-char hex, like procps) -------------------
    Field {
        name: "pending",
        aliases: &["sig", "sig_pend"],
        header: "PENDING",
        align: Align::Left,
        compute: |c| format!("{:016x}", c.status.sigpnd),
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

/// Resolve a GID to a group name via `/etc/group`, caching results in
/// the per-run map. Falls back to the numeric value if the group
/// doesn't exist.
fn gid_name(c: &mut ProcessContext, gid: u32) -> String {
    if let Some(name) = c.gid_cache.get(&gid) {
        return name.clone();
    }
    let resolved = std::fs::read_to_string("/etc/group")
        .ok()
        .and_then(|contents| {
            for line in contents.lines() {
                let fields: Vec<&str> = line.split(':').collect();
                if fields.len() >= 3
                    && let Ok(file_gid) = fields[2].parse::<u32>()
                    && file_gid == gid
                {
                    return Some(fields[0].to_string());
                }
            }
            None
        })
        .unwrap_or_else(|| gid.to_string());
    c.gid_cache.insert(gid, resolved.clone());
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_canonical_and_aliases() {
        assert_eq!(lookup("pid").map(|f| f.name), Some("pid"));
        assert_eq!(lookup("PID").map(|f| f.name), Some("pid"));
        // `tid` is an alias of `pid` here (no thread-table support yet).
        assert_eq!(lookup("tid").map(|f| f.name), Some("pid"));
        assert_eq!(lookup("ucmd").map(|f| f.name), Some("comm"));
        assert_eq!(lookup("cmd").map(|f| f.name), Some("args"));
        assert!(lookup("nosuchfield").is_none());
    }

    #[test]
    fn pid_and_args_are_distinct_fields() {
        let pid = lookup("pid").unwrap();
        let args = lookup("args").unwrap();
        assert!(!std::ptr::eq(pid, args));
    }
}
