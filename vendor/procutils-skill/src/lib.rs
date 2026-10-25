use clap::Parser;
use procutils_common::{
    MAX_TERM_WIDTH,
    man::ManContent,
    procmatch::{MatchOptions, find_matching_processes},
    signal::{all_signals, parse_signum_any},
};
use std::process::ExitCode;

pub const MAN: ManContent = ManContent {
    description: Some(include_str!("../man/description.man")),
    extra_sections: &[
        ("EXAMPLES", include_str!("../man/examples.man")),
        ("NOTES", include_str!("../man/notes.man")),
        ("DIVERGENCES", include_str!("../man/divergences.man")),
        ("SEE ALSO", include_str!("../man/see_also.man")),
    ],
};

/// Lift the first `-SIG` / `-N` / `-NAME` argument out of `argv` and
/// rewrite it as `--signal SIG`, so the rest of the args parse normally
/// under clap. The signal can appear anywhere on the command line.
/// `argv[0]` is preserved untouched.
pub fn preprocess_argv(argv: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len() + 1);
    let mut iter = argv.into_iter();
    if let Some(arg0) = iter.next() {
        out.push(arg0);
    }
    let mut found = false;
    for arg in iter {
        if !found
            && arg.starts_with('-')
            && !arg.starts_with("--")
            && arg.len() > 1
            && (arg == "-0" || parse_signum_any(&arg[1..]).is_some())
        {
            out.push("--signal".to_string());
            out.push(arg[1..].to_string());
            found = true;
        } else {
            out.push(arg);
        }
    }
    out
}

/// Send a signal to processes selected by user, tty, pid, or command.
///
/// `skill` is obsolete; prefer `pkill` for new use cases. The signal
/// may be given as `--signal SIG` or as a short `-SIG` flag at any
/// position; names with or without the `SIG` prefix and signal
/// numbers are all accepted.
#[derive(Parser)]
#[command(
    name = "skill",
    version,
    about,
    disable_help_flag = true,
    max_term_width = MAX_TERM_WIDTH,
    override_usage = "skill [signal] [options] <expression>"
)]
pub struct Args {
    /// Print help.
    #[arg(long, action = clap::ArgAction::HelpLong)]
    help: Option<bool>,

    /// Signal to send (name or number). Default: TERM.
    #[arg(long)]
    signal: Option<String>,

    /// List signal names.
    #[arg(short = 'l', long)]
    list: bool,

    /// Print signal table (number + name).
    #[arg(short = 'L', long)]
    table: bool,

    /// No-op mode: print matching PIDs instead of signaling.
    #[arg(short = 'n', long = "no-action")]
    no_action: bool,

    /// Verbose: explain what is being done.
    #[arg(short, long)]
    verbose: bool,

    /// Match processes by tty (repeatable).
    #[arg(short = 't', long, value_name = "TTY")]
    tty: Vec<String>,

    /// Match processes by user name or UID (repeatable).
    #[arg(short = 'u', long, value_name = "USER")]
    user: Vec<String>,

    /// Match processes by PID (repeatable).
    #[arg(short = 'p', long, value_name = "PID")]
    pid: Vec<i32>,

    /// Match processes by command name (repeatable, exact match).
    #[arg(short = 'c', long, value_name = "COMMAND")]
    command: Vec<String>,

    /// Free-form bare PIDs.
    expr: Vec<String>,
}

pub fn run(args: Args) -> ExitCode {
    if args.list {
        let names: Vec<String> = all_signals().map(|(_, n)| n).collect();
        println!("{}", names.join(" "));
        return ExitCode::SUCCESS;
    }

    if args.table {
        let mut line = String::new();
        for (i, (num, name)) in all_signals().enumerate() {
            line.push_str(&format!("{num:>2} {name}"));
            if (i + 1) % 8 == 0 {
                println!("{line}");
                line.clear();
            } else {
                line.push('\t');
            }
        }
        if !line.is_empty() {
            println!("{}", line.trim_end());
        }
        return ExitCode::SUCCESS;
    }

    // Resolve the signal: --signal wins, otherwise default to TERM.
    let signum: i32 = match args.signal.as_deref() {
        Some("0") => 0,
        Some(s) => match parse_signum_any(s) {
            Some(n) => n,
            None => {
                eprintln!("skill: unknown signal: {s}");
                return ExitCode::from(2);
            }
        },
        None => libc::SIGTERM,
    };

    // Bare expression args become extra PID filters.
    let mut pid_filter: Vec<i32> = args.pid.clone();
    for s in &args.expr {
        match s.parse::<i32>() {
            Ok(p) => pid_filter.push(p),
            Err(_) => {
                eprintln!("skill: bare expression argument must be a PID: {s}");
                return ExitCode::from(2);
            }
        }
    }

    let no_filter = pid_filter.is_empty()
        && args.tty.is_empty()
        && args.user.is_empty()
        && args.command.is_empty();
    if no_filter {
        eprintln!("skill: no expression given");
        eprintln!("Usage: skill [signal] [options] <expression>");
        return ExitCode::from(2);
    }

    // Build a regex pattern from -c COMMAND values: matched as exact
    // process names, OR'd together.
    let pattern = if args.command.is_empty() {
        String::new()
    } else {
        let alts: Vec<String> = args.command.iter().map(|c| regex::escape(c)).collect();
        format!("^({})$", alts.join("|"))
    };

    let opts = MatchOptions {
        pattern,
        full: false,
        ignore_case: false,
        exact: false,
        inverse: false,
        newest: false,
        oldest: false,
        older: None,
        pid: if pid_filter.is_empty() {
            None
        } else {
            Some(pid_filter)
        },
        parent: None,
        pgroup: None,
        group: None,
        session: None,
        terminal: if args.tty.is_empty() {
            None
        } else {
            Some(args.tty.clone())
        },
        euid: if args.user.is_empty() {
            None
        } else {
            Some(args.user.clone())
        },
        uid: None,
        runstates: None,
        env: None,
    };

    let matches = match find_matching_processes(&opts, "skill") {
        Ok(m) => m,
        Err(code) => return code,
    };

    if matches.is_empty() {
        return ExitCode::from(1);
    }

    if args.no_action {
        for p in &matches {
            println!("{}", p.pid);
        }
        return ExitCode::SUCCESS;
    }

    let mut any_failed = false;
    for p in &matches {
        let rc = unsafe { libc::kill(p.pid as libc::pid_t, signum) };
        if rc == 0 {
            if args.verbose {
                println!("{}: {}", p.pid, p.comm);
            }
        } else {
            let errno = std::io::Error::last_os_error();
            eprintln!("skill: ({}) - {errno}", p.pid);
            any_failed = true;
        }
    }

    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
