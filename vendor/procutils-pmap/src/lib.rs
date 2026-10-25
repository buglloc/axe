use clap::Parser;
use procfs::process::{MMPermissions, MMapPath, MemoryMap, Process};
use procutils_common::{MAX_TERM_WIDTH, man::ManContent};
use std::process::ExitCode;

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

/// Report memory map of a process.
#[derive(Parser)]
#[command(
    name = "pmap",
    version,
    about,
    max_term_width = MAX_TERM_WIDTH,
    override_usage = "pmap [options] PID [PID ...]"
)]
pub struct Args {
    /// Show the extended format.
    #[arg(short = 'x', long)]
    extended: bool,

    /// Show the device format.
    #[arg(short, long)]
    device: bool,

    /// Do not display some header or footer lines.
    #[arg(short, long)]
    quiet: bool,

    /// Show full path to files in the mapping column.
    #[arg(short = 'p', long)]
    show_path: bool,

    /// Show kernel-supplied mapping names (`[vdso]`, `[vsyscall]`,
    /// `[stack]`, `[heap]`, `[vvar]`) verbatim, without our default
    /// padded form (`[ vdso ]` etc).
    #[arg(short = 'k', long = "use-kernel-name")]
    use_kernel_name: bool,

    /// Limit results to the given address range. Accepts a single
    /// address or a comma-separated `LOW,HIGH` pair. Hex with or
    /// without `0x` prefix.
    #[arg(short = 'A', long = "range", value_name = "LOW[,HIGH]")]
    range: Option<String>,

    /// Process IDs.
    #[arg(required = true)]
    pid: Vec<i32>,
}

/// Parse `-A LOW` or `-A LOW,HIGH`. A single value sets HIGH = LOW
/// (interpret as "any mapping containing this address").
fn parse_range(s: &str) -> Result<(u64, u64), String> {
    fn parse_addr(a: &str) -> Result<u64, String> {
        let stripped = a.trim_start_matches("0x").trim_start_matches("0X");
        u64::from_str_radix(stripped, 16).map_err(|_| format!("invalid address: {a}"))
    }
    match s.split_once(',') {
        Some((lo, hi)) => Ok((parse_addr(lo)?, parse_addr(hi)?)),
        None => {
            let v = parse_addr(s)?;
            Ok((v, v))
        }
    }
}

/// True if `[map_start, map_end)` overlaps the inclusive range `[low, high]`.
fn in_range(map_start: u64, map_end: u64, low: u64, high: u64) -> bool {
    map_start <= high && map_end > low
}

const ADDRESS_WIDTH: usize = std::mem::size_of::<usize>() * 2;

fn format_perms_pmap(perms: MMPermissions) -> String {
    let r = if perms.contains(MMPermissions::READ) {
        'r'
    } else {
        '-'
    };
    let w = if perms.contains(MMPermissions::WRITE) {
        'w'
    } else {
        '-'
    };
    let x = if perms.contains(MMPermissions::EXECUTE) {
        'x'
    } else {
        '-'
    };
    let s = if perms.contains(MMPermissions::SHARED) {
        's'
    } else {
        '-'
    };
    // pmap uses 5-char format: "rwx-s" or "rwx--"
    format!("{r}{w}{x}{s}-")
}

fn format_mapping(
    map: &MemoryMap,
    show_path: bool,
    kernel_name: bool,
    stack_start: Option<u64>,
) -> String {
    if matches!(&map.pathname, MMapPath::Vsys(_)) {
        return format!("  [ shmid=0x{:x} ]", map.inode);
    }
    if let MMapPath::Path(path) = &map.pathname {
        let path = path.to_string_lossy();
        if let Some(last_slash) = path.rfind('/') {
            let start = if show_path {
                path.find('/').unwrap()
            } else if last_slash + 1 == path.len() {
                last_slash
            } else {
                last_slash + 1
            };
            return path[start..].to_string();
        }
    }
    if let MMapPath::Other(name) = &map.pathname
        && let Some(last_slash) = name.rfind('/')
    {
        let start = if show_path {
            name.find('/').unwrap()
        } else {
            last_slash + 1
        };
        return format!("{}]", &name[start..]);
    }
    if kernel_name {
        match &map.pathname {
            MMapPath::Heap => return "[heap]".into(),
            MMapPath::Stack => return "[stack]".into(),
            MMapPath::TStack(tid) => return format!("[stack:{tid}]"),
            MMapPath::Vdso => return "[vdso]".into(),
            MMapPath::Vvar => return "[vvar]".into(),
            MMapPath::Vsyscall => return "[vsyscall]".into(),
            MMapPath::Rollup => return "[rollup]".into(),
            MMapPath::Other(name) => return format!("[{name}]"),
            _ => {}
        }
    }
    if stack_start.is_some_and(|stack| stack >= map.address.0 && stack <= map.address.1) {
        "  [ stack ]".into()
    } else {
        "  [ anon ]".into()
    }
}

fn kbytes(map: &MemoryMap) -> u64 {
    (map.address.1 - map.address.0) / 1024
}

pub fn run(args: Args) -> ExitCode {
    let range = match args.range.as_deref().map(parse_range) {
        Some(Ok(r)) => Some(r),
        Some(Err(e)) => {
            eprintln!("pmap: {e}");
            return ExitCode::from(2);
        }
        None => None,
    };

    let mut not_found = false;

    for &pid in &args.pid {
        let proc = match Process::new(pid) {
            Ok(p) => p,
            Err(e) => {
                if !matches!(e, procfs::ProcError::NotFound(_)) {
                    eprintln!("pmap: {pid}: {e}");
                }
                not_found = true;
                continue;
            }
        };

        let cmdline = proc.cmdline().unwrap_or_default().join(" ");
        let stack_start = proc.stat().ok().map(|stat| stat.startstack);

        println!("{pid}:   {cmdline}");

        if args.extended {
            show_extended(&proc, &args, range, stack_start, pid, &mut not_found);
        } else if args.device {
            show_device(&proc, &args, range, stack_start, pid, &mut not_found);
        } else {
            show_default(&proc, &args, range, stack_start, pid, &mut not_found);
        }
    }

    if not_found {
        ExitCode::from(42)
    } else {
        ExitCode::SUCCESS
    }
}

fn show_default(
    proc: &Process,
    args: &Args,
    range: Option<(u64, u64)>,
    stack_start: Option<u64>,
    pid: i32,
    not_found: &mut bool,
) {
    let maps = match proc.maps() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pmap: {pid}: {e}");
            *not_found = true;
            return;
        }
    };

    let mut total_kb = 0;
    for m in &maps.0 {
        if let Some((lo, hi)) = range
            && !in_range(m.address.0, m.address.1, lo, hi)
        {
            continue;
        }
        let kb = kbytes(m);
        total_kb += kb;
        println!(
            "{:0width$x} {:>6}K {} {}",
            m.address.0,
            kb,
            format_perms_pmap(m.perms),
            format_mapping(m, args.show_path, args.use_kernel_name, stack_start),
            width = ADDRESS_WIDTH,
        );
    }

    if !args.quiet {
        println!(" total {total_kb:>ADDRESS_WIDTH$}K");
    }
}

fn show_extended(
    proc: &Process,
    args: &Args,
    range: Option<(u64, u64)>,
    stack_start: Option<u64>,
    pid: i32,
    not_found: &mut bool,
) {
    let maps = match proc.smaps() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pmap: {pid}: {e}");
            *not_found = true;
            return;
        }
    };

    if !args.quiet {
        println!(
            "{:<width$} {:>7} {:>7} {:>7} {:<5} Mapping",
            "Address",
            "Kbytes",
            "RSS",
            "Dirty",
            "Mode",
            width = ADDRESS_WIDTH,
        );
    }
    let mut total_kb = 0u64;
    let mut total_rss = 0u64;
    let mut total_dirty = 0u64;

    for m in &maps.0 {
        if let Some((lo, hi)) = range
            && !in_range(m.address.0, m.address.1, lo, hi)
        {
            continue;
        }
        let kb = kbytes(m);
        let rss = m.extension.map.get("Rss").copied().unwrap_or(0) / 1024;
        let dirty = (m.extension.map.get("Private_Dirty").copied().unwrap_or(0)
            + m.extension.map.get("Shared_Dirty").copied().unwrap_or(0))
            / 1024;

        total_kb += kb;
        total_rss += rss;
        total_dirty += dirty;

        println!(
            "{:0width$x} {kb:>7} {rss:>7} {dirty:>7} {} {}",
            m.address.0,
            format_perms_pmap(m.perms),
            format_mapping(m, args.show_path, args.use_kernel_name, stack_start),
            width = ADDRESS_WIDTH,
        );
    }

    if !args.quiet {
        println!("{:-<ADDRESS_WIDTH$} ------- ------- ------- ", "");
        println!(
            "{:<width$} {:>7} {:>7} {:>7}",
            "total kB",
            total_kb,
            total_rss,
            total_dirty,
            width = ADDRESS_WIDTH,
        );
    }
}

fn show_device(
    proc: &Process,
    args: &Args,
    range: Option<(u64, u64)>,
    stack_start: Option<u64>,
    pid: i32,
    not_found: &mut bool,
) {
    let maps = match proc.maps() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pmap: {pid}: {e}");
            *not_found = true;
            return;
        }
    };

    if !args.quiet {
        println!(
            "{:<width$} {:>7} {:<5} {:<16} {:<9} Mapping",
            "Address",
            "Kbytes",
            "Mode",
            "Offset",
            "Device",
            width = ADDRESS_WIDTH,
        );
    }
    let mut total_kb = 0u64;
    let mut total_writable_private = 0u64;
    let mut total_shared = 0u64;

    for m in &maps.0 {
        if let Some((lo, hi)) = range
            && !in_range(m.address.0, m.address.1, lo, hi)
        {
            continue;
        }
        let kb = kbytes(m);
        total_kb += kb;

        if m.perms.contains(MMPermissions::SHARED) {
            total_shared += kb;
        }
        if m.perms.contains(MMPermissions::WRITE) && m.perms.contains(MMPermissions::PRIVATE) {
            total_writable_private += kb;
        }

        println!(
            "{:0width$x} {kb:>7} {} {:016x} {:03x}:{:05x} {}",
            m.address.0,
            format_perms_pmap(m.perms),
            m.offset,
            m.dev.0,
            m.dev.1,
            format_mapping(m, args.show_path, args.use_kernel_name, stack_start),
            width = ADDRESS_WIDTH,
        );
    }

    if !args.quiet {
        println!(
            "mapped: {total_kb}K    writeable/private: {total_writable_private}K    shared: {total_shared}K",
        );
    }
}
