#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use serde_json::{Map as JsonMap, Value as JsonValue};

pub type AppletFn = fn(Vec<OsString>) -> i32;

pub fn commands() -> HashMap<String, AppletFn> {
    let mut commands = HashMap::new();
    commands.insert("fuser".into(), fuser as AppletFn);
    commands.insert("lsblk".into(), lsblk as AppletFn);
    commands.insert("lslocks".into(), lslocks as AppletFn);
    commands.insert("namei".into(), namei as AppletFn);
    commands.insert("pstree".into(), pstree as AppletFn);
    commands
}

fn utf8_args(args: Vec<OsString>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|value| {
            value.into_string().map_err(|value| {
                format!("argument is not valid UTF-8: {}", value.to_string_lossy())
            })
        })
        .collect()
}

fn text(value: &OsStr) -> String {
    String::from_utf8_lossy(value.as_bytes()).into_owned()
}

fn read_trim(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|bytes| {
        String::from_utf8_lossy(&bytes)
            .trim_matches(|character: char| character == '\0' || character.is_whitespace())
            .to_owned()
    })
}

fn user_name(uid: u32) -> String {
    uucore::entries::uid2usr(uid).unwrap_or_else(|_| uid.to_string())
}

fn group_name(gid: u32) -> String {
    uucore::entries::gid2grp(gid).unwrap_or_else(|_| gid.to_string())
}

fn print_error(name: &str, error: String, status: i32) -> i32 {
    eprintln!("{name}: {error}");
    status
}

// -----------------------------------------------------------------------------
// namei

#[derive(Default)]
struct NameiOptions {
    modes: bool,
    owners: bool,
    vertical: bool,
    no_symlinks: bool,
    mountpoints: bool,
}

struct NameiLine {
    component: OsString,
    metadata: Option<Metadata>,
    target: Option<PathBuf>,
    error: Option<String>,
    mountpoint: bool,
    depth: usize,
}

pub fn namei(args: Vec<OsString>) -> i32 {
    match run_namei(args) {
        Ok(status) => status,
        Err(error) => print_error("namei", error, 1),
    }
}

fn run_namei(args: Vec<OsString>) -> Result<i32, String> {
    let mut options = NameiOptions::default();
    let mut operands = Vec::new();
    let mut parse_options = true;

    for argument in args.into_iter().skip(1) {
        if parse_options && argument == OsStr::new("--") {
            parse_options = false;
            continue;
        }
        if parse_options && argument.as_bytes().starts_with(b"-") && argument != OsStr::new("-") {
            let value = argument.to_string_lossy();
            match value.as_ref() {
                "--help" | "-h" => {
                    println!(
                        "Usage: namei [options] <pathname>...\n\
                         -x, --mountpoints  show mount point directories with a 'D'\n\
                         -m, --modes        show mode bits\n\
                         -o, --owners       show owner and group\n\
                         -l, --long         use -m -o -v\n\
                         -n, --nosymlinks   do not follow symbolic links\n\
                         --nolinks          alias for --nosymlinks\n\
                         -v, --vertical     align mode and owner columns"
                    );
                    return Ok(0);
                }
                "--version" | "-V" => {
                    println!("namei (AXE)");
                    return Ok(0);
                }
                "--mountpoints" | "-x" => options.mountpoints = true,
                "--modes" | "-m" => options.modes = true,
                "--owners" | "-o" => options.owners = true,
                "--long" | "-l" => {
                    options.modes = true;
                    options.owners = true;
                    options.vertical = true;
                }
                "--nosymlinks" | "--nolinks" | "-n" => options.no_symlinks = true,
                "--vertical" | "-v" => options.vertical = true,
                "--context" | "-Z" => {
                    return Err("security context support is unavailable".into());
                }
                value if value.starts_with('-') => {
                    let flags = value.strip_prefix('-').unwrap_or_default();
                    if flags.is_empty() {
                        return Err("unknown option '-'".into());
                    }
                    for flag in flags.chars() {
                        match flag {
                            'x' => options.mountpoints = true,
                            'm' => options.modes = true,
                            'o' => options.owners = true,
                            'l' => {
                                options.modes = true;
                                options.owners = true;
                                options.vertical = true;
                            }
                            'n' => options.no_symlinks = true,
                            'v' => options.vertical = true,
                            'Z' => return Err("security context support is unavailable".into()),
                            'h' | 'V' => {
                                return Err(format!("option '-{flag}' must be used alone"));
                            }
                            _ => return Err(format!("unsupported option '-{flag}'")),
                        }
                    }
                }
                _ => operands.push(argument),
            }
        } else {
            operands.push(argument);
        }
    }

    if operands.is_empty() {
        return Err("pathname argument is missing".into());
    }

    let mut failed = false;
    for operand in operands {
        let display = text(&operand);
        println!("f: {display}");
        let mut lines = Vec::new();
        let components = path_components(Path::new(&operand))
            .into_iter()
            .map(|component| (component, 0))
            .collect();
        let base = if Path::new(&operand).is_absolute() {
            PathBuf::from("/")
        } else {
            PathBuf::from(".")
        };
        let mut links = 0;
        let mut path_failed = false;
        namei_walk(
            base,
            components,
            &options,
            &mut lines,
            &mut links,
            &mut path_failed,
        );
        failed |= path_failed;
        print_namei_lines(&lines, &options);
    }
    Ok(if failed { 1 } else { 0 })
}

fn path_components(path: &Path) -> Vec<OsString> {
    path.components()
        .map(|component| OsString::from_vec(component.as_os_str().as_bytes().to_vec()))
        .collect()
}

fn namei_walk(
    base: PathBuf,
    mut components: Vec<(OsString, usize)>,
    options: &NameiOptions,
    lines: &mut Vec<NameiLine>,
    links: &mut usize,
    failed: &mut bool,
) {
    let Some((component, depth)) = components.first().cloned() else {
        return;
    };
    components.remove(0);
    let candidate = if component.as_bytes() == b"/" {
        PathBuf::from("/")
    } else {
        base.join(&component)
    };
    let metadata = fs::symlink_metadata(&candidate);
    let mut target = None;
    let mut link_error = None;
    let mut mountpoint = false;
    match metadata {
        Ok(metadata) => {
            if options.mountpoints && metadata.file_type().is_dir() {
                let current = fs::metadata(&candidate).ok();
                let parent = fs::metadata(&base).ok();
                mountpoint = depth == 0
                    || current
                        .as_ref()
                        .zip(parent.as_ref())
                        .is_some_and(|(current, parent)| current.dev() != parent.dev());
            }
            if metadata.file_type().is_symlink() {
                match fs::read_link(&candidate) {
                    Ok(value) => target = Some(value),
                    Err(error) => link_error = Some(error.to_string()),
                }
            }
            let link_failed = link_error.is_some();
            if link_failed {
                *failed = true;
            }
            lines.push(NameiLine {
                component: component.clone(),
                metadata: Some(metadata),
                target: target.clone(),
                error: link_error,
                mountpoint,
                depth,
            });
            if link_failed {
                return;
            }
        }
        Err(err) => {
            *failed = true;
            lines.push(NameiLine {
                component: component.clone(),
                metadata: None,
                target: None,
                error: Some(err.to_string()),
                mountpoint: false,
                depth,
            });
            return;
        }
    }

    if let Some(target) = target.filter(|_| !options.no_symlinks) {
        *links += 1;
        if *links > 256 {
            *failed = true;
            lines.push(NameiLine {
                component: OsString::from("<symbolic link loop>"),
                metadata: None,
                target: None,
                error: Some("too many levels of symbolic links".into()),
                mountpoint: false,
                depth: depth + 1,
            });
            return;
        }
        let target_base = if target.is_absolute() {
            PathBuf::from("/")
        } else {
            candidate
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        };
        let mut next = path_components(Path::new(&target))
            .into_iter()
            .map(|component| (component, depth + 1))
            .collect::<Vec<_>>();
        next.extend(components);
        namei_walk(target_base, next, options, lines, links, failed);
    } else {
        namei_walk(candidate, components, options, lines, links, failed);
    }
}

fn mode_string(metadata: &Metadata) -> String {
    let mode = metadata.mode();
    let file_type = metadata.file_type();
    let first = if file_type.is_dir() {
        'd'
    } else if file_type.is_symlink() {
        'l'
    } else if file_type.is_char_device() {
        'c'
    } else if file_type.is_block_device() {
        'b'
    } else if file_type.is_fifo() {
        'p'
    } else if file_type.is_socket() {
        's'
    } else {
        '-'
    };
    let mut result = vec![first, '-', '-', '-', '-', '-', '-', '-', '-', '-'];
    for (index, bit) in [
        0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001,
    ]
    .into_iter()
    .enumerate()
    {
        result[index + 1] = if mode & bit == 0 {
            '-'
        } else {
            match index % 3 {
                0 => 'r',
                1 => 'w',
                _ => 'x',
            }
        };
    }
    if mode & 0o4000 != 0 {
        result[3] = if result[3] == 'x' { 's' } else { 'S' };
    }
    if mode & 0o2000 != 0 {
        result[6] = if result[6] == 'x' { 's' } else { 'S' };
    }
    if mode & 0o1000 != 0 {
        result[9] = if result[9] == 'x' { 't' } else { 'T' };
    }
    result.into_iter().collect()
}

fn print_namei_lines(lines: &[NameiLine], options: &NameiOptions) {
    let owner_width = if options.owners && options.vertical {
        lines
            .iter()
            .filter_map(|line| line.metadata.as_ref())
            .map(|metadata| user_name(metadata.uid()).len())
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let group_width = if options.owners && options.vertical {
        lines
            .iter()
            .filter_map(|line| line.metadata.as_ref())
            .map(|metadata| group_name(metadata.gid()).len())
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    for line in lines {
        let indent = " ".repeat(line.depth * 2 + 1);
        let component = text(&line.component);
        if line.metadata.is_none() || line.error.is_some() {
            println!("{component} - {}", line.error.as_deref().unwrap_or("error"));
            continue;
        }
        let Some(metadata) = line.metadata.as_ref() else {
            continue;
        };
        let kind = if line.mountpoint && metadata.file_type().is_dir() {
            'D'
        } else if metadata.file_type().is_dir() {
            'd'
        } else if metadata.file_type().is_symlink() {
            'l'
        } else if metadata.file_type().is_char_device() {
            'c'
        } else if metadata.file_type().is_block_device() {
            'b'
        } else if metadata.file_type().is_fifo() {
            'p'
        } else if metadata.file_type().is_socket() {
            's'
        } else {
            '-'
        };
        if options.modes || options.owners {
            let mode = mode_string(metadata);
            let owner = user_name(metadata.uid());
            let group = group_name(metadata.gid());
            if options.modes && options.owners {
                if options.vertical {
                    print!("{indent}{mode} {owner:owner_width$} {group:group_width$} {component}");
                } else {
                    print!("{indent}{mode} {owner} {group} {component}");
                }
            } else if options.modes {
                print!("{indent}{mode} {component}");
            } else if options.vertical {
                print!("{indent}{owner:owner_width$} {group:group_width$} {component}");
            } else {
                print!("{indent}{owner} {group} {component}");
            }
        } else {
            print!("{indent}{kind} {component}");
        }
        if let Some(target) = &line.target {
            print!(" -> {}", text(target.as_os_str()));
        }
        println!();
    }
}

// -----------------------------------------------------------------------------
// fuser

#[derive(Default)]
struct FuserOptions {
    all: bool,
    silent: bool,
    ipv4: bool,
    ipv6: bool,
    mount: bool,
    namespace: Option<String>,
    users: bool,
    verbose: bool,
    inode: bool,
    is_mountpoint: bool,
    kill: bool,
    interactive: bool,
    writeonly: bool,
    list_signals: bool,
    signal: i32,
}

#[derive(Clone)]
struct ProcessInfo {
    pid: u32,
    uid: u32,
    command: String,
}

struct FdRef {
    name: OsString,
    target: PathBuf,
    metadata: Option<Metadata>,
    socket_inode: Option<u64>,
    writable: bool,
}

#[derive(Clone)]
struct FuserMatch {
    process: ProcessInfo,
    access: String,
}

fn parse_signal_option(value: &str) -> Result<Option<i32>, String> {
    let Some(token) = value
        .strip_prefix('-')
        .filter(|token| !token.starts_with('-'))
    else {
        return Ok(None);
    };
    if token == "4" || token == "6" {
        return Ok(None);
    }
    if token.chars().all(|character| character.is_ascii_digit()) {
        let signal = token
            .parse::<i32>()
            .map_err(|_| format!("invalid signal '{token}'"))?;
        if (0..=64).contains(&signal) {
            return Ok(Some(signal));
        }
        return Err(format!("invalid signal '{token}'"));
    }
    if token
        .chars()
        .all(|character| character.is_ascii_alphabetic())
    {
        let name = token.strip_prefix("SIG").unwrap_or(token);
        return Ok(signal_number(name));
    }
    Ok(None)
}

fn signal_number(name: &str) -> Option<i32> {
    Some(match name.to_ascii_uppercase().as_str() {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "ILL" => libc::SIGILL,
        "TRAP" => libc::SIGTRAP,
        "ABRT" | "IOT" => libc::SIGABRT,
        "BUS" => libc::SIGBUS,
        "FPE" => libc::SIGFPE,
        "KILL" => libc::SIGKILL,
        "USR1" => libc::SIGUSR1,
        "SEGV" => libc::SIGSEGV,
        "USR2" => libc::SIGUSR2,
        "PIPE" => libc::SIGPIPE,
        "ALRM" => libc::SIGALRM,
        "TERM" => libc::SIGTERM,
        "STKFLT" => libc::SIGSTKFLT,
        "CHLD" | "CLD" => libc::SIGCHLD,
        "CONT" => libc::SIGCONT,
        "STOP" => libc::SIGSTOP,
        "TSTP" => libc::SIGTSTP,
        "TTIN" => libc::SIGTTIN,
        "TTOU" => libc::SIGTTOU,
        "URG" => libc::SIGURG,
        "XCPU" => libc::SIGXCPU,
        "XFSZ" => libc::SIGXFSZ,
        "VTALRM" => libc::SIGVTALRM,
        "PROF" => libc::SIGPROF,
        "WINCH" => libc::SIGWINCH,
        "POLL" | "IO" => libc::SIGIO,
        "PWR" => libc::SIGPWR,
        "SYS" => libc::SIGSYS,
        _ => return None,
    })
}

fn signal_names() -> &'static str {
    "HUP INT QUIT ILL TRAP ABRT BUS FPE KILL USR1 SEGV USR2 PIPE ALRM TERM STKFLT\n\
     CHLD CONT STOP TSTP TTIN TTOU URG XCPU XFSZ VTALRM PROF WINCH POLL PWR SYS"
}
pub fn fuser(args: Vec<OsString>) -> i32 {
    match run_fuser(args) {
        Ok(status) => status,
        Err(error) => print_error("fuser", error, 1),
    }
}

fn run_fuser(args: Vec<OsString>) -> Result<i32, String> {
    let mut options = FuserOptions {
        signal: libc::SIGKILL,
        ..FuserOptions::default()
    };
    let mut operands = Vec::new();
    let mut parse_options = true;
    let mut iter = args.into_iter().skip(1);
    while let Some(argument) = iter.next() {
        if parse_options && argument == OsStr::new("--") {
            parse_options = false;
            continue;
        }
        if !parse_options || !argument.as_bytes().starts_with(b"-") || argument == OsStr::new("-") {
            operands.push(argument);
            continue;
        }
        let value = argument.to_string_lossy();
        if value == "--help" {
            eprintln!(
                "Usage: fuser [-avsuIM] [-4|-6] [-c|-m] [-n file|tcp|udp] NAME...\n\
                 -a show unused names, -s silent, -u show users, -v verbose,\n\
                 -I compare inodes, -M require mount point, -k kill (read-only access scan)\n\
                 -i ask before killing, -w kill only writable users, -l list signals\n\
                 -f is accepted and ignored; -SIGNAL and -NUM select a signal"
            );
            return Ok(0);
        }
        if value == "--version" || value == "-V" {
            eprintln!("fuser (AXE)");
            return Ok(0);
        }
        if value == "--namespace" || value == "-n" {
            options.namespace = Some(
                iter.next()
                    .ok_or("-n/--namespace requires file, tcp, or udp")?
                    .into_string()
                    .map_err(|_| "namespace is not valid UTF-8")?,
            );
            continue;
        }
        if let Some(namespace) = value.strip_prefix("--namespace=") {
            options.namespace = Some(namespace.to_owned());
            continue;
        }
        match value.as_ref() {
            "--all" => options.all = true,
            "--silent" => options.silent = true,
            "--user" => options.users = true,
            "--verbose" => options.verbose = true,
            "--inode" => options.inode = true,
            "--ismountpoint" => options.is_mountpoint = true,
            "--ipv4" => options.ipv4 = true,
            "--ipv6" => options.ipv6 = true,
            "--mount" => options.mount = true,
            "--kill" => options.kill = true,
            "--interactive" => options.interactive = true,
            "--writeonly" => options.writeonly = true,
            "--list-signals" => options.list_signals = true,
            "--force" => {}
            _ => {
                if let Some(signal) = parse_signal_option(&value)? {
                    options.signal = signal;
                    options.kill = true;
                    continue;
                }
                let flags = value
                    .strip_prefix('-')
                    .ok_or_else(|| format!("unexpected operand '{value}'"))?;
                if flags.is_empty() {
                    return Err("unknown option '-'".into());
                }
                for flag in flags.chars() {
                    match flag {
                        'a' => options.all = true,
                        's' => options.silent = true,
                        '4' => options.ipv4 = true,
                        '6' => options.ipv6 = true,
                        'c' | 'm' => options.mount = true,
                        'u' => options.users = true,
                        'v' => options.verbose = true,
                        'I' => options.inode = true,
                        'M' => options.is_mountpoint = true,
                        'f' => {}
                        'k' => options.kill = true,
                        'i' => options.interactive = true,
                        'w' => options.writeonly = true,
                        'l' => options.list_signals = true,
                        'V' | 'h' => return Err(format!("option '-{flag}' must be used alone")),
                        _ => return Err(format!("unsupported option '-{flag}'")),
                    }
                }
            }
        }
    }
    if options.list_signals {
        println!("{}", signal_names());
        return Ok(0);
    }
    if options.ipv4 && options.ipv6 {
        return Err("-4 and -6 are mutually exclusive".into());
    }
    if options.namespace.is_none() {
        let mut detected = None;
        for operand in &mut operands {
            let value = text(operand);
            let Some((port, namespace)) = value.rsplit_once('/') else {
                continue;
            };
            if !matches!(namespace, "tcp" | "udp") || port.parse::<u16>().is_err() {
                continue;
            }
            if detected
                .as_deref()
                .is_some_and(|current| current != namespace)
            {
                return Err("tcp and udp namespace shortcuts cannot be combined".into());
            }
            detected = Some(namespace.to_owned());
            *operand = OsString::from(port);
        }
        options.namespace = detected;
    }
    if operands.is_empty() {
        return Err("missing file, socket, or namespace operand".into());
    }
    let namespace = options.namespace.as_deref().unwrap_or("file");
    if matches!(namespace, "tcp" | "udp") && (options.mount || options.is_mountpoint) {
        return Err("mount options cannot be combined with tcp or udp namespace".into());
    }
    let processes = list_processes().map_err(|error| format!("cannot read /proc: {error}"))?;
    let mountpoints = if options.is_mountpoint {
        mount_points().unwrap_or_default()
    } else {
        HashSet::new()
    };
    let mut any = false;
    let mut had_error = false;
    if matches!(namespace, "tcp" | "udp") {
        let queries = operands
            .iter()
            .map(parse_socket_query)
            .collect::<Result<Vec<_>, _>>()?;
        let sockets = socket_entries(namespace, &options);
        for (operand, query) in operands.iter().zip(queries) {
            let matches = network_matches(&processes, &sockets, &query, &options);
            let (matches, kill_failed) = apply_kill(matches, &options);
            any |= !matches.is_empty();
            had_error |= kill_failed;
            print_fuser_matches(&text(operand), &matches, &options, true);
        }
    } else if namespace != "file" {
        return Err(format!("unsupported namespace '{namespace}'"));
    } else {
        for operand in operands {
            let path = Path::new(&operand);
            let metadata = match fs::metadata(path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    eprintln!("fuser: {}: {error}", text(&operand));
                    had_error = true;
                    continue;
                }
            };
            if options.is_mountpoint && !mountpoints.contains(&path.to_path_buf()) {
                print_fuser_matches(&text(&operand), &[], &options, false);
                continue;
            }
            let matches = file_matches(&processes, &metadata, &options);
            let (matches, kill_failed) = apply_kill(matches, &options);
            any |= !matches.is_empty();
            had_error |= kill_failed;
            print_fuser_matches(&text(&operand), &matches, &options, false);
        }
    }
    if had_error {
        Ok(1)
    } else if any {
        Ok(0)
    } else {
        Ok(1)
    }
}

fn apply_kill(matches: Vec<FuserMatch>, options: &FuserOptions) -> (Vec<FuserMatch>, bool) {
    if !options.kill {
        return (matches, false);
    }
    // SAFETY: getpid has no arguments or caller-side memory requirements.
    let self_pid = unsafe { libc::getpid() as u32 };
    let mut successful = Vec::new();
    let mut failed = false;
    for matched in matches {
        if matched.process.pid == self_pid {
            eprintln!("fuser: refusing to signal own process {}", self_pid);
            failed = true;
            continue;
        }
        if options.interactive {
            eprint!(
                "Kill process {} ({})? ",
                matched.process.pid, matched.process.command
            );
            let mut answer = String::new();
            if io::stdin().read_line(&mut answer).is_err()
                || !matches!(answer.trim().as_bytes().first(), Some(b'y' | b'Y'))
            {
                continue;
            }
        }
        // SAFETY: pid and signal are plain integers; the kernel validates both.
        let result = unsafe { libc::kill(matched.process.pid as libc::pid_t, options.signal) };
        if result == 0 {
            successful.push(matched);
        } else {
            failed = true;
            eprintln!(
                "fuser: cannot signal {}: {}",
                matched.process.pid,
                io::Error::last_os_error()
            );
        }
    }
    (successful, failed)
}

fn list_processes() -> io::Result<Vec<ProcessInfo>> {
    let mut processes = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let uid = fs::metadata(entry.path())
            .map(|metadata| metadata.uid())
            .unwrap_or(0);
        let command = read_trim(&entry.path().join("comm")).unwrap_or_else(|| pid.to_string());
        processes.push(ProcessInfo { pid, uid, command });
    }
    processes.sort_unstable_by_key(|process| process.pid);
    Ok(processes)
}

fn process_fds(pid: u32) -> Vec<FdRef> {
    let mut result = Vec::new();
    for name in ["cwd", "root", "exe"] {
        let path = PathBuf::from(format!("/proc/{pid}/{name}"));
        if let Ok(target) = fs::read_link(&path) {
            result.push(FdRef {
                name: OsString::from(name),
                metadata: fs::metadata(&path).ok(),
                socket_inode: socket_inode(target.as_os_str()),
                writable: false,
                target,
            });
        }
    }
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return result;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        result.push(FdRef {
            writable: fd_is_writable(pid, &name),
            name,
            metadata: fs::metadata(entry.path()).ok(),
            socket_inode: socket_inode(target.as_os_str()),
            target,
        });
    }
    result
}

fn fd_is_writable(pid: u32, fd: &OsStr) -> bool {
    let Some(fd) = fd.to_str() else {
        return false;
    };
    let Ok(content) = fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")) else {
        return false;
    };
    content
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("flags:")?.trim();
            u32::from_str_radix(value, 8)
                .ok()
                .map(|flags| flags & 3 != 0)
        })
        .unwrap_or(false)
}

fn socket_inode(target: &OsStr) -> Option<u64> {
    let value = text(target);
    value
        .strip_prefix("socket:[")
        .and_then(|value| value.strip_suffix(']'))
        .and_then(|value| value.parse().ok())
}

fn file_matches(
    processes: &[ProcessInfo],
    target: &Metadata,
    options: &FuserOptions,
) -> Vec<FuserMatch> {
    let target_dev = target.dev();
    let target_ino = target.ino();
    let mut matches = Vec::new();
    for process in processes {
        let mut access = None;
        for fd in process_fds(process.pid) {
            if options.writeonly && !fd.writable {
                continue;
            }
            let Some(metadata) = fd.metadata else {
                continue;
            };
            let matched = if options.mount {
                metadata.dev() == target_dev
            } else if options.inode {
                metadata.ino() == target_ino
            } else {
                metadata.dev() == target_dev && metadata.ino() == target_ino
            };
            if matched {
                access = Some(if fd.name == OsStr::new("cwd") {
                    "c"
                } else if fd.name == OsStr::new("root") {
                    "r"
                } else if fd.name == OsStr::new("exe") {
                    "e"
                } else if fd.writable {
                    "F"
                } else {
                    "f"
                });
                break;
            }
        }
        if let Some(access) = access {
            matches.push(FuserMatch {
                process: process.clone(),
                access: access.into(),
            });
        }
    }
    matches
}

#[derive(Clone, Copy)]
enum SocketFamily {
    V4,
    V6,
}

#[derive(Clone)]
struct SocketEntry {
    inode: u64,
    local_port: u16,
    remote_host: String,
    remote_port: u16,
}

#[derive(Default)]
struct SocketQuery {
    local_port: Option<u16>,
    remote_host: Option<String>,
    remote_port: Option<u16>,
}

fn parse_socket_query(argument: &OsString) -> Result<SocketQuery, String> {
    let value = text(argument);
    if value.is_empty() {
        return Err("empty socket specification".into());
    }
    let fields = value.split(',').collect::<Vec<_>>();
    if fields.len() > 3 {
        return Err(format!("invalid socket specification '{value}'"));
    }
    let local_port = (!fields[0].is_empty())
        .then(|| {
            fields[0]
                .parse()
                .map_err(|_| format!("invalid local port '{}'", fields[0]))
        })
        .transpose()?;
    let remote_host = fields
        .get(1)
        .filter(|field| !field.is_empty())
        .map(|field| (*field).to_owned());
    let remote_port = fields
        .get(2)
        .filter(|field| !field.is_empty())
        .map(|field| {
            field
                .parse()
                .map_err(|_| format!("invalid remote port '{field}'"))
        })
        .transpose()?;
    Ok(SocketQuery {
        local_port,
        remote_host,
        remote_port,
    })
}

fn socket_entries(namespace: &str, options: &FuserOptions) -> Vec<SocketEntry> {
    let mut entries = Vec::new();
    let families = if options.ipv4 {
        vec![(SocketFamily::V4, format!("/proc/net/{namespace}"))]
    } else if options.ipv6 {
        vec![(SocketFamily::V6, format!("/proc/net/{namespace}6"))]
    } else {
        vec![
            (SocketFamily::V4, format!("/proc/net/{namespace}")),
            (SocketFamily::V6, format!("/proc/net/{namespace}6")),
        ]
    };
    for (family, path) in families {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines().skip(1) {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 10 {
                continue;
            }
            let Some((_local_host, local_port)) = parse_hex_endpoint(fields[1], family) else {
                continue;
            };
            let Some((remote_host, remote_port)) = parse_hex_endpoint(fields[2], family) else {
                continue;
            };
            let Some(inode) = fields[9].parse().ok() else {
                continue;
            };
            entries.push(SocketEntry {
                inode,
                local_port,
                remote_host,
                remote_port,
            });
        }
    }
    entries
}

fn parse_hex_endpoint(value: &str, family: SocketFamily) -> Option<(String, u16)> {
    let (address, port) = value.rsplit_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let host = match family {
        SocketFamily::V4 if address.len() == 8 => {
            let number = u32::from_str_radix(address, 16).ok()?;
            std::net::Ipv4Addr::from(number.to_le_bytes()).to_string()
        }
        SocketFamily::V6 => address.to_owned(),
        _ => address.to_owned(),
    };
    Some((host, port))
}

fn network_matches(
    processes: &[ProcessInfo],
    sockets: &[SocketEntry],
    query: &SocketQuery,
    options: &FuserOptions,
) -> Vec<FuserMatch> {
    let matching_inodes = sockets
        .iter()
        .filter(|socket| {
            query
                .local_port
                .is_none_or(|port| port == socket.local_port)
                && query
                    .remote_port
                    .is_none_or(|port| port == socket.remote_port)
                && query
                    .remote_host
                    .as_ref()
                    .is_none_or(|host| host == &socket.remote_host)
        })
        .map(|socket| socket.inode)
        .collect::<HashSet<_>>();
    let mut matches = Vec::new();
    for process in processes {
        let matched = process_fds(process.pid).into_iter().any(|fd| {
            (!options.writeonly || fd.writable)
                && fd
                    .socket_inode
                    .is_some_and(|inode| matching_inodes.contains(&inode))
        });
        if matched {
            matches.push(FuserMatch {
                process: process.clone(),
                access: "f".into(),
            });
        }
    }
    matches
}

fn mount_points() -> io::Result<HashSet<PathBuf>> {
    let mut result = HashSet::new();
    for line in fs::read_to_string("/proc/self/mountinfo")?.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() > 4 {
            result.insert(PathBuf::from(unescape_mount_field(fields[4])));
        }
    }
    Ok(result)
}

fn unescape_mount_field(value: &str) -> String {
    let mut result = String::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && let Ok(number) = u8::from_str_radix(&value[index + 1..index + 4], 8)
        {
            result.push(number as char);
            index += 4;
            continue;
        }
        result.push(bytes[index] as char);
        index += 1;
    }
    result
}

fn print_fuser_matches(label: &str, matches: &[FuserMatch], options: &FuserOptions, network: bool) {
    if options.silent {
        return;
    }
    if options.verbose {
        eprintln!("                     USER        PID ACCESS COMMAND");
        if matches.is_empty() && options.all {
            eprintln!("{label:<21}                     -");
        }
        for matched in matches {
            eprintln!(
                "{label:<21} {:<10} {:>7} {:<6} {}",
                user_name(matched.process.uid),
                matched.process.pid,
                matched.access,
                matched.process.command
            );
        }
        return;
    }
    if options.users && !matches.is_empty() {
        eprintln!(
            "{label}: {}",
            matches
                .iter()
                .map(|matched| user_name(matched.process.uid))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    if options.kill {
        if matches.is_empty() && options.all {
            eprintln!("{label}:{}", if network { " -" } else { "" });
        }
        return;
    }
    if matches.is_empty() {
        if options.all {
            eprintln!("{label}:{}", if network { " -" } else { "" });
        }
        return;
    }
    for (index, matched) in matches.iter().enumerate() {
        if index != 0 {
            print!(" ");
        }
        print!("{}", matched.process.pid);
    }
    println!();
}

// -----------------------------------------------------------------------------
// lslocks

#[derive(Clone, Debug, PartialEq, Eq)]
struct LockRecord {
    id: u64,
    lock_type: String,
    mandatory: bool,
    mode: String,
    pid: i64,
    major: u64,
    minor: u64,
    inode: u64,
    start: u64,
    end: Option<u64>,
    path: Option<String>,
    file_size: Option<u64>,
    command: String,
}

#[derive(Default)]
struct LocksOptions {
    bytes: bool,
    no_headings: bool,
    columns: Option<Vec<String>>,
    output_append: bool,
    output_all: bool,
    pid: Option<i64>,
    raw: bool,
    no_truncate: bool,
    json: bool,
    no_inaccessible: bool,
}

pub fn lslocks(args: Vec<OsString>) -> i32 {
    match run_lslocks(args) {
        Ok(()) => 0,
        Err(error) => print_error("lslocks", error, 1),
    }
}

fn run_lslocks(args: Vec<OsString>) -> Result<(), String> {
    let mut options = LocksOptions::default();
    let mut iter = utf8_args(args)?.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-b" | "--bytes" => options.bytes = true,
            "-n" | "--noheadings" => options.no_headings = true,
            "-r" | "--raw" => options.raw = true,
            "-u" | "--notruncate" => options.no_truncate = true,
            "-i" | "--noinaccessible" => options.no_inaccessible = true,
            "-J" | "--json" => options.json = true,
            "-O" | "--output-all" => options.output_all = true,
            "-H" | "--list-columns" => {
                print_lock_columns();
                return Ok(());
            }
            "-h" | "--help" => {
                println!(
                    "Usage: lslocks [-bJinru] [-o LIST] [-O] [-p PID]\n\
                     List local system locks from /proc/locks."
                );
                return Ok(());
            }
            "-V" | "--version" => {
                println!("lslocks (AXE)");
                return Ok(());
            }
            "-p" | "--pid" => {
                options.pid = Some(
                    iter.next()
                        .ok_or("-p/--pid requires a PID")?
                        .parse()
                        .map_err(|_| "invalid PID")?,
                );
            }
            "-o" | "--output" => {
                let value = iter.next().ok_or("-o/--output requires a list")?;
                options.output_append |= value
                    .split([',', ' ', '\t'])
                    .any(|part| part.starts_with('+'));
                options.columns = Some(parse_lock_columns(&value)?);
            }
            value if value.starts_with("--output=") => {
                options.output_append |= value[9..]
                    .split([',', ' ', '\t'])
                    .any(|part| part.starts_with('+'));
                options.columns = Some(parse_lock_columns(&value[9..])?);
            }
            value if value.starts_with("-o") && value.len() > 2 => {
                options.output_append |= value[2..]
                    .split([',', ' ', '\t'])
                    .any(|part| part.starts_with('+'));
                options.columns = Some(parse_lock_columns(&value[2..])?);
            }
            value if value.starts_with('-') => return Err(format!("unsupported option '{value}'")),
            value => return Err(format!("unexpected operand '{value}'")),
        }
    }
    if options.json && options.raw {
        return Err("options --json and --raw cannot be combined".into());
    }
    let content =
        fs::read_to_string("/proc/locks").map_err(|error| format!("/proc/locks: {error}"))?;
    let mut locks = content
        .lines()
        .filter_map(parse_lock_line)
        .filter(|record| options.pid.is_none_or(|pid| pid == record.pid))
        .collect::<Vec<_>>();
    for record in &mut locks {
        if let Some((path, size)) = resolve_lock_path(record) {
            record.path = Some(path);
            record.file_size = size;
        }
        if record.command.is_empty() {
            record.command = if record.pid >= 0 {
                read_trim(Path::new(&format!("/proc/{}/comm", record.pid)))
                    .unwrap_or_else(|| "?".into())
            } else {
                "-".into()
            };
        }
    }
    if options.no_inaccessible {
        locks.retain(|record| record.path.is_some());
    }
    let defaults = default_lock_columns();
    let columns = if options.output_all {
        all_lock_columns()
    } else if options.output_append {
        let mut columns = defaults;
        if let Some(extra) = options.columns.as_ref() {
            columns.extend(extra.iter().cloned());
        }
        columns.dedup();
        columns
    } else if let Some(columns) = options.columns.as_ref() {
        columns.clone()
    } else {
        defaults
    };
    if options.json {
        print_locks_json(&locks, &columns, &options);
    } else {
        print_locks_table(&locks, &columns, &options);
    }
    Ok(())
}

fn parse_lock_columns(value: &str) -> Result<Vec<String>, String> {
    let columns = value
        .split([',', ' ', '\t'])
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_start_matches('+').to_ascii_uppercase())
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Err("empty output column list".into());
    }
    for column in &columns {
        if !lock_column_names().contains(&column.as_str()) {
            return Err(format!("unknown column '{column}'"));
        }
    }
    Ok(columns)
}

fn lock_column_names() -> [&'static str; 13] {
    [
        "COMMAND", "PID", "TYPE", "SIZE", "INODE", "MAJ:MIN", "MODE", "M", "START", "END", "PATH",
        "BLOCKER", "HOLDERS",
    ]
}

fn print_lock_columns() {
    for (name, kind, description) in [
        (
            "COMMAND",
            "<string>",
            "command of the process holding the lock",
        ),
        ("PID", "<integer>", "PID of the process holding the lock"),
        ("TYPE", "<string>", "kind of lock"),
        (
            "SIZE",
            "<string|number>",
            "size of the lock, use <number> if --bytes is given",
        ),
        ("INODE", "<integer>", "inode number"),
        ("MAJ:MIN", "<string>", "major:minor device number"),
        ("MODE", "<string>", "lock access mode"),
        (
            "M",
            "<boolean>",
            "mandatory state of the lock: 0 (none), 1 (set)",
        ),
        ("START", "<integer>", "relative byte offset of the lock"),
        ("END", "<integer>", "ending offset of the lock"),
        ("PATH", "<string>", "path of the locked file"),
        (
            "BLOCKER",
            "<integer>",
            "PID of the process blocking the lock",
        ),
        ("HOLDERS", "<string>", "holders of the lock"),
    ] {
        println!("{name:>7} {kind:<15} {description}");
    }
}

fn all_lock_columns() -> Vec<String> {
    lock_column_names().into_iter().map(str::to_owned).collect()
}

fn default_lock_columns() -> Vec<String> {
    [
        "COMMAND", "PID", "TYPE", "SIZE", "MODE", "M", "START", "END", "PATH",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn parse_lock_line(line: &str) -> Option<LockRecord> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 8 {
        return None;
    }
    let id = fields[0].strip_suffix(':')?.parse().ok()?;
    let pid = fields[4].parse().ok()?;
    let device = fields[5].split(':').collect::<Vec<_>>();
    let (major, minor, inode, start_field, end_field) = match device.as_slice() {
        [major, minor, inode] => (
            u64::from_str_radix(major, 16).ok()?,
            u64::from_str_radix(minor, 16).ok()?,
            parse_decimal_or_hex(inode)?,
            6,
            7,
        ),
        [major, minor] if fields.len() >= 9 => (
            u64::from_str_radix(major, 16).ok()?,
            u64::from_str_radix(minor, 16).ok()?,
            parse_decimal_or_hex(fields[6])?,
            7,
            8,
        ),
        _ => return None,
    };
    let start = parse_decimal_or_hex(fields[start_field])?;
    let end = (fields[end_field] != "EOF")
        .then(|| parse_decimal_or_hex(fields[end_field]))
        .flatten();
    Some(LockRecord {
        id,
        lock_type: fields[1].to_owned(),
        mandatory: fields[2] == "MANDATORY",
        mode: fields[3].to_owned(),
        pid,
        major,
        minor,
        inode,
        start,
        end,
        path: None,
        file_size: None,
        command: String::new(),
    })
}

fn parse_decimal_or_hex(value: &str) -> Option<u64> {
    value
        .strip_prefix("0x")
        .and_then(|value| u64::from_str_radix(value, 16).ok())
        .or_else(|| value.parse().ok())
}

fn resolve_lock_path(record: &LockRecord) -> Option<(String, Option<u64>)> {
    let pids = if record.pid >= 0 {
        vec![record.pid as u32]
    } else {
        list_processes()
            .ok()?
            .into_iter()
            .map(|process| process.pid)
            .collect()
    };
    for pid in pids {
        for fd in process_fds(pid) {
            let Some(metadata) = fd.metadata else {
                continue;
            };
            let device = metadata.dev();
            if libc::major(device) as u64 != record.major
                || libc::minor(device) as u64 != record.minor
                || metadata.ino() != record.inode
            {
                continue;
            }
            let target = text(fd.target.as_os_str());
            if !target.starts_with("/") || target.ends_with(" (deleted)") {
                continue;
            }
            let size = fs::metadata(Path::new(&target))
                .ok()
                .map(|metadata| metadata.len());
            return Some((target, size));
        }
    }
    fallback_lock_path(record).map(|path| (path, None))
}

fn fallback_lock_path(record: &LockRecord) -> Option<String> {
    let content = fs::read_to_string("/proc/self/mountinfo").ok()?;
    for line in content.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let Some(device) = fields.get(2) else {
            continue;
        };
        let Some((major, minor)) = device.split_once(':') else {
            continue;
        };
        if major.parse::<u64>().ok() != Some(record.major)
            || minor.parse::<u64>().ok() != Some(record.minor)
        {
            continue;
        }
        let Some(field) = fields.get(4) else {
            continue;
        };
        let mountpoint = unescape_mount_field(field);
        return Some(format!("{mountpoint}..."));
    }
    None
}

fn lock_size(record: &LockRecord, bytes: bool) -> String {
    let Some(size) = record.file_size else {
        return "-".into();
    };
    if bytes {
        size.to_string()
    } else {
        human_size(size)
    }
}

fn human_size(size: u64) -> String {
    const UNITS: [&str; 6] = ["", "K", "M", "G", "T", "P"];
    if size < 1024 {
        return size.to_string();
    }
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 10.0 {
        format!("{value:.0}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

fn lock_value(record: &LockRecord, column: &str, options: &LocksOptions) -> String {
    match column {
        "COMMAND" => record.command.clone(),
        "PID" => record.pid.to_string(),
        "TYPE" => record.lock_type.clone(),
        "SIZE" => lock_size(record, options.bytes),
        "INODE" => record.inode.to_string(),
        "MAJ:MIN" => format!("{}:{}", record.major, record.minor),
        "MODE" => record.mode.clone(),
        "M" => if record.mandatory { "1" } else { "0" }.into(),
        "START" => record.start.to_string(),
        "END" => record
            .end
            .map_or_else(|| "EOF".into(), |end| end.to_string()),
        "PATH" => record.path.clone().unwrap_or_else(|| "-".into()),
        "BLOCKER" | "HOLDERS" => "-".into(),
        _ => String::new(),
    }
}

fn print_locks_table(locks: &[LockRecord], columns: &[String], options: &LocksOptions) {
    let mut rows = Vec::new();
    for record in locks {
        let _ = record.id;
        rows.push(
            columns
                .iter()
                .map(|column| lock_value(record, column, options))
                .collect::<Vec<_>>(),
        );
    }
    let mut widths = columns.iter().map(String::len).collect::<Vec<_>>();
    for row in &rows {
        for (index, value) in row.iter().enumerate() {
            let value = if options.no_truncate {
                value.clone()
            } else {
                truncate(value, 64)
            };
            widths[index] = widths[index].max(value.len());
        }
    }
    if !options.no_headings {
        if options.raw {
            println!("{}", columns.join(" "));
        } else {
            println!(
                "{}",
                columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| format!("{column:<width$}", width = widths[index]))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim_end()
            );
        }
    }
    for row in rows {
        let row = row
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                let value = if options.no_truncate {
                    value
                } else {
                    truncate(&value, 64)
                };
                if options.raw {
                    value
                } else {
                    format!("{value:<width$}", width = widths[index])
                }
            })
            .collect::<Vec<_>>();
        println!("{}", row.join(" ").trim_end());
    }
}

fn print_locks_json(locks: &[LockRecord], columns: &[String], options: &LocksOptions) {
    let mut root = JsonMap::new();
    let mut values = Vec::new();
    for record in locks {
        let mut value = JsonMap::new();
        for column in columns {
            value.insert(
                column.to_ascii_lowercase(),
                lock_json_value(record, column, options),
            );
        }
        values.push(JsonValue::Object(value));
    }
    root.insert("locks".into(), JsonValue::Array(values));
    let json = JsonValue::Object(root);
    let result = if std::env::var_os("LIBSMARTCOLS_JSON").as_deref() == Some(OsStr::new("compact"))
    {
        serde_json::to_string(&json)
    } else {
        serde_json::to_string_pretty(&json)
    };
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => eprintln!("lslocks: cannot encode JSON: {error}"),
    }
}

fn lock_json_value(record: &LockRecord, column: &str, options: &LocksOptions) -> JsonValue {
    match column {
        "COMMAND" => record.command.clone().into(),
        "PID" => record.pid.into(),
        "TYPE" => record.lock_type.clone().into(),
        "SIZE" if options.bytes => record.file_size.map_or(JsonValue::Null, JsonValue::from),
        "SIZE" => record
            .file_size
            .map_or_else(|| JsonValue::from("-"), JsonValue::from),
        "INODE" => record.inode.into(),
        "MAJ:MIN" => format!("{}:{}", record.major, record.minor).into(),
        "MODE" => record.mode.clone().into(),
        "M" => record.mandatory.into(),
        "START" => record.start.into(),
        "END" => record
            .end
            .map_or_else(|| JsonValue::from("EOF"), JsonValue::from),
        "PATH" => record.path.clone().map_or(JsonValue::Null, JsonValue::from),
        "BLOCKER" => JsonValue::Null,
        "HOLDERS" => JsonValue::Array(Vec::new()),
        _ => JsonValue::Null,
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    if max <= 3 {
        return value.chars().take(max).collect();
    }
    let mut result = value.chars().take(max - 3).collect::<String>();
    result.push_str("...");
    result
}

// -----------------------------------------------------------------------------
// pstree

#[derive(Clone)]
struct TreeProcess {
    pid: u32,
    ppid: u32,
    pgrp: u32,
    uid: u32,
    name: String,
    arguments: String,
    threads: Vec<TreeThread>,
    namespaces: BTreeMap<String, u64>,
    security: Option<String>,
}

#[derive(Clone)]
struct TreeThread {
    tid: u32,
    name: String,
}

#[derive(Default)]
struct PstreeOptions {
    arguments: bool,
    ascii: bool,
    unicode: bool,
    vt100: bool,
    no_compact: bool,
    groups: bool,
    long: bool,
    numeric: bool,
    pids: bool,
    show_parents: bool,
    hide_threads: bool,
    thread_names: bool,
    highlight_pid: Option<u32>,
    color: Option<String>,
    ns_sort: Option<String>,
    ns_changes: bool,
    uid_changes: bool,
    security: bool,
}

pub fn pstree(args: Vec<OsString>) -> i32 {
    match run_pstree(args) {
        Ok(()) => 0,
        Err(error) => print_error("pstree", error, 1),
    }
}

fn current_pid() -> u32 {
    // SAFETY: getpid has no arguments or caller-side memory requirements.
    unsafe { libc::getpid() as u32 }
}

fn run_pstree(args: Vec<OsString>) -> Result<(), String> {
    let mut options = PstreeOptions {
        ascii: true,
        ..PstreeOptions::default()
    };
    let mut selector = None;
    let mut parse_options = true;
    let mut iter = utf8_args(args)?.into_iter().skip(1);
    while let Some(argument) = iter.next() {
        if parse_options && argument == "--" {
            parse_options = false;
            continue;
        }
        if !parse_options || !argument.starts_with('-') || argument == "-" {
            if selector.is_some() {
                return Err("only one PID or user selector is supported".into());
            }
            selector = Some(argument);
            continue;
        }
        match argument.as_str() {
            "-a" | "--arguments" => options.arguments = true,
            "-A" | "--ascii" => {
                options.ascii = true;
                options.unicode = false;
                options.vt100 = false;
            }
            "-c" | "--compact-not" => options.no_compact = true,
            "-g" | "--show-pgids" => {
                options.groups = true;
                options.no_compact = true;
            }
            "-l" | "--long" => options.long = true,
            "-n" | "--numeric-sort" => options.numeric = true,
            "-p" | "--show-pids" => {
                options.pids = true;
                options.no_compact = true;
            }
            "-s" | "--show-parents" => options.show_parents = true,
            "-T" | "--hide-threads" => options.hide_threads = true,
            "-t" | "--thread-names" => options.thread_names = true,
            "-U" | "--unicode" => {
                options.unicode = true;
                options.ascii = false;
                options.vt100 = false;
            }
            "-G" | "--vt100" => {
                options.vt100 = true;
                options.ascii = false;
                options.unicode = false;
            }
            "-S" | "--ns-changes" => options.ns_changes = true,
            "-u" | "--uid-changes" => options.uid_changes = true,
            "-Z" | "--security-context" => options.security = true,
            "-h" | "--highlight-all" => options.highlight_pid = Some(current_pid()),
            "-H" | "--highlight-pid" => {
                options.highlight_pid = Some(
                    iter.next()
                        .ok_or("-H/--highlight-pid requires a PID")?
                        .parse()
                        .map_err(|_| "invalid highlight PID")?,
                );
            }
            "-N" | "--ns-sort" => {
                options.ns_sort = Some(iter.next().ok_or("-N/--ns-sort requires a namespace")?);
            }
            "-C" | "--color" => {
                options.color = Some(iter.next().ok_or("-C/--color requires a type")?);
            }
            "--help" => {
                println!(
                    "Usage: pstree [-aAcCglGhnN:p sSTtUZ] [PID|USER]\n\
                     Read-only process hierarchy from /proc. -C color, -G VT100, -U Unicode,\n\
                     -h highlight ancestors, -H PID highlight PID, -N namespace sort,\n\
                     -S namespace changes, -u UID changes, -Z security context."
                );
                return Ok(());
            }
            "-V" | "--version" => {
                println!("pstree (AXE)");
                return Ok(());
            }
            value if value.starts_with("--color=") => {
                options.color = Some(value[8..].to_owned());
            }
            value if value.starts_with("--highlight-pid=") => {
                options.highlight_pid =
                    Some(value[16..].parse().map_err(|_| "invalid highlight PID")?);
            }
            value if value.starts_with("--ns-sort=") => {
                options.ns_sort = Some(value[10..].to_owned());
            }
            value if value.starts_with('-') => {
                let flags = value.strip_prefix('-').unwrap_or_default();
                if flags.is_empty() {
                    return Err("unknown option '-'".into());
                }
                for flag in flags.chars() {
                    match flag {
                        'a' => options.arguments = true,
                        'A' => options.ascii = true,
                        'c' => options.no_compact = true,
                        'g' => {
                            options.groups = true;
                            options.no_compact = true;
                        }
                        'l' => options.long = true,
                        'n' => options.numeric = true,
                        'p' => {
                            options.pids = true;
                            options.no_compact = true;
                        }
                        's' => options.show_parents = true,
                        'T' => options.hide_threads = true,
                        't' => options.thread_names = true,
                        'U' => options.unicode = true,
                        'G' => options.vt100 = true,
                        'S' => options.ns_changes = true,
                        'u' => options.uid_changes = true,
                        'Z' => options.security = true,
                        'h' => options.highlight_pid = Some(current_pid()),
                        _ => return Err(format!("unsupported option '-{flag}'")),
                    }
                }
            }
            _ => unreachable!(),
        }
    }
    if let Some(color) = options.color.as_deref()
        && !matches!(color, "auto" | "always" | "never" | "none")
    {
        return Err(format!("unsupported color mode '{color}'"));
    }
    if let Some(namespace) = options.ns_sort.as_deref()
        && !matches!(
            namespace,
            "cgroup" | "ipc" | "mnt" | "net" | "pid" | "time" | "user" | "uts"
        )
    {
        return Err(format!("unsupported namespace '{namespace}'"));
    }
    if options.show_parents && selector.is_none() {
        return Err("-s requires a PID or user selector".into());
    }

    let processes = read_tree_processes();
    if processes.is_empty() {
        return Err("cannot read any processes from /proc".into());
    }
    let selector = selector.unwrap_or_else(|| "1".into());
    let selected = select_tree_roots(&processes, &selector)?;
    let mut output = String::new();
    if options.show_parents {
        let pid = selected[0];
        let mut chain = Vec::new();
        let mut current = pid;
        let mut seen = HashSet::new();
        while let Some(process) = processes.get(&current) {
            if !seen.insert(current) {
                break;
            }
            chain.push(tree_display(process, &options));
            if process.ppid == 0 || process.ppid == current {
                break;
            }
            current = process.ppid;
        }
        chain.reverse();
        output.push_str(&chain.join("---"));
        output.push('\n');
    } else {
        let mut children = HashMap::<u32, Vec<u32>>::new();
        for process in processes.values() {
            if processes.contains_key(&process.ppid) && process.ppid != process.pid {
                children.entry(process.ppid).or_default().push(process.pid);
            }
        }
        for ids in children.values_mut() {
            if options.numeric {
                ids.sort_unstable();
            } else if let Some(namespace) = options.ns_sort.as_deref() {
                ids.sort_unstable_by_key(|pid| {
                    processes
                        .get(pid)
                        .and_then(|process| process.namespaces.get(namespace))
                        .copied()
                        .unwrap_or_default()
                });
            } else {
                ids.sort_unstable_by_key(|pid| {
                    processes
                        .get(pid)
                        .map(|process| process.name.to_ascii_lowercase())
                        .unwrap_or_default()
                });
            }
        }
        for (index, pid) in selected.iter().enumerate() {
            if index != 0 {
                output.push('\n');
            }
            let Some(root) = processes.get(pid) else {
                continue;
            };
            output.push_str(&tree_display(root, &options));
            render_tree_children(*pid, "", &processes, &children, &options, &mut output);
        }
        if !output.is_empty() {
            output.push('\n');
        }
    }
    print!("{output}");
    Ok(())
}

fn read_tree_processes() -> BTreeMap<u32, TreeProcess> {
    let mut processes = BTreeMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return processes;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let path = entry.path();
        let Some((comm, ppid, pgrp)) = read_stat(pid) else {
            continue;
        };
        let uid = read_proc_uid(&path).unwrap_or_else(|| {
            fs::metadata(&path)
                .map(|metadata| metadata.uid())
                .unwrap_or(0)
        });
        let arguments = fs::read(path.join("cmdline"))
            .ok()
            .map(|bytes| {
                bytes
                    .split(|byte| *byte == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let namespaces = read_namespace_ids(&path);
        let security = read_trim(&path.join("attr/current"));
        let mut threads = Vec::new();
        if let Ok(tasks) = fs::read_dir(path.join("task")) {
            for task in tasks.flatten() {
                let task_name = task.file_name();
                let Ok(tid) = task_name.to_string_lossy().parse::<u32>() else {
                    continue;
                };
                if tid == pid {
                    continue;
                }
                if let Some(name) = read_trim(&task.path().join("comm")) {
                    threads.push(TreeThread { tid, name });
                }
            }
        }
        threads.sort_unstable_by_key(|thread| thread.tid);
        processes.insert(
            pid,
            TreeProcess {
                pid,
                ppid,
                pgrp,
                uid,
                name: comm,
                arguments,
                threads,
                namespaces,
                security,
            },
        );
    }
    processes
}

fn read_stat(pid: u32) -> Option<(String, u32, u32)> {
    let value = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_process_stat(&value)
}

fn parse_process_stat(value: &str) -> Option<(String, u32, u32)> {
    let open = value.find('(')?;
    let close = value.rfind(") ")?;
    let comm = value[open + 1..close].to_owned();
    let fields = value[close + 2..].split_whitespace().collect::<Vec<_>>();
    let ppid = fields.get(1)?.parse().ok()?;
    let pgrp = fields.get(2)?.parse().ok()?;
    Some((comm, ppid, pgrp))
}

fn read_proc_uid(path: &Path) -> Option<u32> {
    for line in fs::read_to_string(path.join("status")).ok()?.lines() {
        if let Some(value) = line.strip_prefix("Uid:") {
            return value.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn read_namespace_ids(path: &Path) -> BTreeMap<String, u64> {
    let mut result = BTreeMap::new();
    for name in ["cgroup", "ipc", "mnt", "net", "pid", "time", "user", "uts"] {
        let Ok(target) = fs::read_link(path.join("ns").join(name)) else {
            continue;
        };
        let target = target.to_string_lossy();
        let Some(id) = target
            .strip_prefix(&format!("{name}:["))
            .and_then(|value| value.strip_suffix(']'))
        else {
            continue;
        };
        if let Ok(id) = id.parse() {
            result.insert(name.into(), id);
        }
    }
    result
}

fn select_tree_roots(
    processes: &BTreeMap<u32, TreeProcess>,
    selector: &str,
) -> Result<Vec<u32>, String> {
    if let Ok(pid) = selector.parse::<u32>() {
        if processes.contains_key(&pid) {
            return Ok(vec![pid]);
        }
        return Err(format!("PID {pid} does not exist"));
    }
    let selected = processes
        .values()
        .filter(|process| user_name(process.uid) == selector || process.uid.to_string() == selector)
        .map(|process| process.pid)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(format!("user '{selector}' has no visible processes"));
    }
    Ok(selected)
}

fn tree_display(process: &TreeProcess, options: &PstreeOptions) -> String {
    let mut value = process.name.clone();
    if options.arguments && !process.arguments.is_empty() {
        let arguments = if options.long {
            process.arguments.clone()
        } else {
            truncate(&process.arguments, 80)
        };
        value.push(' ');
        value.push_str(&arguments);
    }
    if options.pids || options.groups {
        value.push('(');
        if options.pids {
            value.push_str(&process.pid.to_string());
        }
        if options.groups {
            if options.pids {
                value.push(',');
            }
            value.push_str(&process.pgrp.to_string());
        }
        value.push(')');
    }
    if options.uid_changes {
        value.push_str(&format!("{{uid:{}}}", process.uid));
    }
    if options.ns_changes {
        value.push_str("[ns]");
    }
    if options.security {
        value.push(' ');
        value.push_str(process.security.as_deref().unwrap_or("?"));
    }
    if options.ns_sort.is_some() && !process.namespaces.is_empty() {
        let namespace = options.ns_sort.as_deref().unwrap_or_default();
        if let Some(id) = process.namespaces.get(namespace) {
            value.push_str(&format!(" [{id}]"));
        }
    }
    if options.highlight_pid == Some(process.pid) {
        value = format!("\x1b[1m{value}\x1b[0m");
    } else if options
        .color
        .as_deref()
        .is_some_and(|color| color != "never" && color != "none")
    {
        value = format!("\x1b[36m{value}\x1b[0m");
    }
    value
}

fn subtree_signature(
    pid: u32,
    processes: &BTreeMap<u32, TreeProcess>,
    children: &HashMap<u32, Vec<u32>>,
) -> String {
    fn visit(
        pid: u32,
        processes: &BTreeMap<u32, TreeProcess>,
        children: &HashMap<u32, Vec<u32>>,
        active: &mut HashSet<u32>,
        depth: usize,
    ) -> String {
        let Some(process) = processes.get(&pid) else {
            return String::new();
        };
        if depth > 128 || !active.insert(pid) {
            return format!("{}[]", process.name);
        }
        let mut child_signatures = children
            .get(&pid)
            .into_iter()
            .flatten()
            .map(|child| visit(*child, processes, children, active, depth + 1))
            .collect::<Vec<_>>();
        child_signatures.sort_unstable();
        active.remove(&pid);
        format!("{}[{}]", process.name, child_signatures.join("|"))
    }

    visit(pid, processes, children, &mut HashSet::new(), 0)
}

fn render_tree_children(
    pid: u32,
    prefix: &str,
    processes: &BTreeMap<u32, TreeProcess>,
    children: &HashMap<u32, Vec<u32>>,
    options: &PstreeOptions,
    output: &mut String,
) {
    let mut ids = children.get(&pid).cloned().unwrap_or_default();
    if options.numeric {
        ids.sort_unstable();
    } else if let Some(namespace) = options.ns_sort.as_deref() {
        ids.sort_unstable_by_key(|child| {
            processes
                .get(child)
                .and_then(|process| process.namespaces.get(namespace))
                .copied()
                .unwrap_or_default()
        });
    } else if !options.no_compact {
        ids.sort_unstable_by_key(|child| processes.get(child).map(|process| process.name.clone()));
    }
    let compact_ids = ids.clone();
    let thread_count = processes
        .get(&pid)
        .filter(|_| !options.hide_threads)
        .map_or(0, |process| process.threads.len());
    let child_count = ids.len();
    let total = child_count + thread_count;
    for (index, child) in ids.into_iter().enumerate() {
        let last = index + 1 == total;
        let (branch, next) = if options.unicode {
            (
                if last { "└─ " } else { "├─ " },
                if last { "   " } else { "│  " },
            )
        } else if options.vt100 {
            (
                if last { "`- " } else { "q- " },
                if last { "   " } else { "x  " },
            )
        } else {
            (
                if last { "`- " } else { "|- " },
                if last { "   " } else { "|  " },
            )
        };
        let Some(process) = processes.get(&child) else {
            continue;
        };
        let compact = !options.no_compact
            && !options.pids
            && !options.groups
            && !options.arguments
            && !options.ns_changes
            && !options.uid_changes
            && !options.security
            && options.ns_sort.is_none()
            && options.highlight_pid.is_none();
        let signature = compact.then(|| subtree_signature(child, processes, children));
        let repeat_count = signature.as_ref().map_or(1, |signature| {
            compact_ids
                .iter()
                .filter(|other| {
                    subtree_signature(**other, processes, children).as_str() == signature.as_str()
                })
                .count()
        });
        if repeat_count > 1
            && compact_ids[..index].iter().any(|other| {
                subtree_signature(*other, processes, children).as_str()
                    == signature.as_deref().unwrap_or_default()
            })
        {
            continue;
        }
        let mut display = tree_display(process, options);
        if repeat_count > 1 {
            display.push_str(&format!("---{repeat_count}*"));
        }
        output.push('\n');
        output.push_str(prefix);
        output.push_str(branch);
        output.push_str(&display);
        render_tree_children(
            child,
            &format!("{prefix}{next}"),
            processes,
            children,
            options,
            output,
        );
    }
    if let Some(process) = processes.get(&pid)
        && !options.hide_threads
    {
        for (offset, thread) in process.threads.iter().enumerate() {
            let last = child_count + offset + 1 == total;
            let (branch, _) = if options.unicode {
                (if last { "└─ " } else { "├─ " }, "")
            } else if options.vt100 {
                (if last { "`- " } else { "q- " }, "")
            } else {
                (if last { "`- " } else { "|- " }, "")
            };
            let mut value = if options.thread_names {
                thread.name.clone()
            } else {
                format!("{{{}}}", thread.name)
            };
            if options.pids {
                value.push_str(&format!("({})", thread.tid));
            }
            output.push('\n');
            output.push_str(prefix);
            output.push_str(branch);
            output.push_str(&value);
        }
    }
}

// -----------------------------------------------------------------------------
// lsblk

#[derive(Clone, Default)]
struct BlockDevice {
    name: String,
    sys_path: PathBuf,
    parent: Option<String>,
    children: Vec<String>,
    devno: String,
    size_sectors: u64,
    kind: String,
    attrs: BTreeMap<String, String>,
    mountpoints: Vec<String>,
}

#[derive(Default)]
struct LsblkOptions {
    all: bool,
    noempty: bool,
    bytes: bool,
    nodeps: bool,
    filesystem: bool,
    perms: bool,
    discard: bool,
    ascii: bool,
    json: bool,
    pairs: bool,
    list: bool,
    no_headings: bool,
    columns: Option<Vec<String>>,
    output_all: bool,
    paths: bool,
    raw: bool,
    inverse: bool,
    topology: bool,
    zoned: bool,
    scsi: bool,
    nvme: bool,
    virtio: bool,
    merge: bool,
    tree: bool,
    shell: bool,
    width: Option<usize>,
    sort: Option<String>,
    dedup: Option<String>,
    include_majors: HashSet<u64>,
    exclude_majors: HashSet<u64>,
    list_columns: bool,
}

pub fn lsblk(args: Vec<OsString>) -> i32 {
    match run_lsblk(args) {
        Ok(status) => status,
        Err(error) => print_error("lsblk", error, 1),
    }
}

fn run_lsblk(args: Vec<OsString>) -> Result<i32, String> {
    let mut options = LsblkOptions::default();
    let mut operands = Vec::new();
    let mut parse_options = true;
    let mut iter = utf8_args(args)?.into_iter().skip(1);
    while let Some(value) = iter.next() {
        if parse_options && value == "--" {
            parse_options = false;
            continue;
        }
        if !parse_options || !value.starts_with('-') || value == "-" {
            operands.push(value);
            continue;
        }
        match value.as_str() {
            "-a" | "--all" => options.all = true,
            "-A" | "--noempty" => options.noempty = true,
            "-b" | "--bytes" => options.bytes = true,
            "-d" | "--nodeps" => options.nodeps = true,
            "-D" | "--discard" => options.discard = true,
            "-f" | "--fs" => options.filesystem = true,
            "-i" | "--ascii" => options.ascii = true,
            "-J" | "--json" => options.json = true,
            "-l" | "--list" => options.list = true,
            "-m" | "--perms" => options.perms = true,
            "-M" | "--merge" => options.merge = true,
            "-n" | "--noheadings" => options.no_headings = true,
            "-N" | "--nvme" => options.nvme = true,
            "-O" | "--output-all" => options.output_all = true,
            "-p" | "--paths" => options.paths = true,
            "-P" | "--pairs" => options.pairs = true,
            "-r" | "--raw" => options.raw = true,
            "-s" | "--inverse" => options.inverse = true,
            "-S" | "--scsi" => options.scsi = true,
            "-t" | "--topology" => options.topology = true,
            "-T" | "--tree" => options.tree = true,
            "-v" | "--virtio" => options.virtio = true,
            "-y" | "--shell" => options.shell = true,
            "-z" | "--zoned" => options.zoned = true,
            "-H" | "--list-columns" => options.list_columns = true,
            "-e" | "--exclude" => {
                options.exclude_majors =
                    parse_major_list(&iter.next().ok_or("-e/--exclude requires a list")?)?;
            }
            "-I" | "--include" => {
                options.include_majors =
                    parse_major_list(&iter.next().ok_or("-I/--include requires a list")?)?;
            }
            "-E" | "--dedup" => {
                options.dedup = Some(
                    iter.next()
                        .ok_or("-E/--dedup requires a column")?
                        .to_ascii_uppercase(),
                );
            }
            "-w" | "--width" => {
                options.width = Some(
                    iter.next()
                        .ok_or("-w/--width requires a number")?
                        .parse()
                        .map_err(|_| "invalid output width")?,
                );
            }
            "-x" | "--sort" => {
                options.sort = Some(
                    iter.next()
                        .ok_or("-x/--sort requires a column")?
                        .to_ascii_uppercase(),
                );
            }
            "-h" | "--help" => {
                println!(
                    "Usage: lsblk [options] [DEVICE...]\n\
                     Read-only sysfs block-device discovery. Supported presets include\n\
                     -A -D -e -E -I -M -m -N -P -T -v -w -x -y -z and -H.\n\
                     Experimental filters, counters, alternate sysroots, and hyperlinks are unsupported."
                );
                return Ok(0);
            }
            "-V" | "--version" => {
                println!("lsblk (AXE)");
                return Ok(0);
            }
            "-Q" | "--filter" | "--highlight" | "--ct-filter" | "--ct" | "--sysroot"
            | "--properties-by" | "--hyperlink" => {
                return Err(format!(
                    "option '{value}' is unsupported by this read-only backend"
                ));
            }
            "-o" | "--output" => {
                options.columns = Some(parse_block_columns(
                    &iter.next().ok_or("-o/--output requires a list")?,
                )?);
            }
            value if value.starts_with("--output=") => {
                options.columns = Some(parse_block_columns(&value[9..])?);
            }
            value if value.starts_with("-o") && value.len() > 2 => {
                options.columns = Some(parse_block_columns(&value[2..])?);
            }
            value if value.starts_with("--tree=") => options.tree = true,
            value if value.starts_with("--width=") => {
                options.width = Some(value[8..].parse().map_err(|_| "invalid output width")?);
            }
            value if value.starts_with("--exclude=") => {
                options.exclude_majors = parse_major_list(&value[10..])?;
            }
            value if value.starts_with("--include=") => {
                options.include_majors = parse_major_list(&value[10..])?;
            }
            value if value.starts_with("--dedup=") => {
                options.dedup = Some(value[8..].to_ascii_uppercase());
            }
            value if value.starts_with("--sort=") => {
                options.sort = Some(value[7..].to_ascii_uppercase())
            }
            value if value.starts_with('-') => return Err(format!("unsupported option '{value}'")),
            _ => unreachable!(),
        }
    }
    if options.json && (options.raw || options.pairs) {
        return Err("options --json, --raw, and --pairs cannot be combined".into());
    }
    if options.raw && options.pairs {
        return Err("options --raw and --pairs cannot be combined".into());
    }
    for column in options.sort.iter().chain(options.dedup.iter()) {
        if !block_column_names().contains(&column.as_str()) {
            return Err(format!("unknown column '{column}'"));
        }
    }
    if options.list_columns {
        print_block_columns();
        return Ok(0);
    }
    let requested_names = operands
        .iter()
        .map(|operand| {
            Path::new(operand).file_name().map_or_else(
                || operand.clone(),
                |name| name.to_string_lossy().into_owned(),
            )
        })
        .collect::<HashSet<_>>();
    let mut devices = discover_block_devices()?;
    let found_operands = if requested_names.is_empty() {
        0
    } else {
        devices
            .iter()
            .filter(|device| requested_names.contains(&device.name))
            .count()
    };
    if !requested_names.is_empty() {
        devices.retain(|device| requested_names.contains(&device.name));
    }
    if options.scsi {
        devices.retain(is_scsi);
    }
    if options.nvme {
        devices.retain(|device| device.name.starts_with("nvme"));
    }
    if options.virtio {
        devices.retain(|device| {
            device
                .sys_path
                .components()
                .any(|component| component.as_os_str().to_string_lossy().contains("virtio"))
        });
    }
    if !options.include_majors.is_empty() {
        devices.retain(|device| {
            major_of(device).is_some_and(|major| options.include_majors.contains(&major))
        });
    }
    if !options.exclude_majors.is_empty() {
        devices.retain(|device| {
            major_of(device).is_none_or(|major| !options.exclude_majors.contains(&major))
        });
    }
    if !options.all || options.noempty {
        devices.retain(|device| device.size_sectors != 0 && !device.name.starts_with("ram"));
    }
    if let Some(column) = options.sort.as_deref() {
        devices.sort_unstable_by_key(|device| block_value(device, column, &options));
    }
    if let Some(column) = options.dedup.as_deref() {
        let mut seen = HashSet::new();
        devices.retain(|device| seen.insert(block_value(device, column, &options)));
    }
    let columns = if options.output_all {
        all_block_columns()
    } else if let Some(columns) = options.columns.as_ref() {
        columns.clone()
    } else if options.perms {
        ["NAME", "OWNER", "GROUP", "MODE"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else if options.discard {
        ["NAME", "DISC-ALN", "DISC-GRAN", "DISC-MAX", "DISC-ZERO"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else if options.zoned {
        [
            "NAME",
            "ZONED",
            "ZONE-SZ",
            "ZONE-WGRAN",
            "ZONE-APP",
            "ZONE-NR",
            "ZONE-OMAX",
            "ZONE-AMAX",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    } else if options.filesystem {
        [
            "NAME",
            "FSTYPE",
            "FSVER",
            "LABEL",
            "UUID",
            "FSAVAIL",
            "FSUSE%",
            "MOUNTPOINTS",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    } else if options.topology {
        [
            "NAME",
            "ALIGNMENT",
            "MIN-IO",
            "OPT-IO",
            "PHY-SEC",
            "LOG-SEC",
            "ROTA",
            "SCHED",
            "RQ-SIZE",
            "RA",
            "WSAME",
            "FSTYPE",
            "MOUNTPOINTS",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    } else {
        ["NAME", "MAJ:MIN", "RM", "SIZE", "RO", "TYPE", "MOUNTPOINTS"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    };
    if options.json {
        print_block_json(&devices, &columns, &options);
    } else {
        print_block_table(&devices, &columns, &options);
    }
    if !requested_names.is_empty() {
        Ok(if found_operands == 0 {
            32
        } else if found_operands < requested_names.len() {
            64
        } else {
            0
        })
    } else {
        Ok(0)
    }
}

fn major_of(device: &BlockDevice) -> Option<u64> {
    device.devno.split_once(':')?.0.parse().ok()
}

fn parse_major_list(value: &str) -> Result<HashSet<u64>, String> {
    let mut result = HashSet::new();
    for item in value.split(',') {
        if item.is_empty() {
            continue;
        }
        result.insert(
            item.parse()
                .map_err(|_| format!("invalid major number '{item}'"))?,
        );
    }
    if result.is_empty() {
        return Err("empty major-number list".into());
    }
    Ok(result)
}

fn parse_block_columns(value: &str) -> Result<Vec<String>, String> {
    let mut columns = Vec::new();
    for raw in value.split([',', ' ', '\t']) {
        if raw.is_empty() {
            continue;
        }
        let column = raw.trim_start_matches('+').to_ascii_uppercase();
        if !block_column_names().contains(&column.as_str()) {
            return Err(format!("unknown column '{column}'"));
        }
        columns.push(column);
    }
    if columns.is_empty() {
        return Err("empty output column list".into());
    }
    Ok(columns)
}

fn block_column_names() -> [&'static str; 50] {
    [
        "NAME",
        "KNAME",
        "PATH",
        "MAJ:MIN",
        "SIZE",
        "TYPE",
        "FSTYPE",
        "FSVER",
        "LABEL",
        "UUID",
        "FSAVAIL",
        "FSUSE%",
        "MOUNTPOINT",
        "MOUNTPOINTS",
        "PKNAME",
        "HCTL",
        "VENDOR",
        "MODEL",
        "REV",
        "SERIAL",
        "RO",
        "RM",
        "HOTPLUG",
        "STATE",
        "OWNER",
        "GROUP",
        "MODE",
        "ALIGNMENT",
        "MIN-IO",
        "OPT-IO",
        "PHY-SEC",
        "LOG-SEC",
        "ROTA",
        "SCHED",
        "RQ-SIZE",
        "RA",
        "WSAME",
        "DISC-ALN",
        "DISC-GRAN",
        "DISC-MAX",
        "DISC-ZERO",
        "TRAN",
        "SUBSYSTEMS",
        "ZONED",
        "ZONE-SZ",
        "ZONE-WGRAN",
        "ZONE-APP",
        "ZONE-NR",
        "ZONE-OMAX",
        "ZONE-AMAX",
    ]
}

fn print_block_columns() {
    for (name, kind, description) in [
        ("NAME", "<string>", "device name"),
        ("ALIGNMENT", "<integer>", "alignment offset"),
        (
            "ID-LINK",
            "<string>",
            "the shortest udev /dev/disk/by-id link name",
        ),
        ("ID", "<string>", "udev ID (based on ID-LINK)"),
        ("DISC-ALN", "<integer>", "discard alignment offset"),
        ("DAX", "<boolean>", "dax-capable device"),
        (
            "DISC-GRAN",
            "<string|number>",
            "discard granularity, use <number> if --bytes is given",
        ),
        ("DISK-SEQ", "<integer>", "disk sequence number"),
        (
            "DISC-MAX",
            "<string|number>",
            "discard max bytes, use <number> if --bytes is given",
        ),
        ("DISC-ZERO", "<boolean>", "discard zeroes data"),
        (
            "FSAVAIL",
            "<string|number>",
            "filesystem size available for unprivileged users, use <number> if --bytes is given",
        ),
        ("FSROOTS", "<string>", "mounted filesystem roots"),
        (
            "FSSIZE",
            "<string|number>",
            "filesystem size, use <number> if --bytes is given",
        ),
        ("FSTYPE", "<string>", "filesystem type"),
        (
            "FSUSED",
            "<string|number>",
            "filesystem size used, use <number> if --bytes is given",
        ),
        ("FSUSE%", "<string>", "filesystem use percentage"),
        ("FSVER", "<string>", "filesystem version"),
        ("GROUP", "<string>", "group name"),
        ("HCTL", "<string>", "Host:Channel:Target:Lun for SCSI"),
        (
            "HOTPLUG",
            "<boolean>",
            "removable or hotplug device (usb, pcmcia, ...)",
        ),
        ("KNAME", "<string>", "internal kernel device name"),
        ("LABEL", "<string>", "filesystem LABEL"),
        ("LOG-SEC", "<integer>", "logical sector size"),
        ("MAJ:MIN", "<string>", "major:minor device number"),
        ("MAJ", "<string>", "major device number"),
        ("MIN", "<string>", "minor device number"),
        ("MIN-IO", "<integer>", "minimum I/O size"),
        ("MODE", "<string>", "device node permissions"),
        ("MODEL", "<string>", "device identifier"),
        ("MQ", "<string>", "device queues"),
        ("OPT-IO", "<integer>", "optimal I/O size"),
        ("OWNER", "<string>", "user name"),
        ("PARTFLAGS", "<string>", "partition flags"),
        ("PARTLABEL", "<string>", "partition LABEL"),
        (
            "PARTN",
            "<integer>",
            "partition number as read from the partition table",
        ),
        ("PARTTYPE", "<string>", "partition type code or UUID"),
        ("PARTTYPENAME", "<string>", "partition type name"),
        ("PARTUUID", "<string>", "partition UUID"),
        ("PATH", "<string>", "path to the device node"),
        ("PHY-SEC", "<integer>", "physical sector size"),
        ("PKNAME", "<string>", "internal parent kernel device name"),
        ("PTTYPE", "<string>", "partition table type"),
        (
            "PTUUID",
            "<string>",
            "partition table identifier (usually UUID)",
        ),
        ("RA", "<integer>", "read-ahead of the device"),
        ("RAND", "<boolean>", "adds randomness"),
        ("REV", "<string>", "device revision"),
        ("RM", "<boolean>", "removable device"),
        ("RO", "<boolean>", "read-only device"),
        ("ROTA", "<boolean>", "rotational device"),
        ("RQ-SIZE", "<integer>", "request queue size"),
        ("SCHED", "<string>", "I/O scheduler name"),
        ("SERIAL", "<string>", "disk serial number"),
        (
            "SIZE",
            "<string|number>",
            "size of the device, use <number> if --bytes is given",
        ),
        (
            "START",
            "<integer>",
            "partition start offset (in 512-byte sectors)",
        ),
        ("STATE", "<string>", "state of the device"),
        (
            "SUBSYSTEMS",
            "<string>",
            "de-duplicated chain of subsystems",
        ),
        ("MOUNTPOINT", "<string>", "where the device is mounted"),
        (
            "MOUNTPOINTS",
            "<string>",
            "all locations where device is mounted",
        ),
        ("TRAN", "<string>", "device transport type"),
        ("TYPE", "<string>", "device type"),
        ("UUID", "<string>", "filesystem UUID"),
        ("VENDOR", "<string>", "device vendor"),
        (
            "WSAME",
            "<string|number>",
            "write same max bytes, use <number> if --bytes is given",
        ),
        ("WWN", "<string>", "unique storage identifier"),
        ("ZONED", "<string>", "zone model"),
        (
            "ZONE-SZ",
            "<string|number>",
            "zone size, use <number> if --bytes is given",
        ),
        (
            "ZONE-WGRAN",
            "<string|number>",
            "zone write granularity, use <number> if --bytes is given",
        ),
        (
            "ZONE-APP",
            "<string|number>",
            "zone append max bytes, use <number> if --bytes is given",
        ),
        ("ZONE-NR", "<integer>", "number of zones"),
        ("ZONE-OMAX", "<integer>", "maximum number of open zones"),
        ("ZONE-AMAX", "<integer>", "maximum number of active zones"),
    ] {
        println!("{name:>12} {kind:<15} {description}");
    }
}

fn all_block_columns() -> Vec<String> {
    block_column_names()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn discover_block_devices() -> Result<Vec<BlockDevice>, String> {
    let entries = fs::read_dir("/sys/class/block")
        .map_err(|error| format!("failed to access sysfs directory: {error}"))?;
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let sys_path = fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path());
        let devno = read_trim(&entry.path().join("dev")).unwrap_or_default();
        let size_sectors = read_trim(&entry.path().join("size"))
            .and_then(|value| value.parse().ok())
            .unwrap_or_default();
        let uevent_path = entry.path().join("uevent");
        let kind = if entry.path().join("partition").exists() {
            "part".to_owned()
        } else {
            read_uevent_value(&uevent_path, "DEVTYPE").unwrap_or_else(|| "disk".into())
        };
        let parent = partition_parent(&sys_path, &name);
        let mut attrs = BTreeMap::new();
        attrs.insert("KNAME".into(), name.clone());
        attrs.insert("PATH".into(), format!("/dev/{name}"));
        attrs.insert("MAJ:MIN".into(), devno.clone());
        attrs.insert("TYPE".into(), kind.clone());
        attrs.insert(
            "RO".into(),
            read_trim(&entry.path().join("ro")).unwrap_or_default(),
        );
        attrs.insert(
            "RM".into(),
            read_trim(&entry.path().join("removable")).unwrap_or_default(),
        );
        attrs.insert(
            "HOTPLUG".into(),
            read_trim(&entry.path().join("device/hotplug")).unwrap_or_default(),
        );
        attrs.insert(
            "STATE".into(),
            read_trim(&entry.path().join("device/state")).unwrap_or_default(),
        );
        attrs.insert(
            "VENDOR".into(),
            read_trim(&entry.path().join("device/vendor")).unwrap_or_default(),
        );
        attrs.insert(
            "MODEL".into(),
            read_trim(&entry.path().join("device/model")).unwrap_or_default(),
        );
        attrs.insert(
            "REV".into(),
            read_trim(&entry.path().join("device/rev")).unwrap_or_default(),
        );
        attrs.insert(
            "SERIAL".into(),
            read_trim(&entry.path().join("device/serial")).unwrap_or_default(),
        );
        if let Ok(metadata) = fs::metadata(format!("/dev/{name}")) {
            attrs.insert("OWNER".into(), user_name(metadata.uid()));
            attrs.insert("GROUP".into(), group_name(metadata.gid()));
            attrs.insert("MODE".into(), mode_string(&metadata));
        }
        for name in [
            "alignment_offset",
            "minimum_io_size",
            "optimal_io_size",
            "physical_block_size",
            "logical_block_size",
            "rotational",
            "scheduler",
            "nr_requests",
            "read_ahead_kb",
            "discard_alignment",
            "discard_granularity",
            "discard_max_bytes",
            "discard_zeroes_data",
            "write_same_max_bytes",
            "queue_type",
            "zoned",
            "chunk_sectors",
            "zone_write_granularity",
            "zone_append_max_bytes",
            "nr_zones",
            "max_open_zones",
            "max_active_zones",
        ] {
            let key = match name {
                "alignment_offset" => "ALIGNMENT",
                "minimum_io_size" => "MIN-IO",
                "optimal_io_size" => "OPT-IO",
                "physical_block_size" => "PHY-SEC",
                "logical_block_size" => "LOG-SEC",
                "rotational" => "ROTA",
                "scheduler" => "SCHED",
                "nr_requests" => "RQ-SIZE",
                "read_ahead_kb" => "RA",
                "discard_alignment" => "DISC-ALN",
                "discard_granularity" => "DISC-GRAN",
                "discard_max_bytes" => "DISC-MAX",
                "discard_zeroes_data" => "DISC-ZERO",
                "write_same_max_bytes" => "WSAME",
                "queue_type" => "QUEUE",
                "zoned" => "ZONED",
                "chunk_sectors" => "ZONE-SZ",
                "zone_write_granularity" => "ZONE-WGRAN",
                "zone_append_max_bytes" => "ZONE-APP",
                "nr_zones" => "ZONE-NR",
                "max_open_zones" => "ZONE-OMAX",
                "max_active_zones" => "ZONE-AMAX",
                _ => unreachable!(),
            };
            if let Some(value) = read_trim(&entry.path().join(format!("queue/{name}"))) {
                attrs.insert(key.into(), value);
            }
        }
        if !devno.is_empty() {
            for (key, value) in read_udev_properties(&devno) {
                attrs.entry(key).or_insert(value);
            }
        }
        devices.push(BlockDevice {
            name,
            sys_path,
            parent,
            children: Vec::new(),
            devno,
            size_sectors,
            kind,
            attrs,
            mountpoints: Vec::new(),
        });
    }
    let mounts = block_mountpoints();
    for device in &mut devices {
        if let Some(mountpoints) = mounts.get(&device.name) {
            device.mountpoints = mountpoints.clone();
        }
        if device.mountpoints.is_empty()
            && let Some(mountpoints) = mounts.get(&device.devno)
        {
            device.mountpoints = mountpoints.clone();
        }
    }
    let names = devices
        .iter()
        .map(|device| device.name.clone())
        .collect::<HashSet<_>>();
    for device in &mut devices {
        let slave_path = device.sys_path.join("slaves");
        if let Ok(entries) = fs::read_dir(slave_path) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if names.contains(&name) {
                    device.children.push(name);
                }
            }
        }
    }
    for index in 0..devices.len() {
        if let Some(parent) = devices[index].parent.clone() {
            if names.contains(&parent) && !devices[index].children.is_empty() {
                continue;
            }
            let child_name = devices[index].name.clone();
            if let Some(parent_device) = devices.iter_mut().find(|device| device.name == parent) {
                parent_device.children.push(child_name);
            }
        }
    }
    for device in &mut devices {
        device.children.sort_unstable();
        device.children.dedup();
    }
    devices.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(devices)
}

fn partition_parent(path: &Path, name: &str) -> Option<String> {
    if !path
        .components()
        .any(|component| component.as_os_str() == OsStr::new("block"))
    {
        return None;
    }
    let parent = path.parent()?.file_name()?.to_string_lossy().into_owned();
    (parent != name && parent != "block" && !parent.is_empty()).then_some(parent)
}

fn read_uevent_value(path: &Path, key: &str) -> Option<String> {
    fs::read_to_string(path).ok()?.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then(|| value.to_owned())
    })
}

fn read_udev_properties(devno: &str) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    let path = PathBuf::from(format!("/run/udev/data/b{devno}"));
    let Ok(content) = fs::read_to_string(path) else {
        return result;
    };
    for line in content.lines() {
        let Some(value) = line.strip_prefix("E:") else {
            continue;
        };
        let Some((key, value)) = value.split_once('=') else {
            continue;
        };
        let key = match key {
            "ID_FS_TYPE" => "FSTYPE",
            "ID_FS_VERSION" => "FSVER",
            "ID_FS_LABEL" => "LABEL",
            "ID_FS_UUID" => "UUID",
            "ID_FS_AVAIL" => "FSAVAIL",
            "ID_FS_USE" => "FSUSE%",
            "ID_SERIAL_SHORT" => "SERIAL",
            "ID_BUS" => "TRAN",
            _ => continue,
        };
        result.insert(key.into(), value.to_owned());
    }
    result
}

fn block_mountpoints() -> HashMap<String, Vec<String>> {
    let mut result = HashMap::new();
    let Ok(content) = fs::read_to_string("/proc/self/mountinfo") else {
        return result;
    };
    for line in content.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let Some(separator) = fields.iter().position(|field| *field == "-") else {
            continue;
        };
        if fields.len() <= separator + 2 || fields.len() <= 5 {
            continue;
        }
        let mountpoint = unescape_mount_field(fields[4]);
        let source = unescape_mount_field(fields[separator + 2]);
        if let Some(name) = Path::new(&source).file_name().and_then(OsStr::to_str) {
            result
                .entry(name.to_owned())
                .or_insert_with(Vec::new)
                .push(mountpoint.clone());
        }
    }
    result
}

fn is_scsi(device: &BlockDevice) -> bool {
    device
        .sys_path
        .components()
        .any(|component| component.as_os_str().to_string_lossy().contains("target"))
}

fn block_value(device: &BlockDevice, column: &str, options: &LsblkOptions) -> String {
    match column {
        "NAME" => {
            if options.paths {
                format!("/dev/{}", device.name)
            } else {
                device.name.clone()
            }
        }
        "KNAME" => device.name.clone(),
        "PATH" => format!("/dev/{}", device.name),
        "MAJ:MIN" => device.devno.clone(),
        "SIZE" => {
            let bytes = device.size_sectors.saturating_mul(512);
            if options.bytes {
                bytes.to_string()
            } else {
                human_size(bytes)
            }
        }
        "TYPE" => device.kind.clone(),
        "MOUNTPOINT" => device.mountpoints.first().cloned().unwrap_or_default(),
        "MOUNTPOINTS" => device.mountpoints.join(","),
        "PKNAME" => device.parent.clone().unwrap_or_default(),
        "HCTL" => hctl(device),
        "SUBSYSTEMS" => "block".into(),
        _ => device.attrs.get(column).cloned().unwrap_or_default(),
    }
}

fn hctl(device: &BlockDevice) -> String {
    let components = device
        .sys_path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    components
        .iter()
        .find_map(|component| component.strip_prefix("target").map(str::to_owned))
        .unwrap_or_default()
}

fn visible_block_roots(devices: &[BlockDevice], options: &LsblkOptions) -> Vec<String> {
    let names = devices
        .iter()
        .map(|device| device.name.clone())
        .collect::<HashSet<_>>();
    if options.inverse {
        devices
            .iter()
            .filter(|device| device.children.is_empty())
            .map(|device| device.name.clone())
            .collect()
    } else {
        devices
            .iter()
            .filter(|device| {
                device
                    .parent
                    .as_ref()
                    .is_none_or(|parent| !names.contains(parent))
            })
            .map(|device| device.name.clone())
            .collect()
    }
}

fn print_block_table(devices: &[BlockDevice], columns: &[String], options: &LsblkOptions) {
    let by_name = devices
        .iter()
        .map(|device| (device.name.as_str(), device))
        .collect::<HashMap<_, _>>();
    let roots = visible_block_roots(devices, options);
    let mut rows = Vec::<(usize, &BlockDevice)>::new();
    for root in roots {
        collect_block_rows(&root, 0, &by_name, options, &mut rows, &mut HashSet::new());
    }
    if options.list || options.merge {
        rows.sort_unstable_by(|left, right| left.1.name.cmp(&right.1.name));
    }
    let tree = options.tree || (!options.list && !options.raw && !options.pairs);
    let mut values = Vec::new();
    for (depth, device) in rows {
        let mut row = columns
            .iter()
            .map(|column| block_value(device, column, options))
            .collect::<Vec<_>>();
        if tree && !row.is_empty() && columns[0] == "NAME" {
            let branch = if options.ascii {
                if depth == 0 { "" } else { "`- " }
            } else if depth == 0 {
                ""
            } else {
                "└─ "
            };
            row[0] = format!("{}{}", "  ".repeat(depth.saturating_sub(1)), branch) + &row[0];
        }
        if let Some(width) = options.width {
            for value in &mut row {
                *value = truncate(value, width);
            }
        }
        values.push(row);
    }
    if options.pairs {
        for row in &values {
            println!(
                "{}",
                columns
                    .iter()
                    .zip(row)
                    .map(|(column, value)| format!(
                        "{}=\"{}\"",
                        if options.shell {
                            shell_column(column)
                        } else {
                            column.clone()
                        },
                        pair_escape(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        return;
    }
    let mut widths = columns.iter().map(String::len).collect::<Vec<_>>();
    for row in &values {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.len());
        }
    }
    if !options.no_headings {
        if options.raw {
            println!(
                "{}",
                columns
                    .iter()
                    .map(|column| if options.shell {
                        shell_column(column)
                    } else {
                        column.clone()
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        } else {
            println!(
                "{}",
                columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let column = if options.shell {
                            shell_column(column)
                        } else {
                            column.clone()
                        };
                        format!("{column:<width$}", width = widths[index])
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim_end()
            );
        }
    }
    for row in values {
        println!(
            "{}",
            row.into_iter()
                .enumerate()
                .map(|(index, value)| {
                    if options.raw {
                        value
                    } else {
                        format!("{value:<width$}", width = widths[index])
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
                .trim_end()
        );
    }
}

fn shell_column(column: &str) -> String {
    column
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn pair_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn collect_block_rows<'a>(
    name: &str,
    depth: usize,
    by_name: &HashMap<&'a str, &'a BlockDevice>,
    options: &LsblkOptions,
    rows: &mut Vec<(usize, &'a BlockDevice)>,
    seen: &mut HashSet<String>,
) {
    if !seen.insert(name.to_owned()) {
        return;
    }
    let Some(device) = by_name.get(name).copied() else {
        return;
    };
    rows.push((depth, device));
    if options.nodeps {
        return;
    }
    if options.inverse {
        for parent in by_name.values().filter(|candidate| {
            candidate
                .children
                .iter()
                .any(|child| child.as_str() == name)
        }) {
            collect_block_rows(&parent.name, depth + 1, by_name, options, rows, seen);
        }
    } else {
        for child in &device.children {
            collect_block_rows(child, depth + 1, by_name, options, rows, seen);
        }
    }
}

fn print_block_json(devices: &[BlockDevice], columns: &[String], options: &LsblkOptions) {
    let by_name = devices
        .iter()
        .map(|device| (device.name.as_str(), device))
        .collect::<HashMap<_, _>>();
    let roots = visible_block_roots(devices, options);
    let values = roots
        .iter()
        .filter_map(|root| json_block_device(root, &by_name, columns, options, &mut HashSet::new()))
        .collect::<Vec<_>>();
    let mut object = JsonMap::new();
    object.insert("blockdevices".into(), JsonValue::Array(values));
    let json = JsonValue::Object(object);
    let result = if std::env::var_os("LIBSMARTCOLS_JSON").as_deref() == Some(OsStr::new("compact"))
    {
        serde_json::to_string(&json)
    } else {
        serde_json::to_string_pretty(&json)
    };
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => eprintln!("lsblk: cannot encode JSON: {error}"),
    }
}

fn json_block_device<'a>(
    name: &str,
    by_name: &HashMap<&'a str, &'a BlockDevice>,
    columns: &[String],
    options: &LsblkOptions,
    seen: &mut HashSet<String>,
) -> Option<JsonValue> {
    if !seen.insert(name.to_owned()) {
        return None;
    }
    let device = by_name.get(name).copied()?;
    let mut object = JsonMap::new();
    for column in columns {
        let value = block_value(device, column, options);
        if column == "SIZE" && options.bytes {
            object.insert(
                column.to_ascii_lowercase(),
                value.parse::<u64>().unwrap_or_default().into(),
            );
        } else {
            object.insert(column.to_ascii_lowercase(), value.into());
        }
    }
    if !options.nodeps {
        let children = if options.inverse {
            by_name
                .values()
                .filter(|candidate| {
                    candidate
                        .children
                        .iter()
                        .any(|child| child.as_str() == name)
                })
                .filter_map(|parent| {
                    json_block_device(&parent.name, by_name, columns, options, seen)
                })
                .collect::<Vec<_>>()
        } else {
            device
                .children
                .iter()
                .filter_map(|child| json_block_device(child, by_name, columns, options, seen))
                .collect::<Vec<_>>()
        };
        if !children.is_empty() {
            object.insert("children".into(), JsonValue::Array(children));
        }
    }
    Some(JsonValue::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_string_identifies_character_devices() {
        let metadata = fs::metadata("/dev/null").expect("/dev/null");
        assert_eq!(mode_string(&metadata).len(), 10);
        assert_eq!(mode_string(&metadata).as_bytes()[0], b'c');
    }

    #[test]
    fn lock_parser_handles_eof_and_device_numbers() {
        let record =
            parse_lock_line("12: POSIX ADVISORY WRITE 42 08:01:1234 10 EOF").expect("lock");
        assert_eq!(record.id, 12);
        assert_eq!(record.pid, 42);
        assert_eq!((record.major, record.minor), (8, 1));
        assert_eq!(record.inode, 1234);
        assert_eq!(record.end, None);
    }

    #[test]
    fn lock_size_is_stable_for_human_and_byte_modes() {
        let record = LockRecord {
            id: 1,
            lock_type: "POSIX".into(),
            mandatory: false,
            mode: "READ".into(),
            pid: 1,
            major: 8,
            minor: 1,
            inode: 2,
            start: 0,
            end: Some(2047),
            path: None,
            file_size: Some(2048),
            command: "init".into(),
        };
        assert_eq!(lock_size(&record, true), "2048");
        assert_eq!(lock_size(&record, false), "2.0K");
    }

    #[test]
    fn socket_query_parser_accepts_local_and_remote_ports() {
        let query = parse_socket_query(&OsString::from("8080,127.0.0.1,443")).expect("query");
        assert_eq!(query.local_port, Some(8080));
        assert_eq!(query.remote_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(query.remote_port, Some(443));
    }

    #[test]
    fn process_stat_parser_handles_parenthesized_command_names() {
        let stat = "42 (worker (io)) S 7 42 42 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        assert_eq!(
            parse_process_stat(stat),
            Some(("worker (io)".into(), 7, 42))
        );
    }

    #[test]
    fn block_columns_are_case_insensitive() {
        assert_eq!(
            parse_block_columns("name,maj:min,size").expect("columns"),
            vec!["NAME".to_owned(), "MAJ:MIN".to_owned(), "SIZE".to_owned()]
        );
        assert!(parse_block_columns("NO_SUCH_COLUMN").is_err());
    }

    #[test]
    fn lock_columns_reject_unknown_values() {
        assert_eq!(
            parse_lock_columns("PID,COMMAND").expect("columns"),
            vec!["PID".to_owned(), "COMMAND".to_owned()]
        );
        assert!(parse_lock_columns("NO_SUCH_COLUMN").is_err());
    }

    #[test]
    fn lock_parser_accepts_ofd_unknown_pid() {
        let record = parse_lock_line("7: OFDLCK ADVISORY WRITE -1 00:05:9 0 EOF").expect("lock");
        assert_eq!(record.pid, -1);
        assert_eq!(record.end, None);
    }

    #[test]
    fn pair_formatter_escapes_values_and_shell_columns() {
        assert_eq!(shell_column("FSUSE%"), "FSUSE_");
        assert_eq!(pair_escape(r#"a\"b"#), r#"a\\\"b"#);
    }
}
