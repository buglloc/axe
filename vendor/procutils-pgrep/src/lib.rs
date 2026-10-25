use clap::Parser;
use procutils_common::{MAX_TERM_WIDTH, man::ManContent, procmatch::MatchOptions};
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

const MATCHING: &str = "Matching";
const FILTERS: &str = "Filters";
const SELECTION: &str = "Selection";
const OUTPUT: &str = "Output";

/// Look up processes based on name and other attributes.
#[derive(Parser)]
#[command(name = "pgrep", version, about, max_term_width = MAX_TERM_WIDTH)]
pub struct Args {
    /// Match against the full command line instead of just the process name.
    #[arg(short, long, help_heading = MATCHING)]
    full: bool,

    /// Match processes case-insensitively.
    #[arg(short, long, help_heading = MATCHING)]
    ignore_case: bool,

    /// Only match processes whose names (or command lines if -f) exactly match the pattern.
    #[arg(short = 'x', long, help_heading = MATCHING)]
    exact: bool,

    /// Negates the matching.
    #[arg(short = 'v', long, help_heading = MATCHING)]
    inverse: bool,

    /// Match only processes which match the process state.
    #[arg(short = 'r', long, value_delimiter = ',', help_heading = FILTERS)]
    runstates: Option<Vec<char>>,

    /// Match against process environment: `NAME` (any value) or
    /// `NAME=VALUE` (exact pair). Reads `/proc/[pid]/environ`;
    /// processes whose environ is unreadable are skipped.
    #[arg(long, value_name = "NAME[=VALUE]", help_heading = FILTERS)]
    env: Option<String>,

    /// Select processes older than the given number of seconds.
    #[arg(short = 'O', long, help_heading = FILTERS)]
    older: Option<f64>,

    /// Only match processes whose process ID is listed.
    #[arg(short = 'p', long = "pid", value_delimiter = ',', help_heading = FILTERS)]
    pid: Option<Vec<i32>>,

    /// Read process IDs from FILE, one per line. Cannot be combined
    /// with `-p`.
    #[arg(
        short = 'F',
        long = "pidfile",
        value_name = "FILE",
        conflicts_with = "pid",
        help_heading = FILTERS,
    )]
    pidfile: Option<std::path::PathBuf>,

    /// Only match processes whose parent process ID is listed.
    #[arg(short = 'P', long, value_delimiter = ',', help_heading = FILTERS)]
    parent: Option<Vec<i32>>,

    /// Only match processes in the process group IDs listed.
    #[arg(short = 'g', long = "pgroup", value_delimiter = ',', help_heading = FILTERS)]
    pgroup: Option<Vec<i32>>,

    /// Only match processes whose real group ID is listed.
    #[arg(short = 'G', long = "group", value_delimiter = ',', help_heading = FILTERS)]
    group: Option<Vec<u32>>,

    /// Only match processes whose process session ID is listed.
    #[arg(short = 's', long, value_delimiter = ',', help_heading = FILTERS)]
    session: Option<Vec<i32>>,

    /// Only match processes whose controlling terminal is listed.
    #[arg(short = 't', long = "terminal", value_delimiter = ',', help_heading = FILTERS)]
    terminal: Option<Vec<String>>,

    /// Only match processes whose effective user ID is listed.
    #[arg(short = 'u', long = "euid", value_delimiter = ',', help_heading = FILTERS)]
    euid: Option<Vec<String>>,

    /// Only match processes whose real user ID is listed.
    #[arg(short = 'U', long = "uid", value_delimiter = ',', help_heading = FILTERS)]
    uid: Option<Vec<String>>,

    /// Select only the newest (most recently started) of the matching processes.
    #[arg(short, long, help_heading = SELECTION)]
    newest: bool,

    /// Select only the oldest (least recently started) of the matching processes.
    #[arg(short, long, help_heading = SELECTION)]
    oldest: bool,

    /// Suppress normal output; instead print a count of matching processes.
    #[arg(short, long, help_heading = OUTPUT)]
    count: bool,

    /// Sets the string used to delimit each process ID in the output.
    #[arg(short, long, default_value = "\n", help_heading = OUTPUT)]
    delimiter: String,

    /// List the process name as well as the process ID.
    #[arg(short, long, help_heading = OUTPUT)]
    list_name: bool,

    /// List the full command line as well as the process ID.
    #[arg(short = 'a', long, help_heading = OUTPUT)]
    list_full: bool,

    /// Suppress output. Exits 0 if any process matched, 1 otherwise.
    #[arg(short = 'q', long, help_heading = OUTPUT)]
    quiet: bool,

    /// Shell-quote the name field with single quotes when it contains
    /// spaces or other shell-unsafe characters. Only meaningful with
    /// `-l` or `-a`.
    #[arg(short = 'Q', help_heading = OUTPUT)]
    shell_quote: bool,

    /// Extended regular expression pattern.
    pattern: Option<String>,
}

/// Wrap `s` in single quotes for shell consumption if it's empty or
/// contains a shell-unsafe character; otherwise pass through unchanged.
/// A single quote inside the string is escaped sh-style as `'\''`.
fn shell_quote(s: &str) -> String {
    let unsafe_char = |c: char| c.is_whitespace() || "'\"\\$`;|&()<>*?[]{}#~!".contains(c);
    if s.is_empty() || s.chars().any(unsafe_char) {
        format!("'{}'", s.replace('\'', r"'\''"))
    } else {
        s.to_string()
    }
}

impl From<&Args> for MatchOptions {
    fn from(args: &Args) -> Self {
        Self {
            pattern: args.pattern.clone().unwrap_or_default(),
            full: args.full,
            ignore_case: args.ignore_case,
            exact: args.exact,
            inverse: args.inverse,
            newest: args.newest,
            oldest: args.oldest,
            older: args.older,
            pid: args.pid.clone(),
            parent: args.parent.clone(),
            pgroup: args.pgroup.clone(),
            group: args.group.clone(),
            session: args.session.clone(),
            terminal: args.terminal.clone(),
            euid: args.euid.clone(),
            uid: args.uid.clone(),
            runstates: args.runstates.clone(),
            env: args.env.clone(),
        }
    }
}

pub fn run(args: Args) -> ExitCode {
    let mut opts = MatchOptions::from(&args);

    if let Some(path) = args.pidfile.as_deref() {
        match procutils_common::procmatch::read_pidfile(path) {
            Ok(pids) => opts.pid = Some(pids),
            Err(e) => {
                eprintln!("pgrep: {e}");
                return ExitCode::from(2);
            }
        }
    }

    if args.pattern.is_none() && !opts.has_filter() {
        eprintln!("pgrep: pattern is required");
        return ExitCode::from(2);
    }

    // The kernel truncates `comm` at TASK_COMM_LEN (15 chars + nul), so a
    // longer pattern can never match the comm field. Without -f, warn.
    if let Some(p) = args.pattern.as_deref()
        && !args.full
        && p.len() > 15
    {
        eprintln!(
            "pgrep: pattern that searches for process name longer than 15 characters will result in zero matches"
        );
    }

    let matches = match procutils_common::procmatch::find_matching_processes(&opts, "pgrep") {
        Ok(m) => m,
        Err(code) => return code,
    };

    if args.count && !args.quiet {
        println!("{}", matches.len());
    }

    if matches.is_empty() {
        return ExitCode::from(1);
    }

    if args.quiet || args.count {
        return ExitCode::SUCCESS;
    }

    let format_name = |s: &str| {
        if args.shell_quote {
            shell_quote(s)
        } else {
            s.to_string()
        }
    };
    let output: Vec<String> = matches
        .iter()
        .map(|p| {
            if args.list_full {
                format!("{} {}", p.pid, format_name(&p.cmdline))
            } else if args.list_name {
                format!("{} {}", p.pid, format_name(&p.comm))
            } else {
                format!("{}", p.pid)
            }
        })
        .collect();
    println!("{}", output.join(&args.delimiter));

    ExitCode::SUCCESS
}
