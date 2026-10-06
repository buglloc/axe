//! Native procps-ng 4.0.6 sort keys, evaluated once from each raw row snapshot.
//!
//! Mappings follow `src/ps/output.c`'s format_array and `library/pids.c`;
//! grammar follows `src/ps/sortformat.c`'s long_sort_parse. Display rounding,
//! truncation and mixed-thread masking never participate in comparisons.

use crate::{fields::ProcessContext, format_helpers::tty_name, output};
use std::{
    cmp::Ordering,
    ffi::{CStr, CString},
};

unsafe extern "C" {
    fn strverscmp(left: *const libc::c_char, right: *const libc::c_char) -> libc::c_int;
    fn strcoll_l(
        left: *const libc::c_char,
        right: *const libc::c_char,
        locale: libc::locale_t,
    ) -> libc::c_int;
}

pub(super) enum SortValue {
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    Text(CString),
    StaticText(&'static CStr),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Pid,
    Tid,
    Ppid,
    Pgid,
    Sid,
    Tpgid,
    Euid,
    Ruid,
    Suid,
    Egid,
    Rgid,
    Euser,
    Ruser,
    Egroup,
    Rgroup,
    Comm,
    Args,
    State,
    Nice,
    Priority,
    Processor,
    Threads,
    VmSize,
    VsizeBytes,
    Rss,
    CpuPercent,
    CpuTicks,
    Elapsed,
    Start,
    Tty,
    Flags,
    Class,
    Fds,
    Wchan,
    Pending,
    Blocked,
    Ignored,
    Caught,
}

impl Key {
    fn parse(name: &str) -> Result<Self, String> {
        // Unlike -o lookup, native sort specifiers are case-sensitive.
        Ok(match name {
            "pid" | "tgid" => Self::Pid,
            "tid" | "lwp" | "spid" => Self::Tid,
            "ppid" => Self::Ppid,
            "pgid" | "pgrp" => Self::Pgid,
            "sid" | "sess" => Self::Sid,
            "tpgid" => Self::Tpgid,
            "uid" | "euid" => Self::Euid,
            "ruid" => Self::Ruid,
            "suid" => Self::Suid,
            "gid" | "egid" => Self::Egid,
            "rgid" => Self::Rgid,
            "user" | "uname" | "euser" => Self::Euser,
            "ruser" => Self::Ruser,
            "group" | "egroup" => Self::Egroup,
            "rgroup" => Self::Rgroup,
            "comm" | "ucmd" | "ucomm" => Self::Comm,
            "args" | "cmd" | "command" => Self::Args,
            "state" | "s" | "stat" => Self::State,
            "nice" | "ni" => Self::Nice,
            "pri" => Self::Priority,
            "psr" | "cpu" | "cpuid" => Self::Processor,
            "nlwp" | "thcount" => Self::Threads,
            // Procps distinguishes status VmSize (KiB) from stat vsize (bytes).
            "vsz" => Self::VmSize,
            "vsize" => Self::VsizeBytes,
            "rss" | "rsz" | "%mem" | "pmem" => Self::Rss,
            "%cpu" | "pcpu" | "c" => Self::CpuPercent,
            "time" | "cputime" | "cputimes" | "times" | "bsdtime" => Self::CpuTicks,
            "etime" | "etimes" => Self::Elapsed,
            "start" | "start_time" | "stime" | "bsdstart" | "lstart" => Self::Start,
            "tty" | "tt" | "tname" => Self::Tty,
            "flags" | "f" => Self::Flags,
            "cls" | "class" | "policy" => Self::Class,
            "fds" => Self::Fds,
            "wchan" => Self::Wchan,
            "pending" | "sig" | "sig_pend" => Self::Pending,
            "blocked" | "sig_block" | "sigmask" => Self::Blocked,
            "ignored" | "sig_ignore" | "sigignore" => Self::Ignored,
            "caught" | "sig_catch" | "sigcatch" => Self::Caught,
            _ => return Err(format!("unsupported sort specifier \"{name}\"")),
        })
    }

    fn value(self, ctx: &mut ProcessContext<'_>, utf8: bool) -> SortValue {
        use SortValue::{Float, Signed, StaticText, Unsigned};
        match self {
            Self::Pid => Signed(i64::from(ctx.status.tgid)),
            Self::Tid => Signed(i64::from(ctx.stat.pid)),
            Self::Ppid => Signed(i64::from(ctx.stat.ppid)),
            Self::Pgid => Signed(i64::from(ctx.stat.pgrp)),
            Self::Sid => Signed(i64::from(ctx.stat.session)),
            Self::Tpgid => Signed(i64::from(ctx.stat.tpgid)),
            Self::Euid => Unsigned(u64::from(ctx.status.euid)),
            Self::Ruid => Unsigned(u64::from(ctx.status.ruid)),
            Self::Suid => Unsigned(u64::from(ctx.status.suid)),
            Self::Egid => Unsigned(u64::from(ctx.status.egid)),
            Self::Rgid => Unsigned(u64::from(ctx.status.rgid)),
            Self::Euser => text(ctx.uid_cache.get(ctx.status.euid)),
            Self::Ruser => text(ctx.uid_cache.get(ctx.status.ruid)),
            Self::Egroup => text(ctx.gid_name(ctx.status.egid)),
            Self::Rgroup => text(ctx.gid_name(ctx.status.rgid)),
            Self::Comm => command_text(&ctx.stat.comm, false, utf8),
            Self::Args if ctx.cmdline.is_empty() => {
                // libproc's CMDLINE fallback is not an empty argv key.
                let mut bytes = Vec::with_capacity(ctx.stat.comm.len() + 13);
                bytes.push(b'[');
                output::write_text(&mut bytes, &ctx.stat.comm, false, utf8, usize::MAX)
                    .expect("writing to Vec cannot fail");
                bytes.push(b']');
                if ctx.stat.state == 'Z' {
                    bytes.extend_from_slice(b" <defunct>");
                }
                SortValue::Text(CString::new(bytes).expect("command text has no NUL bytes"))
            }
            Self::Args => command_text(ctx.cmdline, true, utf8),
            Self::State => Signed(i64::from(ctx.stat.state as u32)),
            Self::Nice => Signed(ctx.stat.nice),
            // PRI is displayed as 39 - priority but sorted by raw priority.
            Self::Priority => Signed(ctx.stat.priority),
            Self::Processor => Signed(i64::from(ctx.stat.processor.unwrap_or(0))),
            Self::Threads => Signed(ctx.group_stat.num_threads),
            Self::VmSize => Unsigned(ctx.group_status.vmsize.unwrap_or(0)),
            Self::VsizeBytes => Unsigned(ctx.stat.vsize),
            Self::Rss => Unsigned(ctx.rss_kb()),
            Self::CpuPercent => Float(ctx.cpu_pct()),
            // Common Hertz preserves ordering; keep integer precision instead
            // of converting TIME_ALL, TICS_ALL or TIME_ELAPSED into doubles.
            Self::CpuTicks => Unsigned(ctx.stat.utime + ctx.stat.stime),
            Self::Elapsed => Unsigned(ctx.elapsed_ticks()),
            Self::Start => Unsigned(ctx.stat.starttime),
            Self::Tty => SortValue::Text(
                CString::new(tty_name(ctx.stat.tty_nr)).expect("TTY names have no NUL bytes"),
            ),
            Self::Flags => Unsigned(u64::from(ctx.stat.flags)),
            Self::Class => StaticText(match ctx.stat.policy {
                None => c"-",
                Some(0) => c"TS",
                Some(1) => c"FF",
                Some(2) => c"RR",
                Some(3) => c"B",
                Some(4) => c"ISO",
                Some(5) => c"IDL",
                Some(6) => c"DLN",
                Some(7) => c"#7",
                Some(8) => c"#8",
                Some(9) => c"#9",
                Some(_) => c"?",
            }),
            Self::Fds => Unsigned(ctx.fds()),
            Self::Wchan => text(ctx.wchan()),
            // Fixed-width hexadecimal signal strings have numeric ordering.
            Self::Pending => Unsigned(ctx.pending_signals()),
            Self::Blocked => Unsigned(ctx.status.sigblk),
            Self::Ignored => Unsigned(ctx.status.sigign),
            Self::Caught => Unsigned(ctx.status.sigcgt),
        }
    }
}

fn text(value: &str) -> SortValue {
    let mut bytes = Vec::with_capacity(value.len() + 1);
    bytes.extend_from_slice(value.as_bytes());
    SortValue::Text(CString::new(bytes).expect("process and account names have no NUL bytes"))
}

fn command_text(value: &str, command_line: bool, utf8: bool) -> SortValue {
    let mut bytes = Vec::with_capacity(value.len() + 1);
    output::write_text(&mut bytes, value, command_line, utf8, usize::MAX)
        .expect("writing to Vec cannot fail");
    SortValue::Text(CString::new(bytes).expect("command text has no NUL bytes"))
}

fn text_ref(value: &SortValue) -> &CStr {
    match value {
        SortValue::Text(value) => value.as_c_str(),
        SortValue::StaticText(value) => value,
        _ => unreachable!("sort values are evaluated by the same specification"),
    }
}

struct Collation(libc::locale_t);

impl Collation {
    fn new() -> Option<Self> {
        // SAFETY: the empty, NUL-terminated locale name selects LC_COLLATE
        // from the environment; a null base creates a separately owned locale.
        let locale =
            unsafe { libc::newlocale(libc::LC_COLLATE_MASK, c"".as_ptr(), std::ptr::null_mut()) };
        if locale.is_null() {
            None
        } else {
            Some(Self(locale))
        }
    }
}

impl Drop for Collation {
    fn drop(&mut self) {
        // SAFETY: this locale is owned here; comparisons borrow it only while
        // the SortSpec is live, and no pointers into it are retained.
        unsafe { libc::freelocale(self.0) };
    }
}

struct SortKey {
    field: Key,
    value_index: usize,
    descending: bool,
}

#[derive(Default)]
pub(super) struct SortSpec {
    keys: Vec<SortKey>,
    fields: Vec<Key>,
    utf8: bool,
    collation: Option<Collation>,
}

impl SortSpec {
    pub(super) fn parse(specs: &[String]) -> Result<Self, String> {
        let Some(spec) = specs.first() else {
            return Ok(Self::default());
        };
        if specs.len() != 1 {
            return Err("multiple sort options".into());
        }
        if spec.is_empty() {
            return Err("empty sort list".into());
        }
        let is_separator = |byte: u8| matches!(byte, b',' | b' ' | b'\t' | b'\n');
        // Native accepts one trailing delimiter, but not leading/adjacent ones.
        let list = if is_separator(*spec.as_bytes().last().unwrap()) {
            &spec[..spec.len() - 1]
        } else {
            spec.as_str()
        };
        let capacity = list.bytes().filter(|&byte| is_separator(byte)).count() + 1;
        let mut result = Self {
            keys: Vec::with_capacity(capacity),
            fields: Vec::with_capacity(capacity),
            ..Self::default()
        };
        for token in list.split([',', ' ', '\t', '\n']) {
            if token.is_empty() {
                return Err("improper sort list".into());
            }
            let (name, descending) = if let Some(name) = token.strip_prefix('-') {
                (name, true)
            } else {
                (token.strip_prefix('+').unwrap_or(token), false)
            };
            let field = Key::parse(name)?;
            // Repeated aliases/directions share one snapshot, including text
            // ownership and expensive fds/wchan data acquisition.
            let value_index = match result.fields.iter().position(|&key| key == field) {
                Some(index) => index,
                None => {
                    result.fields.push(field);
                    result.fields.len() - 1
                }
            };
            result.keys.push(SortKey {
                field,
                value_index,
                descending,
            });
        }
        result.utf8 = result
            .fields
            .iter()
            .any(|key| matches!(key, Key::Comm | Key::Args))
            && output::utf8_locale();
        if result.fields.iter().any(|key| {
            matches!(
                key,
                Key::Comm
                    | Key::Args
                    | Key::Euser
                    | Key::Ruser
                    | Key::Egroup
                    | Key::Rgroup
                    | Key::Class
                    | Key::Wchan
            )
        }) {
            result.collation = Collation::new();
        }
        Ok(result)
    }

    pub(super) fn needs_cmdline(&self) -> bool {
        self.fields.contains(&Key::Args)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub(super) fn values(&self, ctx: &mut ProcessContext<'_>) -> Vec<SortValue> {
        self.fields
            .iter()
            .map(|key| key.value(ctx, self.utf8))
            .collect()
    }

    pub(super) fn compare(&self, left: &[SortValue], right: &[SortValue]) -> Ordering {
        for key in &self.keys {
            let order = match (&left[key.value_index], &right[key.value_index]) {
                (SortValue::Signed(left), SortValue::Signed(right)) => left.cmp(right),
                (SortValue::Unsigned(left), SortValue::Unsigned(right)) => left.cmp(right),
                (SortValue::Float(left), SortValue::Float(right)) => left.total_cmp(right),
                (left, right) => {
                    let left = text_ref(left);
                    let right = text_ref(right);
                    // SAFETY: both CStr values remain live and NUL-terminated;
                    // libc only reads their bytes and does not retain pointers.
                    let comparison = unsafe {
                        if key.field == Key::Tty {
                            strverscmp(left.as_ptr(), right.as_ptr())
                        } else if let Some(locale) = &self.collation {
                            strcoll_l(left.as_ptr(), right.as_ptr(), locale.0)
                        } else {
                            // Invalid/unavailable locales leave native ps in C.
                            libc::strcmp(left.as_ptr(), right.as_ptr())
                        }
                    };
                    comparison.cmp(&0)
                }
            };
            if order != Ordering::Equal {
                return if key.descending {
                    order.reverse()
                } else {
                    order
                };
            }
        }
        // PID/TID tie ordering and mixed-mode grouping belong to the row owner.
        Ordering::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields::RowKind;
    use procfs::process::{Stat, Status};
    use procfs::{FromBufRead, FromRead};
    use procutils_common::uid::UidCache;
    use std::{cell::OnceCell, collections::HashMap};

    fn fixture(pid: i32, name: &str) -> (Stat, Status, &'static str) {
        let mut values = [0_u64; 49];
        values[0] = 1;
        values[1] = pid as u64;
        values[2] = pid as u64;
        values[14] = 20;
        values[16] = 1;
        values[19] = 1_048_576;
        let tail = values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        let stat = Stat::from_read(format!("{pid} ({name}) S {tail}").as_bytes()).unwrap();
        let raw_status = format!(
            "Name:\t{name}\nState:\tS (sleeping)\nTgid:\t{pid}\nPid:\t{pid}\n\
             PPid:\t1\nTracerPid:\t0\nUid:\t0 0 0 0\nGid:\t0 0 0 0\n\
             FDSize:\t64\nGroups:\t\nVmSize:\t1024 kB\nVmRSS:\t128 kB\nThreads:\t1\n\
             SigQ:\t0/0\nSigPnd:\t0\nShdPnd:\t0\nSigBlk:\t0\nSigIgn:\t0\nSigCgt:\t0\n\
             CapInh:\t0\nCapPrm:\t0\nCapEff:\t0\n"
        );
        (
            stat,
            Status::from_buf_read(raw_status.as_bytes()).unwrap(),
            "",
        )
    }

    fn order(spec: &str, kind: RowKind, processes: &[(Stat, Status, &str)]) -> Vec<i32> {
        let sort = SortSpec::parse(&[spec.to_string()]).unwrap();
        let mut uid_cache = UidCache::new();
        let mut gid_cache = HashMap::from([(2, "z_group".into()), (10, "a_group".into())]);
        let mut rows: Vec<_> = processes
            .iter()
            .map(|(stat, status, cmdline)| {
                let mut ctx = ProcessContext {
                    stat,
                    status,
                    group_stat: stat,
                    group_status: status,
                    kind,
                    fds_cache: OnceCell::new(),
                    wchan_cache: OnceCell::new(),
                    cmdline,
                    uid_cache: &mut uid_cache,
                    gid_cache: &mut gid_cache,
                    boot_time: 0,
                    uptime_secs: 1000.0,
                    tps: 100,
                    total_mem_kb: 1_000_000,
                };
                (stat.pid, sort.values(&mut ctx))
            })
            .collect();
        rows.sort_by(|left, right| sort.compare(&left.1, &right.1));
        rows.into_iter().map(|(pid, _)| pid).collect()
    }

    #[test]
    fn cpu_and_memory_sort_below_display_precision() {
        let mut high = fixture(10, "same");
        let mut low = fixture(20, "same");
        high.0.utime = 101;
        low.0.utime = 100;
        high.1.vmrss = Some(129);
        low.1.vmrss = Some(128);
        for spec in ["%cpu", "pcpu", "c", "%mem", "pmem", "rss"] {
            assert_eq!(
                order(spec, RowKind::Process, &[high.clone(), low.clone()]),
                [20, 10],
                "{spec}"
            );
        }
    }

    #[test]
    fn integer_keys_keep_u64_precision_and_signed_priority() {
        let mut high = fixture(10, "same");
        let mut low = fixture(20, "same");
        high.0.vsize = (1_u64 << 53) + 1;
        low.0.vsize = 1_u64 << 53;
        high.0.priority = -1;
        low.0.priority = -99;
        high.0.utime = (1_u64 << 53) + 1;
        low.0.utime = 1_u64 << 53;
        for spec in ["vsize", "pri", "time", "cputimes", "bsdtime"] {
            assert_eq!(
                order(spec, RowKind::Process, &[high.clone(), low.clone()]),
                [20, 10],
                "{spec}"
            );
        }
    }

    #[test]
    fn aliases_keep_numeric_ids_but_names_use_text() {
        let mut low = fixture(10, "same");
        let mut high = fixture(20, "same");
        low.1.euid = 2;
        high.1.euid = 10;
        low.1.egid = 2;
        high.1.egid = 10;
        for spec in ["uid", "euid", "gid", "egid"] {
            assert_eq!(
                order(spec, RowKind::Process, &[high.clone(), low.clone()]),
                [10, 20],
                "{spec}"
            );
        }
        for spec in ["group", "egroup"] {
            assert_eq!(
                order(spec, RowKind::Process, &[low.clone(), high.clone()]),
                [20, 10],
                "{spec}"
            );
        }
    }

    #[test]
    fn command_arguments_and_class_keep_native_text_order() {
        let mut first = fixture(10, "z_comm");
        let mut second = fixture(20, "a_comm");
        first.2 = "a argv";
        second.2 = "z argv";
        first.0.policy = Some(0);
        second.0.policy = Some(3);
        for spec in ["args", "cmd", "command"] {
            assert_eq!(
                order(spec, RowKind::Process, &[second.clone(), first.clone()]),
                [10, 20],
                "{spec}"
            );
        }
        for spec in ["comm", "ucmd", "ucomm", "cls", "class", "policy"] {
            assert_eq!(
                order(spec, RowKind::Process, &[first.clone(), second.clone()]),
                [20, 10],
                "{spec}"
            );
        }
    }

    #[test]
    fn tty_numbers_use_version_order_not_lexical_or_device_order() {
        let mut first = fixture(10, "same");
        let mut second = fixture(20, "same");
        first.0.tty_nr = 0x880a; // pts/10
        second.0.tty_nr = 0x8802; // pts/2
        assert_eq!(
            order("tty", RowKind::Process, &[first.clone(), second.clone()]),
            [20, 10]
        );
        first.0.tty_nr = 0x401; // tty1, whose raw device ID is below pts/2
        assert_eq!(order("tty", RowKind::Process, &[first, second]), [20, 10]);
    }

    #[test]
    fn multiple_keys_keep_direction_precedence_and_true_ties() {
        let first = fixture(10, "same");
        let second = fixture(20, "same");
        let third = fixture(30, "a_name");
        let processes = [first, second, third];
        assert_eq!(
            order("comm,-pid", RowKind::Process, &processes),
            [30, 20, 10]
        );
        assert_eq!(
            order("-comm,+pid", RowKind::Process, &processes),
            [10, 20, 30]
        );
        assert_eq!(order("comm", RowKind::Process, &processes), [30, 10, 20]);
        assert_eq!(
            order(
                "comm",
                RowKind::Process,
                &[processes[1].clone(), processes[0].clone()]
            ),
            [20, 10]
        );
    }

    #[test]
    fn aggregate_and_task_sort_keys_ignore_display_masking() {
        let mut first = fixture(10, "same");
        let mut second = fixture(20, "same");
        first.1.tgid = 100;
        second.1.tgid = 100;
        first.0.nice = 12;
        second.0.nice = 7;
        first.1.shdpnd = 1;
        second.1.shdpnd = 2;
        first.1.sigpnd = 8;
        second.1.sigpnd = 4;
        let processes = [first, second];
        assert_eq!(order("nice", RowKind::ProcessSummary, &processes), [20, 10]);
        assert_eq!(order("-tid", RowKind::ThreadDetail, &processes), [20, 10]);
        assert_eq!(
            order("pending", RowKind::ProcessSummary, &processes),
            [10, 20]
        );
        assert_eq!(order("pending", RowKind::Thread, &processes), [20, 10]);
        assert_eq!(order("pid,-tid", RowKind::Thread, &processes), [20, 10]);
    }

    #[test]
    fn virtual_memory_aliases_use_their_native_raw_sources() {
        let mut first = fixture(10, "same");
        let mut second = fixture(20, "same");
        first.0.vsize = 1024;
        second.0.vsize = 2048;
        first.1.vmsize = Some(2000);
        second.1.vmsize = Some(1000);
        let processes = [first, second];
        assert_eq!(order("vsz", RowKind::Process, &processes), [20, 10]);
        assert_eq!(order("vsize", RowKind::Process, &processes), [10, 20]);
    }

    #[test]
    fn elapsed_and_start_keys_keep_tick_precision_and_opposite_order() {
        let mut older = fixture(10, "same");
        let mut newer = fixture(20, "same");
        older.0.starttime = 10_001;
        newer.0.starttime = 10_002;
        let processes = [older, newer];
        for spec in ["etime", "etimes", "-start", "-start_time", "-lstart"] {
            assert_eq!(
                order(spec, RowKind::Process, &processes),
                [20, 10],
                "{spec}"
            );
        }
        for spec in [
            "-etime",
            "-etimes",
            "start",
            "start_time",
            "stime",
            "bsdstart",
            "lstart",
        ] {
            assert_eq!(
                order(spec, RowKind::Process, &processes),
                [10, 20],
                "{spec}"
            );
        }
    }

    #[test]
    fn flags_and_state_ignore_cooked_columns() {
        let mut high = fixture(10, "same");
        let mut low = fixture(20, "same");
        high.0.flags = 0x400;
        low.0.flags = 0x200;
        high.0.state = 'S';
        low.0.state = 'R';
        let processes = [high, low];
        for spec in ["flags", "f", "state", "s", "stat"] {
            assert_eq!(
                order(spec, RowKind::Process, &processes),
                [20, 10],
                "{spec}"
            );
        }
    }

    #[test]
    fn command_keys_use_native_acquisition_escaping_and_empty_argv_fallback() {
        let mut control = fixture(10, "\tname");
        let mut punctuation = fixture(20, "!name");
        assert_eq!(
            order(
                "comm",
                RowKind::Process,
                &[control.clone(), punctuation.clone()]
            ),
            [20, 10]
        );
        control.2 = "\nargv";
        punctuation.2 = "\targv";
        assert_eq!(
            order(
                "args",
                RowKind::Process,
                &[punctuation.clone(), control.clone()]
            ),
            [10, 20]
        );
        control.2 = "";
        punctuation.2 = "!argv";
        assert_eq!(
            order("args", RowKind::Process, &[control, punctuation]),
            [20, 10]
        );
    }
}
