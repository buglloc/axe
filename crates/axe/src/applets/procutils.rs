use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

pub fn free(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_free::Args>(args, procutils_free::run)
}

pub fn hugetop(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_hugetop::Args>(args, procutils_hugetop::run)
}

pub fn pgrep(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_pgrep::Args>(args, procutils_pgrep::run)
}

pub fn pmap(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_pmap::Args>(args, procutils_pmap::run)
}

pub fn pwdx(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_pwdx::Args>(args, procutils_pwdx::run)
}

pub fn skill(args: Vec<OsString>) -> i32 {
    let args = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let args = procutils_skill::preprocess_argv(args);
    parse_and_run::<procutils_skill::Args>(
        args.into_iter().map(OsString::from).collect(),
        procutils_skill::run,
    )
}

pub fn slabtop(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_slabtop::Args>(args, procutils_slabtop::run)
}

pub fn snice(args: Vec<OsString>) -> i32 {
    let args = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let args = procutils_snice::preprocess_argv(args);
    parse_and_run::<procutils_snice::Args>(
        args.into_iter().map(OsString::from).collect(),
        procutils_snice::run,
    )
}

pub fn sysctl(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_sysctl::Args>(args, procutils_sysctl::run)
}

pub fn tload(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_tload::Args>(args, procutils_tload::run)
}

pub fn top(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_top::Args>(args, procutils_top::run)
}

pub fn vmstat(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_vmstat::Args>(args, procutils_vmstat::run)
}

pub fn w(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_w::Args>(args, procutils_w::run)
}

pub fn ps(args: Vec<OsString>) -> i32 {
    let args = preprocess_ps_args(args);
    parse_and_run::<procutils_ps::Args>(args, procutils_ps::run)
}

#[derive(Default)]
struct PsCompatibility {
    bsd_personality: bool,
    show_all: bool,
    include_no_tty: bool,
    user_format: bool,
    user_filter: bool,
    explicit_pid: bool,
    explicit_format: bool,
    running_only: bool,
    terminal_only: bool,
    unix_all_tty: bool,
    standard_all: bool,
}

fn preprocess_ps_args(mut args: Vec<OsString>) -> Vec<OsString> {
    let mut compatibility = PsCompatibility::default();
    let mut expects_value = false;
    let mut option_end = args.len();

    for (index, arg) in args.iter_mut().enumerate().skip(1) {
        if expects_value {
            expects_value = false;
            continue;
        }

        let bytes = arg.as_bytes();
        if bytes == b"--" {
            option_end = index;
            break;
        }

        if is_bsd_option_cluster(bytes) {
            compatibility.bsd_personality = true;
            compatibility.observe_bsd_options(bytes);

            let mut normalized = bytes
                .iter()
                .copied()
                .filter(|option| matches!(option, b'a' | b'u' | b'x'))
                .collect::<Vec<_>>();
            if normalized.is_empty() {
                normalized.push(b'x');
            }
            normalized.insert(0, b'-');
            *arg = OsString::from_vec(normalized);
            continue;
        }

        expects_value = compatibility.observe_standard_option(bytes);
    }

    if !compatibility.bsd_personality
        && !(compatibility.unix_all_tty && !compatibility.standard_all)
    {
        return args;
    }

    let exact_selection = (compatibility.bsd_personality
        && (compatibility.show_all
            || compatibility.include_no_tty
            || compatibility.running_only
            || compatibility.terminal_only))
        || (compatibility.unix_all_tty && !compatibility.standard_all);
    let can_inject_selection = !compatibility.explicit_pid && !compatibility.user_filter;
    let mut extra = Vec::with_capacity(4);
    if exact_selection && can_inject_selection {
        let pids = selected_ps_pids(&compatibility);
        extra.push(OsString::from("-p"));
        extra.push(OsString::from(pids));
    } else if compatibility.include_no_tty && !compatibility.show_all && !compatibility.user_filter
    {
        // SAFETY: geteuid has no preconditions and does not dereference pointers.
        let effective_uid = unsafe { libc::geteuid() };
        extra.push(OsString::from("-U"));
        extra.push(OsString::from(effective_uid.to_string()));
    }

    if compatibility.bsd_personality && !compatibility.explicit_format {
        let format = if compatibility.user_format {
            "user,pid,%cpu,%mem,vsz,rss,tty,stat,start,bsdtime,command"
        } else {
            "pid,tty,stat,bsdtime,command"
        };
        extra.push(OsString::from("-o"));
        extra.push(OsString::from(format));
    }

    args.splice(option_end..option_end, extra);
    args
}

impl PsCompatibility {
    fn observe_bsd_options(&mut self, options: &[u8]) {
        for option in options {
            match option {
                b'a' => self.show_all = true,
                b'u' => self.user_format = true,
                b'x' => self.include_no_tty = true,
                b'r' => self.running_only = true,
                b'T' => self.terminal_only = true,
                b'w' => {}
                _ => unreachable!("validated BSD option"),
            }
        }
    }

    fn observe_standard_option(&mut self, option: &[u8]) -> bool {
        match option {
            b"--all" => {
                self.show_all = true;
                self.standard_all = true;
                return false;
            }
            b"--format" => {
                self.explicit_format = true;
                return true;
            }
            value if value.starts_with(b"--format=") => {
                self.explicit_format = true;
                return false;
            }
            b"--user" => {
                self.user_filter = true;
                return true;
            }
            value if value.starts_with(b"--user=") => {
                self.user_filter = true;
                return false;
            }
            b"--pid" => {
                self.explicit_pid = true;
                return true;
            }
            value if value.starts_with(b"--pid=") => {
                self.explicit_pid = true;
                return false;
            }
            _ => {}
        }

        let Some(options) = option
            .strip_prefix(b"-")
            .filter(|options| !options.is_empty())
        else {
            return false;
        };

        for (index, option) in options.iter().copied().enumerate() {
            match option {
                b'A' | b'e' => {
                    self.show_all = true;
                    self.standard_all = true;
                }
                b'a' => self.unix_all_tty = true,
                b'u' => self.user_format = true,
                b'x' => self.include_no_tty = true,
                b'f' => self.explicit_format = true,
                b'U' => {
                    self.user_filter = true;
                    return index + 1 == options.len();
                }
                b'o' => {
                    self.explicit_format = true;
                    return index + 1 == options.len();
                }
                b'p' => {
                    self.explicit_pid = true;
                    return index + 1 == options.len();
                }
                _ => {}
            }
        }

        false
    }
}

fn is_bsd_option_cluster(arg: &[u8]) -> bool {
    !arg.is_empty()
        && arg
            .iter()
            .all(|option| matches!(option, b'a' | b'u' | b'x' | b'r' | b'T' | b'w'))
}

fn selected_ps_pids(options: &PsCompatibility) -> String {
    let current_tty = read_ps_process(std::process::id())
        .map(|process| process.tty)
        .unwrap_or_default();
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    let mut pids = fs::read_dir("/proc")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(read_ps_process)
        .filter(|process| {
            if !options.bsd_personality {
                return process.tty != 0 && process.pid != process.session;
            }
            let selected = if options.standard_all {
                true
            } else if options.terminal_only {
                current_tty != 0 && process.tty == current_tty
            } else if options.show_all && options.include_no_tty {
                process.tty != 0 || process.euid == effective_uid
            } else if options.show_all {
                process.tty != 0
            } else if options.include_no_tty {
                process.euid == effective_uid
            } else {
                process.euid == effective_uid && process.tty == current_tty
            };
            selected && (!options.running_only || process.state == b'R')
        })
        .map(|process| process.pid)
        .collect::<Vec<_>>();
    pids.sort_unstable();
    if pids.is_empty() {
        "0".to_owned()
    } else {
        pids.into_iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

struct PsProcess {
    pid: u32,
    euid: u32,
    state: u8,
    session: u32,
    tty: i64,
}

fn read_ps_process(pid: u32) -> Option<PsProcess> {
    let base = format!("/proc/{pid}");
    let stat = fs::read_to_string(format!("{base}/stat")).ok()?;
    let fields = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    let state = fields.first()?.as_bytes().first().copied()?;
    let session = fields.get(3)?.parse().ok()?;
    let tty = fields.get(4)?.parse().ok()?;
    let status = fs::read_to_string(format!("{base}/status")).ok()?;
    let euid = status.lines().find_map(|line| {
        line.strip_prefix("Uid:")?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    })?;
    Some(PsProcess {
        pid,
        euid,
        state,
        session,
        tty,
    })
}

pub fn pkill(args: Vec<OsString>) -> i32 {
    let args = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let args = procutils_pkill::preprocess_argv(args);
    parse_and_run::<procutils_pkill::Args>(
        args.into_iter().map(OsString::from).collect(),
        procutils_pkill::run,
    )
}

pub fn pidof(args: Vec<OsString>) -> i32 {
    run_uutil("uu_pidof", args, uu_pidof::uumain)
}

pub fn pidwait(args: Vec<OsString>) -> i32 {
    run_uutil("uu_pidwait", args, uu_pidwait::uumain)
}

pub fn watch(mut args: Vec<OsString>) -> i32 {
    if let Ok(matches) = procutils_watch::Args::command().try_get_matches_from(args.clone())
        && matches.get_flag("exec")
        && let Some(command_index) = matches
            .indices_of("command")
            .and_then(|mut indices| indices.next())
        && let Err(error) = crate::child::rewrite(&mut args, command_index)
    {
        eprintln!("watch: {error}");
        return 4;
    }
    parse_and_run::<procutils_watch::Args>(args, procutils_watch::run)
}

fn run_uutil(
    util_crate_name: &str,
    args: Vec<OsString>,
    run: impl FnOnce(std::vec::IntoIter<OsString>) -> i32,
) -> i32 {
    procps_uucore::panic::preserve_inherited_sigpipe();
    procps_uucore::panic::mute_sigpipe_panic();

    let name = procps_uucore::get_canonical_util_name(util_crate_name);
    if let Err(error) = procps_uucore::locale::setup_localization(name) {
        eprintln!("axe: could not initialize localization for '{name}': {error}");
        return 99;
    }

    let code = run(args.into_iter());
    if let Err(error) = std::io::stdout().flush() {
        eprintln!("axe: error flushing stdout: {error}");
    }
    code
}

fn parse_and_run<A: Parser>(args: Vec<OsString>, run: fn(A) -> ExitCode) -> i32 {
    let args = match A::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            let code = if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                0
            } else {
                2
            };
            let _ = error.print();
            return code;
        }
    };

    let status = run(args);
    if status == ExitCode::SUCCESS {
        0
    } else if status == ExitCode::from(2) {
        2
    } else if status == ExitCode::from(3) {
        3
    } else {
        1
    }
}
