// This file is part of the uutils procps package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

use clap::{crate_version, Arg, ArgAction, ArgMatches, Command};
#[cfg(unix)]
use rustix::process::geteuid;
use uu_pgrep::process::{walk_process, ProcessInformation};
use uucore::error::UResult;

#[uucore::main]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let matches = uu_app().try_get_matches_from(args)?;

    let arg_program_name = matches.get_many::<String>("program-name");
    let arg_separator = matches.get_one::<String>("S").unwrap();

    if arg_program_name.is_none() {
        uucore::error::set_exit_code(1);
        return Ok(());
    };

    let collected = collect_matched_pids(&matches);

    if collected.is_empty() {
        uucore::error::set_exit_code(1);
        return Ok(());
    };

    let output = collected
        .into_iter()
        .map(|it| it.to_string())
        .collect::<Vec<_>>()
        .join(arg_separator);

    let flag_quiet = matches.get_flag("q");
    if !flag_quiet {
        println!("{output}");
    }

    Ok(())
}

fn match_process_name(
    process: &mut ProcessInformation,
    name_to_match: &str,
    with_workers: bool,
    match_scripts: bool,
) -> bool {
    // ProcessInformation flattens argv with spaces; pidof needs the original
    // NUL boundaries to distinguish executable paths containing spaces.
    let Ok(cmdline) = std::fs::read(format!("/proc/{}/cmdline", process.pid)) else {
        return false;
    };
    let mut argv = cmdline.split(|&byte| byte == 0);
    let argv0 = argv.next().unwrap_or_default();
    if argv0.is_empty() && !with_workers {
        return false;
    }
    let argv0 = argv0.strip_prefix(b"-").unwrap_or(argv0);
    let requested = OsStr::new(name_to_match);
    let requested_base = Path::new(requested).file_name().unwrap_or(requested);
    let argument_matches = |argument: &[u8]| {
        let path = OsStr::from_bytes(argument);
        Path::new(path).file_name() == Some(requested)
            || path == requested_base
            || path == requested
    };
    if argument_matches(argv0) {
        return true;
    }
    let comm = process.name().unwrap_or_default();
    if with_workers && comm == name_to_match {
        return true;
    }
    if let Ok(executable) = std::fs::read_link(format!("/proc/{}/exe", process.pid)) {
        if executable.as_os_str() == requested || executable.file_name() == Some(requested) {
            return true;
        }
    }
    if match_scripts {
        if let Some(script) = argv.next() {
            if let Some(base) = Path::new(OsStr::from_bytes(script)).file_name() {
                if base.as_bytes().starts_with(comm.as_bytes()) && argument_matches(script) {
                    return true;
                }
            }
        }
    }
    argv0.contains(&b' ') && comm == name_to_match
}

fn collect_matched_pids(matches: &ArgMatches) -> Vec<usize> {
    let program_names: Vec<_> = matches
        .get_many::<String>("program-name")
        .unwrap()
        .cloned()
        .collect();
    let with_workers = matches.get_flag("with-workers");
    let match_scripts = matches.get_flag("x");

    let collected = walk_process().collect::<Vec<_>>();
    let arg_omit_pid = matches
        .get_many::<usize>("o")
        .unwrap_or_default()
        .copied()
        .collect::<Vec<_>>();

    // Original pidof silently ignores the check-root option if the user is not root.
    #[cfg(unix)]
    let check_root = matches.get_flag("check-root") && geteuid().as_raw() == 0;
    #[cfg(not(unix))]
    let check_root = false;
    let our_root = ProcessInformation::current_process_info()
        .unwrap()
        .root()
        .unwrap();

    program_names
        .into_iter()
        .flat_map(|program| {
            let mut processed = Vec::new();
            for mut process in collected.clone() {
                if !match_process_name(&mut process, &program, with_workers, match_scripts) {
                    continue;
                }
                if arg_omit_pid.contains(&process.pid) {
                    continue;
                }
                if check_root && process.root().unwrap() != our_root {
                    continue;
                }

                if matches.get_flag("t") {
                    processed.extend_from_slice(process.thread_ids());
                } else {
                    processed.push(process.pid);
                }
            }

            processed.sort_by(|a, b| b.cmp(a));

            let flag_s = matches.get_flag("s");
            if flag_s {
                match processed.first() {
                    Some(first) => vec![*first],
                    None => Vec::new(),
                }
            } else {
                processed
            }
        })
        .collect()
}

#[allow(clippy::cognitive_complexity)]
pub fn uu_app() -> Command {
    Command::new(uucore::util_name())
        .version(crate_version!())
        .about("Find the process ID of a running program")
        .override_usage("pidof [options] [program [...]]")
        .infer_long_args(true)
        .arg(
            Arg::new("program-name")
                .help("Program name.")
                .index(1)
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("check-root")
                .short('c')
                .long("check-root")
                .help("Only return PIDs with the same root directory")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("S")
                .short('S')
                // the pidof bundled with Debian uses -d instead of -S
                .visible_short_alias('d')
                .long("separator")
                .help("Use SEP as separator between PIDs")
                .action(ArgAction::Set)
                .value_name("SEP")
                .default_value(" ")
                .hide_default_value(true),
        )
        // .arg(
        //     Arg::new("n")
        //         .short('n')
        //         .help("Avoid using stat system function on network shares")
        //         .action(ArgAction::SetTrue),
        // )
        .arg(
            Arg::new("o")
                .short('o')
                .long("omit-pid")
                .help("Omit results with a given PID")
                .value_delimiter(',')
                .action(ArgAction::Append)
                .value_parser(clap::value_parser!(usize))
                .value_name("PID"),
        )
        .arg(
            Arg::new("q")
                .short('q')
                .help("Quiet mode. Do not display output")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("s")
                .short('s')
                .long("single-shot")
                .help("Only return one PID")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("t")
                .short('t')
                .long("lightweight")
                .help("Show thread ids instead of process ids")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("with-workers")
                .short('w')
                .long("with-workers")
                .help("Show kernel worker threads as well")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("x")
                .short('x')
                .help("Return PIDs of shells running scripts with a matching name")
                .action(ArgAction::SetTrue),
        )
}
