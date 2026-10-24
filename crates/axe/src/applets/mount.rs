//! util-linux compatible `mount`: list mounts, mount filesystems, and change
//! existing mounts (remount, bind, move, propagation).

use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, DirBuilder};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use libc::c_ulong;

use super::errno::strerror;
use super::linux_storage::probe;

mod getopt;
mod loop_device;
mod umount;

use getopt::{Applet, Getopt, Item, OptionSpec, Spelling};
use loop_device::{LoopError, LoopRequest};

const MOUNTINFO: &str = "/proc/self/mountinfo";
const FSTAB: &str = "/etc/fstab";
const FILESYSTEMS: &str = "/proc/filesystems";
/// util-linux `FS_SEARCH_PATH` for `/sbin/mount.<type>` helpers.
const HELPER_DIRS: [&str; 3] = ["/sbin", "/sbin/fs.d", "/sbin/fs"];

/// util-linux exit codes: incorrect invocation, mount failure, partial `-a` success.
const EXIT_USAGE: i32 = 1;
const EXIT_FAILURE: i32 = 32;
const EXIT_SOME_FAILED: i32 = 64;

const ATIME_FLAGS: c_ulong = libc::MS_NOATIME | libc::MS_RELATIME | libc::MS_STRICTATIME;
/// Per-mount-point flags. The kernel ignores them on `MS_BIND`, so a bind
/// mount applies them with a follow-up `remount,bind` like util-linux does.
const MOUNT_POINT_FLAGS: c_ulong = libc::MS_RDONLY
    | libc::MS_NOSUID
    | libc::MS_NODEV
    | libc::MS_NOEXEC
    | libc::MS_NODIRATIME
    | libc::MS_NOSYMFOLLOW
    | ATIME_FLAGS;

const USAGE: &str = "\
Usage:
 mount [-lhv]
 mount -a [options]
 mount [options] [--source] <source> | [--target] <directory>
 mount [options] <source> <directory>
 mount <operation> <mountpoint> [<target>]

Mount a filesystem.

Options:
 -a, --all               mount all filesystems mentioned in fstab
 -c, --no-canonicalize   don't canonicalize paths
 -f, --fake              dry run; skip the mount(2) syscall
 -i, --internal-only     don't call the mount.<type> helpers
 -l, --show-labels       show also filesystem labels
 -m, --mkdir[=<mode>]    create the target directory (default mode 0755)
 -n, --no-mtab           accepted for compatibility (no mtab is written)
 -r, --read-only         mount the filesystem read-only (same as -o ro)
 -s, --sloppy            pass the sloppy flag to mount.<type> helpers
 -t, --types <list>      limit the set of filesystem types
 -o, --options <list>    comma-separated list of mount options
 -O, --test-opts <list>  limit the set of filesystems (use with -a)
 -v, --verbose           say what is being done
 -w, --rw, --read-write  mount the filesystem read-write (default)
 -L, --label <label>     synonym for LABEL=<label>
 -U, --uuid <uuid>       synonym for UUID=<uuid>
     --source <src>      explicitly specifies source (path, label, uuid)
     --target <target>   explicitly specifies mountpoint
 -h, --help              display this help

Operations:
 -B, --bind              mount a subtree somewhere else (same as -o bind)
 -M, --move              move a subtree to some other place
 -R, --rbind             mount a subtree and all submounts somewhere else
 --make-shared           mark a subtree as shared
 --make-slave            mark a subtree as slave
 --make-private          mark a subtree as private
 --make-unbindable       mark a subtree as unbindable
 --make-rshared          recursively mark a whole subtree as shared
 --make-rslave           recursively mark a whole subtree as slave
 --make-rprivate         recursively mark a whole subtree as private
 --make-runbindable      recursively mark a whole subtree as unbindable";

pub fn mount(args: Vec<OsString>) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(failure) => {
            if !failure.message.is_empty() {
                eprintln!("mount: {}", failure.message);
            }
            failure.code
        }
    }
}

pub fn umount(args: Vec<OsString>) -> i32 {
    umount::run(args)
}

#[derive(Debug)]
struct Failure {
    code: i32,
    message: String,
    /// The source device does not exist: the only failure `nofail` silences.
    missing_source: bool,
}

const DMESG_HINT: &str =
    "       dmesg(1) may have more information after failed mount system call.";

fn usage(message: impl Into<String>) -> Failure {
    Failure {
        code: EXIT_USAGE,
        message: message.into(),
        missing_source: false,
    }
}

/// Command-line errors util-linux follows with its `--help` hint.
fn bad_usage(message: impl std::fmt::Display) -> Failure {
    Applet::Mount.bad_usage(message)
}

fn failed(message: impl Into<String>) -> Failure {
    Failure {
        code: EXIT_FAILURE,
        message: message.into(),
        missing_source: false,
    }
}

/// A failure util-linux reports after attempting the mount, with its dmesg hint.
fn mount_failed(message: impl std::fmt::Display) -> Failure {
    failed(format!("{message}\n{DMESG_HINT}"))
}

impl Failure {
    fn missing_source(self) -> Self {
        Self {
            missing_source: true,
            ..self
        }
    }
}

fn run(args: Vec<OsString>) -> Result<i32, Failure> {
    let args = match Args::parse(args)? {
        Parsed::Run(args) => args,
        Parsed::Help => {
            println!("{USAGE}");
            return Ok(0);
        }
        Parsed::Version => {
            println!("mount from axe {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
    };
    if args.operation && (args.types.is_some() || args.all || args.source.is_some()) {
        return Err(bad_usage("bad usage"));
    }
    if args.all {
        // util-linux ignores operands with --all.
        return mount_all(&args);
    }
    if args.operands.is_empty()
        && args.source.is_none()
        && args.target.is_none()
        && args.options.is_empty()
    {
        list(&args)?;
        return Ok(0);
    }
    let plan = command_line_plan(&args)?;
    execute(&plan, &args, args.verbose, None)?;
    Ok(0)
}

enum Parsed {
    Run(Args),
    Help,
    Version,
}

#[derive(Default)]
struct Args {
    all: bool,
    fake: bool,
    verbose: bool,
    internal_only: bool,
    sloppy: bool,
    no_canonicalize: bool,
    show_labels: bool,
    mkdir: Option<u32>,
    types: Option<String>,
    /// `-O`: fstab option filter for `--all`.
    test_opts: Option<String>,
    /// `-o` groups and option-equivalent flags (`-r`, `-B`, ...) in command-line order.
    options: Vec<String>,
    /// `--bind`, `--rbind`, `--move`, or `--make-*` given as a command-line
    /// operation: util-linux then skips fstab and rejects `-t`, `-a`, and `--source`.
    operation: bool,
    source: Option<OsString>,
    target: Option<OsString>,
    operands: Vec<OsString>,
}

/// The table order also defines ambiguous long-option candidates.
const OPTIONS: &[OptionSpec] = &[
    OptionSpec::flag("all", b'a'),
    OptionSpec::flag("fake", b'f'),
    OptionSpec::flag("fork", b'F'),
    OptionSpec::value("fstab", b'T'),
    OptionSpec::flag("help", b'h'),
    OptionSpec::flag("no-mtab", b'n'),
    OptionSpec::value("options", b'o'),
    OptionSpec::value("test-opts", b'O'),
    OptionSpec::value("pass-fd", b'p'),
    OptionSpec::flag("read-only", b'r'),
    OptionSpec::flag("rw", b'w'),
    OptionSpec::flag("read-write", 0),
    OptionSpec::flag("sloppy", b's'),
    OptionSpec::flag("show-labels", b'l'),
    OptionSpec::flag("internal-only", b'i'),
    OptionSpec::value("types", b't'),
    OptionSpec::value("uuid", b'U'),
    OptionSpec::value("label", b'L'),
    OptionSpec::flag("no-canonicalize", b'c'),
    OptionSpec::flag("version", b'V'),
    OptionSpec::flag("verbose", b'v'),
    OptionSpec::flag("bind", b'B'),
    OptionSpec::flag("move", b'M'),
    OptionSpec::flag("rbind", b'R'),
    OptionSpec::flag("make-shared", 0),
    OptionSpec::flag("make-slave", 0),
    OptionSpec::flag("make-private", 0),
    OptionSpec::flag("make-unbindable", 0),
    OptionSpec::flag("make-rshared", 0),
    OptionSpec::flag("make-rslave", 0),
    OptionSpec::flag("make-rprivate", 0),
    OptionSpec::flag("make-runbindable", 0),
    OptionSpec::optional("mkdir", b'm'),
    OptionSpec::value("namespace", b'N'),
    OptionSpec::value("map-users", 0),
    OptionSpec::value("map-groups", 0),
    OptionSpec::value("target", 0),
    OptionSpec::value("target-prefix", 0),
    OptionSpec::value("source", 0),
    OptionSpec::value("options-mode", 0),
    OptionSpec::value("options-source", 0),
    OptionSpec::flag("options-source-force", 0),
    OptionSpec::flag("onlyonce", 0),
    OptionSpec::flag("exclusive", 0),
    OptionSpec::flag("beneath", 0),
];

impl Args {
    fn parse(args: Vec<OsString>) -> Result<Parsed, Failure> {
        let mut parsed = Self::default();
        let mut options = Getopt::new(args, Applet::Mount, OPTIONS);
        while let Some(item) = options.next()? {
            match item {
                Item::Operand(operand) => parsed.operands.push(operand),
                Item::Option("mkdir", value, _) => {
                    parsed.mkdir = Some(mkdir_mode(value.as_deref())?);
                }
                Item::Option(name, value, spelling) => {
                    if let Some(flow) = parsed.apply(name, value, spelling)? {
                        return Ok(flow);
                    }
                }
            }
        }
        Ok(Parsed::Run(parsed))
    }

    /// Applies one option; `Some` ends parsing (`--help`, `--version`).
    fn apply(
        &mut self,
        name: &str,
        value: Option<OsString>,
        spelling: Spelling,
    ) -> Result<Option<Parsed>, Failure> {
        let text = |value: Option<OsString>| {
            value.unwrap_or_default().into_string().map_err(|value| {
                bad_usage(format!("argument is not valid UTF-8: {}", value.display()))
            })
        };
        match name {
            "all" => self.all = true,
            "fake" => self.fake = true,
            "verbose" => self.verbose = true,
            "internal-only" => self.internal_only = true,
            "sloppy" => self.sloppy = true,
            "no-mtab" => {}
            "no-canonicalize" => self.no_canonicalize = true,
            "show-labels" => self.show_labels = true,
            "read-only" => self.options.push("ro".into()),
            "rw" | "read-write" => self.options.push("rw".into()),
            "bind" | "rbind" | "move" => {
                self.operation = true;
                self.options.push(name.into());
            }
            "make-shared" | "make-slave" | "make-private" | "make-unbindable" | "make-rshared"
            | "make-rslave" | "make-rprivate" | "make-runbindable" => {
                self.operation = true;
                self.options.push(name["make-".len()..].into());
            }
            "types" => self.types = Some(text(value)?),
            "test-opts" => self.test_opts = Some(text(value)?),
            "options" => self.options.push(text(value)?),
            "label" | "uuid" => {
                let mut source = OsString::from(if name == "label" {
                    "LABEL=\""
                } else {
                    "UUID=\""
                });
                source.push(value.unwrap_or_default());
                source.push("\"");
                self.source = Some(source);
            }
            "source" => self.source = value,
            "target" => self.target = value,
            "help" => return Ok(Some(Parsed::Help)),
            "version" => return Ok(Some(Parsed::Version)),
            _ => {
                return Err(usage(format!(
                    "unsupported option '{}'",
                    spelling.display(name)
                )));
            }
        }
        Ok(None)
    }
}

fn mkdir_mode(value: Option<&OsStr>) -> Result<u32, Failure> {
    let Some(value) = value else {
        return Ok(0o755);
    };
    value
        .to_str()
        .and_then(|value| u32::from_str_radix(value, 8).ok())
        .filter(|mode| *mode <= 0o7777)
        .ok_or_else(|| usage(format!("invalid mkdir mode '{}'", value.display())))
}

/// Mount options split into kernel flags, filesystem data, and the userspace
/// options util-linux interprets itself.
#[derive(Debug, Default, PartialEq)]
struct MountOptions {
    /// `MS_REMOUNT`, `MS_BIND`, `MS_REC` (rbind), `MS_MOVE`.
    operation: c_ulong,
    /// Flags explicitly turned on and off; unspecified flags keep their current value on remount.
    set: c_ulong,
    clear: c_ulong,
    /// Propagation changes, applied one `mount(2)` call each after the main operation.
    propagation: Vec<c_ulong>,
    /// Filesystem-specific options passed through as `mount(2)` data.
    data: Vec<String>,
    /// Every option, for `mount.<type>` helpers.
    all: Vec<String>,
    noauto: bool,
    nofail: bool,
    mkdir: Option<u32>,
    /// `loop`, `loop=`, `offset=`, `sizelimit=`: back the source with a loop device.
    loop_request: Option<LoopRequest>,
    /// `encryption=`: cryptoloop, which util-linux rejects.
    encryption: bool,
}

impl MountOptions {
    fn parse<'a>(groups: impl IntoIterator<Item = &'a str>) -> Result<Self, Failure> {
        let mut options = Self::default();
        for group in groups {
            for option in split_options(group) {
                options.apply(option)?;
            }
        }
        Ok(options)
    }

    fn apply(&mut self, option: &str) -> Result<(), Failure> {
        self.all.push(option.to_owned());
        let name = option.split_once('=').map_or(option, |(name, _)| name);
        match option {
            "defaults" | "auto" | "nouser" | "noowner" | "nogroup" | "_netdev" => {}
            // util-linux applies the implied restrictions even for root;
            // later options (`exec`, `suid`, `dev`) override them.
            "user" | "users" => self.flag(libc::MS_NOEXEC | libc::MS_NOSUID | libc::MS_NODEV, true),
            "owner" | "group" => self.flag(libc::MS_NOSUID | libc::MS_NODEV, true),
            "noauto" => self.noauto = true,
            "nofail" => self.nofail = true,
            "remount" => self.operation |= libc::MS_REMOUNT,
            "bind" => self.operation |= libc::MS_BIND,
            "rbind" => self.operation |= libc::MS_BIND | libc::MS_REC,
            "move" => self.operation |= libc::MS_MOVE,
            "ro" => self.flag(libc::MS_RDONLY, true),
            "rw" => self.flag(libc::MS_RDONLY, false),
            "nosuid" => self.flag(libc::MS_NOSUID, true),
            "suid" => self.flag(libc::MS_NOSUID, false),
            "nodev" => self.flag(libc::MS_NODEV, true),
            "dev" => self.flag(libc::MS_NODEV, false),
            "noexec" => self.flag(libc::MS_NOEXEC, true),
            "exec" => self.flag(libc::MS_NOEXEC, false),
            "sync" => self.flag(libc::MS_SYNCHRONOUS, true),
            "async" => self.flag(libc::MS_SYNCHRONOUS, false),
            "dirsync" => self.flag(libc::MS_DIRSYNC, true),
            "mand" => self.flag(libc::MS_MANDLOCK, true),
            "nomand" => self.flag(libc::MS_MANDLOCK, false),
            "nodiratime" => self.flag(libc::MS_NODIRATIME, true),
            "diratime" => self.flag(libc::MS_NODIRATIME, false),
            "iversion" => self.flag(libc::MS_I_VERSION, true),
            "noiversion" => self.flag(libc::MS_I_VERSION, false),
            "lazytime" => self.flag(libc::MS_LAZYTIME, true),
            "nolazytime" => self.flag(libc::MS_LAZYTIME, false),
            "silent" => self.flag(libc::MS_SILENT, true),
            "loud" => self.flag(libc::MS_SILENT, false),
            "nosymfollow" => self.flag(libc::MS_NOSYMFOLLOW, true),
            "symfollow" => self.flag(libc::MS_NOSYMFOLLOW, false),
            "noatime" => self.atime(libc::MS_NOATIME),
            "atime" => self.flag(libc::MS_NOATIME, false),
            "relatime" => self.atime(libc::MS_RELATIME),
            "norelatime" => self.flag(libc::MS_RELATIME, false),
            "strictatime" => self.atime(libc::MS_STRICTATIME),
            "nostrictatime" => self.flag(libc::MS_STRICTATIME, false),
            "shared" => self.propagation.push(libc::MS_SHARED),
            "rshared" => self.propagation.push(libc::MS_SHARED | libc::MS_REC),
            "slave" => self.propagation.push(libc::MS_SLAVE),
            "rslave" => self.propagation.push(libc::MS_SLAVE | libc::MS_REC),
            "private" => self.propagation.push(libc::MS_PRIVATE),
            "rprivate" => self.propagation.push(libc::MS_PRIVATE | libc::MS_REC),
            "unbindable" => self.propagation.push(libc::MS_UNBINDABLE),
            "runbindable" => self.propagation.push(libc::MS_UNBINDABLE | libc::MS_REC),
            "loop" => {
                self.loop_request.get_or_insert_default();
            }
            _ if matches!(name, "loop" | "offset" | "sizelimit") => {
                let value = option.split_once('=').map_or("", |(_, value)| value);
                let request = self.loop_request.get_or_insert_default();
                match name {
                    "loop" => request.device = Some(PathBuf::from(value)),
                    "offset" => request.offset = parse_size(value)?,
                    _ => request.size_limit = parse_size(value)?,
                }
            }
            _ if name == "encryption" => self.encryption = true,
            _ if name.eq_ignore_ascii_case("x-mount.mkdir") => {
                self.mkdir = Some(mkdir_mode(
                    option.split_once('=').map(|(_, mode)| OsStr::new(mode)),
                )?);
            }
            _ if name.starts_with("x-")
                || name.starts_with("X-")
                || matches!(name, "comment" | "helper") => {}
            _ => self.data.push(unquote_value(option)),
        }
        Ok(())
    }

    fn flag(&mut self, flag: c_ulong, on: bool) {
        if on {
            self.set |= flag;
            self.clear &= !flag;
        } else {
            self.clear |= flag;
            self.set &= !flag;
        }
    }

    /// The atime modes are mutually exclusive: selecting one drops the others.
    fn atime(&mut self, flag: c_ulong) {
        self.flag(ATIME_FLAGS & !flag, false);
        self.flag(flag, true);
    }

    /// Current flags updated with the explicitly requested changes.
    fn merged(&self, current: c_ulong) -> c_ulong {
        let mut flags = (current & !self.clear) | self.set;
        if flags & ATIME_FLAGS == 0 && self.clear & ATIME_FLAGS != 0 {
            // `atime`/`norelatime` without a replacement mode means strictatime.
            flags |= libc::MS_STRICTATIME;
        }
        flags
    }

    fn propagation_only(&self) -> bool {
        !self.propagation.is_empty()
            && self.operation == 0
            && self.set == 0
            && self.clear == 0
            && self.data.is_empty()
    }

    fn data(&self) -> Option<String> {
        (!self.data.is_empty()).then(|| self.data.join(","))
    }
}

/// Splits a comma-separated option list, keeping commas inside double quotes
/// (`context="system_u:object_r:tmp_t:s0:c127,c456"`).
fn split_options(list: &str) -> impl Iterator<Item = &str> {
    let mut quoted = false;
    list.split(move |character| {
        if character == '"' {
            quoted = !quoted;
        }
        character == ',' && !quoted
    })
    .filter(|option| !option.is_empty())
}

/// Drops the quotes around a value (`mode="0750"`) as libmount does before
/// handing it to the kernel. Values with commas keep them: the kernel's
/// LSM option parser needs the quotes to keep `context="a,b"` intact.
fn unquote_value(option: &str) -> String {
    match option.split_once('=') {
        Some((name, value))
            if value.len() >= 2
                && value.starts_with('"')
                && value.ends_with('"')
                && !value.contains(',') =>
        {
            format!("{name}={}", &value[1..value.len() - 1])
        }
        _ => option.to_owned(),
    }
}

/// What to mount, before path and source resolution.
struct Plan {
    source: Option<OsString>,
    target: OsString,
    fs_type: Option<String>,
    /// `fs_type` came from `-t` and may be a negated list; fstab types are literal.
    type_pattern: bool,
    /// Option groups in precedence order (fstab first, then command line).
    options: Vec<String>,
}

fn command_line_plan(args: &Args) -> Result<Plan, Failure> {
    let mut operands = args.operands.iter().cloned();
    let (source, target, either) = match (
        args.source.clone(),
        args.target.clone(),
        args.operands.len(),
    ) {
        (None, None, 2) => (operands.next(), operands.next(), None),
        (None, None, 1) => (None, None, operands.next()),
        (Some(source), None, 1) => (Some(source), operands.next(), None),
        (None, Some(target), 1) => (operands.next(), Some(target), None),
        (None, None, 0) => return Err(bad_usage("bad usage")),
        (source, target, 0) => (source, target, None),
        _ => return Err(bad_usage("bad usage")),
    };
    if let (Some(source), Some(target)) = (&source, &target) {
        return Ok(Plan {
            source: Some(source.clone()),
            target: target.clone(),
            fs_type: args.types.clone(),
            type_pattern: true,
            options: args.options.clone(),
        });
    }
    let Some(spec) = target.clone().or_else(|| source.clone()).or(either) else {
        return Err(bad_usage("bad usage"));
    };
    let cli = MountOptions::parse(args.options.iter().map(String::as_str))?;
    // A command-line operation (`--bind`, `--make-*`, ...) makes util-linux skip fstab.
    let entries = if args.operation {
        Vec::new()
    } else {
        read_fstab(FSTAB)?
    };

    if cli.operation & libc::MS_REMOUNT != 0 {
        let target = remount_target(&spec, args)?;
        // util-linux merges the first fstab entry of the mountpoint before `-o`.
        let mut options = Vec::new();
        if let Some(entry) = entries
            .into_iter()
            .find(|entry| same_path(&entry.target, &target))
        {
            options.push(entry.options);
        }
        options.extend(args.options.iter().cloned());
        return Ok(Plan {
            source: None,
            target: target.into_os_string(),
            fs_type: args.types.clone(),
            type_pattern: true,
            options,
        });
    }
    if args.operation {
        if cli.propagation_only() {
            return Ok(Plan {
                source: None,
                target: spec,
                fs_type: None,
                type_pattern: false,
                options: args.options.clone(),
            });
        }
        // util-linux attempts the bind or move without a source.
        let target = fs::canonicalize(&spec).unwrap_or_else(|_| PathBuf::from(&spec));
        return Err(usage(format!(
            "{}: mount failed: Invalid argument.",
            target.display()
        )));
    }

    let by_target = |spec: &OsStr| {
        entries
            .iter()
            .find(|entry| same_path(&entry.target, Path::new(spec)))
    };
    let by_source = |spec: &OsStr| {
        entries
            .iter()
            .find(|entry| same_source(&entry.source, spec))
    };
    let entry = match (&source, &target) {
        (Some(source), None) => by_source(source).ok_or_else(|| {
            usage(format!(
                "{}: can't find mount source {} in {FSTAB}.",
                source.display(),
                source.display()
            ))
        }),
        (None, Some(target)) => by_target(target).ok_or_else(|| {
            usage(format!(
                "{}: can't find mount point in {FSTAB}.",
                target.display()
            ))
        }),
        _ => by_target(&spec)
            .or_else(|| by_source(&spec))
            .ok_or_else(|| usage(format!("{}: can't find in {FSTAB}.", spec.display()))),
    }?;
    let mut options = vec![entry.options.clone()];
    options.extend(args.options.iter().cloned());
    Ok(Plan {
        source: Some(entry.source.clone()),
        target: entry.target.clone().into_os_string(),
        fs_type: args.types.clone().or_else(|| Some(entry.fs_type.clone())),
        type_pattern: args.types.is_some(),
        options,
    })
}

/// Resolves the operand of a single-operand remount: a mountpoint or a mounted source.
fn remount_target(spec: &OsStr, args: &Args) -> Result<PathBuf, Failure> {
    let path = if args.no_canonicalize {
        PathBuf::from(spec)
    } else {
        fs::canonicalize(spec).unwrap_or_else(|_| PathBuf::from(spec))
    };
    let table = mount_table()?;
    if table.iter().any(|entry| entry.target == path) {
        return Ok(path);
    }
    Ok(table
        .iter()
        .rev()
        .find(|entry| entry.source == spec || Path::new(&entry.source) == path)
        .map_or(path, |entry| entry.target.clone()))
}

/// Performs one mount; `verbose` prints util-linux's single-mount status line.
/// `mount_all_table` is the table `--all` read up front, which util-linux
/// keeps using for its diagnostics during the whole run.
fn execute(
    plan: &Plan,
    args: &Args,
    verbose: bool,
    mount_all_table: Option<&[MountEntry]>,
) -> Result<(), Failure> {
    let mut options = MountOptions::parse(plan.options.iter().map(String::as_str))?;
    if let Some(mode) = args.mkdir.or(options.mkdir)
        && !args.fake
    {
        DirBuilder::new()
            .recursive(true)
            .mode(mode)
            .create(&plan.target)
            .map_err(|error| {
                failed(format!(
                    "{}: failed to create mountpoint: {}",
                    plan.target.display(),
                    strerror(&error)
                ))
            })?;
    }
    let target = if args.no_canonicalize {
        PathBuf::from(&plan.target)
    } else {
        fs::canonicalize(&plan.target).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                mount_failed(format!(
                    "{}: mount point does not exist.",
                    plan.target.display()
                ))
            } else {
                mount_failed(format!("{}: {}.", plan.target.display(), strerror(&error)))
            }
        })?
    };
    let remount = options.operation & libc::MS_REMOUNT != 0;
    let propagation_only = !remount && options.propagation_only() && plan.source.is_none();
    let source = if remount || propagation_only {
        None
    } else {
        let spec = plan
            .source
            .as_deref()
            .ok_or_else(|| bad_usage("bad usage"))?;
        // util-linux names the target as given in tag lookup errors.
        Some(resolve_source(spec, Path::new(&plan.target), args)?)
    };
    // A new filesystem on a regular file goes through a loop device; the
    // handle keeps a fresh autoclear device attached until the mount holds it.
    let mut attached = match &source {
        Some(file)
            if !remount
                && options.operation & (libc::MS_BIND | libc::MS_MOVE) == 0
                && !args.fake =>
        {
            attach_loop(file, plan, &options)?
        }
        _ => None,
    };
    if let Some(device) = &attached
        && device.reused
        && mount_table()?
            .iter()
            .any(|entry| entry.target == target && Path::new(&entry.source) == device.device)
    {
        // util-linux refuses to stack the same file on the same mountpoint.
        return Err(usage(format!(
            "{}: {} is already mounted.",
            plan.target.display(),
            source.as_deref().unwrap_or_default().display()
        )));
    }
    if attached.as_ref().is_some_and(|attached| attached.read_only) {
        options.flag(libc::MS_RDONLY, true);
    }
    let source = attached
        .as_ref()
        .map(|attached| attached.device.clone().into_os_string())
        .or(source);
    let call = Call {
        target: &target,
        source: source.as_deref(),
        fs_type: None,
        kind: Kind::Mount,
        fake: args.fake,
        mount_table: mount_all_table,
    };

    if remount {
        let bind = options.operation & libc::MS_BIND != 0;
        let table = mount_table()?;
        let entry = table.iter().rev().find(|entry| entry.target == target);
        let current = entry.map_or(0, |entry| {
            // A plain remount also reconfigures the superblock.
            entry.mount_flags() | if bind { 0 } else { entry.super_flags() }
        });
        let call = Call {
            kind: Kind::Remount,
            ..call
        };
        if !bind && options.set & libc::MS_DIRSYNC != 0 {
            // The kernel cannot reconfigure dirsync; util-linux reports the
            // EINVAL that legacy mount(2) would silently swallow.
            return Err(call.failure(io::Error::from_raw_os_error(libc::EINVAL)));
        }
        let flags =
            options.merged(current) | (options.operation & (libc::MS_REMOUNT | libc::MS_BIND));
        let data = if bind { None } else { options.data() };
        call.mount(flags, data.as_deref())?;
        if verbose {
            let source = entry.map_or(OsStr::new("none"), |entry| entry.source.as_os_str());
            let verb = if bind { "bound on" } else { "mounted on" };
            println!("mount: {} {verb} {}.", source.display(), target.display());
        }
    } else if let Some(source) = &source {
        let verb = if options.operation & libc::MS_MOVE != 0 {
            Call {
                kind: Kind::Move,
                ..call
            }
            .mount(libc::MS_MOVE, None)?;
            "moved to"
        } else if options.operation & libc::MS_BIND != 0 {
            Call {
                kind: Kind::Bind,
                ..call
            }
            .mount(options.operation & (libc::MS_BIND | libc::MS_REC), None)?;
            if (options.set | options.clear) & MOUNT_POINT_FLAGS != 0 {
                let current = if args.fake {
                    0
                } else {
                    mount_table()?
                        .iter()
                        .rev()
                        .find(|entry| entry.target == target)
                        .map_or(0, MountEntry::mount_flags)
                };
                Call {
                    source: None,
                    kind: Kind::Remount,
                    ..call
                }
                .mount(
                    options.merged(current) | libc::MS_REMOUNT | libc::MS_BIND,
                    None,
                )?;
            }
            "bound on"
        } else {
            let mounted = new_mount(&call, plan, &options, args);
            if !matches!(mounted, Ok(None | Some(0)))
                && let Some(attached) = attached.take()
            {
                attached.release_unused();
            }
            if let Some(code) = mounted? {
                return match code {
                    0 => Ok(()),
                    code => Err(Failure {
                        code,
                        message: String::new(),
                        missing_source: false,
                    }),
                };
            }
            "mounted on"
        };
        if verbose {
            println!("mount: {} {verb} {}.", source.display(), target.display());
        }
    }

    let propagation = Call {
        source: None,
        kind: Kind::Propagation,
        ..call
    };
    for &flags in &options.propagation {
        propagation.mount(flags, None)?;
    }
    if verbose && propagation_only {
        println!("mount: {} propagation flags changed.", target.display());
    }
    Ok(())
}

/// Mounts a new filesystem; returns the exit status when a `mount.<type>` helper did the work.
fn new_mount(
    call: &Call<'_>,
    plan: &Plan,
    options: &MountOptions,
    args: &Args,
) -> Result<Option<i32>, Failure> {
    let source = call.source.expect("new mounts have a source");
    let types = filesystem_types(
        plan.fs_type.as_deref(),
        plan.type_pattern,
        source,
        call.target,
    )?;
    if let [fs_type] = types.as_slice()
        && !args.internal_only
        && !args.fake
        && let Some(helper) = HELPER_DIRS
            .iter()
            .map(|directory| Path::new(directory).join(format!("mount.{fs_type}")))
            .find(|helper| helper.is_file())
    {
        let mut command = Command::new(&helper);
        command.arg(source).arg(call.target);
        for (enabled, flag) in [(args.sloppy, "-s"), (args.fake, "-f"), (args.verbose, "-v")] {
            if enabled {
                command.arg(flag);
            }
        }
        if !options.all.is_empty() {
            command.arg("-o").arg(options.all.join(","));
        }
        let status = command
            .status()
            .map_err(|error| failed(format!("{}: {}", helper.display(), strerror(&error))))?;
        return Ok(Some(status.code().unwrap_or(EXIT_FAILURE)));
    }

    let mut last = None;
    for fs_type in &types {
        let attempt = Call {
            fs_type: Some(fs_type),
            ..*call
        };
        match attempt.create(options, options.set) {
            Ok(()) => return Ok(None),
            Err(error)
                if matches!(error.errno(), Some(libc::EROFS | libc::EACCES))
                    && options.set & libc::MS_RDONLY == 0
                    && options.clear & libc::MS_RDONLY == 0
                    && is_block_device(source) =>
            {
                // util-linux retries write-protected devices read-only unless `-w` was explicit.
                match attempt.create(options, options.set | libc::MS_RDONLY) {
                    Ok(()) => {
                        eprintln!(
                            "mount: {}: WARNING: source write-protected, mounted read-only.",
                            call.target.display()
                        );
                        return Ok(None);
                    }
                    Err(error) => return Err(attempt.attach_failure(error)),
                }
            }
            Err(error) => {
                let retry = matches!(error.errno(), Some(libc::EINVAL | libc::ENODEV));
                last = Some(attempt.attach_failure(error));
                if !retry {
                    break;
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| call.failure(io::Error::from_raw_os_error(libc::ENODEV))))
}

/// A failed attach plus the kernel's fs-context message (`fsconfig() failed: ...`).
struct AttachError {
    error: io::Error,
    kernel_message: Option<String>,
}

impl AttachError {
    fn errno(&self) -> Option<i32> {
        self.error.raw_os_error()
    }
}

impl From<io::Error> for AttachError {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            kernel_message: None,
        }
    }
}

/// The `-t`/fstab type list, else the probed type, else every non-`nodev`
/// kernel filesystem. A `-t` list with a leading `no` is negated (`-t nonfs,smbfs`).
fn filesystem_types(
    explicit: Option<&str>,
    pattern: bool,
    source: &OsStr,
    target: &Path,
) -> Result<Vec<String>, Failure> {
    let explicit = explicit.filter(|types| !types.is_empty() && *types != "auto");
    let negated = pattern && explicit.is_some_and(|types| types.starts_with("no"));
    if let Some(types) = explicit
        && !negated
    {
        return Ok(types
            .split(',')
            .filter(|fs_type| !fs_type.is_empty() && *fs_type != "auto")
            .map(str::to_owned)
            .collect());
    }
    let allowed = |fs_type: &str| explicit.is_none_or(|types| matches_types(types, fs_type));
    // Like util-linux, a missing source is not checked up front: the kernel's
    // answer for each candidate type decides the error.
    let path = Path::new(source);
    if let Ok(Some(signature)) = probe(path)
        && let Some(fs_type) = signature.fs_type
        && allowed(fs_type)
    {
        return Ok(vec![fs_type.to_owned()]);
    }
    let filesystems = fs::read_to_string(FILESYSTEMS)
        .map_err(|error| failed(format!("{FILESYSTEMS}: {}", strerror(&error))))?;
    let types: Vec<String> = filesystems
        .lines()
        .filter(|line| !line.starts_with("nodev"))
        .map(|line| line.trim().to_owned())
        .filter(|fs_type| !fs_type.is_empty() && allowed(fs_type))
        .collect();
    if types.is_empty() {
        return Err(failed(format!(
            "{}: failed to determine filesystem type of {}.",
            target.display(),
            source.display()
        )));
    }
    Ok(types)
}

fn is_block_device(source: &OsStr) -> bool {
    fs::metadata(source).is_ok_and(|metadata| metadata.file_type().is_block_device())
}

/// Attaches a loop device when the options ask for one or, like util-linux,
/// when a regular file is mounted as a block filesystem.
fn attach_loop(
    file: &OsStr,
    plan: &Plan,
    options: &MountOptions,
) -> Result<Option<loop_device::Attached>, Failure> {
    let target = plan.target.display();
    if options.encryption {
        let mut list = options.all.join(",");
        if !options
            .all
            .iter()
            .any(|option| matches!(option.as_str(), "ro" | "rw"))
        {
            list.insert_str(0, "rw,");
        }
        return Err(usage(format!(
            "{target}: failed to parse mount options '{list}': Inappropriate ioctl for device."
        )));
    }
    let request = match &options.loop_request {
        Some(request) => request.clone(),
        None if needs_auto_loop(file, plan) => LoopRequest::default(),
        None => return Ok(None),
    };
    let read_only = options.set & libc::MS_RDONLY != 0;
    // util-linux names the backing file by its canonical path when it exists.
    let named = fs::canonicalize(file).map_or_else(|_| file.to_owned(), PathBuf::into_os_string);
    match loop_device::attach(Path::new(file), &request, read_only) {
        Ok(attached) => Ok(Some(attached)),
        Err(LoopError::Overlap) => Err(failed(format!(
            "{target}: overlapping loop device exists for {}.",
            named.display()
        ))),
        Err(LoopError::Setup(_)) => Err(failed(format!(
            "{target}: failed to setup loop device for {}.",
            named.display()
        ))),
    }
}

/// A regular file needs a loop device unless every requested type is a
/// `nodev` filesystem (tmpfs, proc, ...) that ignores its source.
fn needs_auto_loop(file: &OsStr, plan: &Plan) -> bool {
    if !fs::metadata(file).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let Some(types) = plan
        .fs_type
        .as_deref()
        .filter(|types| !types.is_empty() && *types != "auto")
    else {
        return true;
    };
    if plan.type_pattern && types.starts_with("no") {
        return true;
    }
    let filesystems = fs::read_to_string(FILESYSTEMS).unwrap_or_default();
    let virtual_types: Vec<&str> = filesystems
        .lines()
        .filter_map(|line| line.strip_prefix("nodev"))
        .map(str::trim)
        .collect();
    types
        .split(',')
        .any(|fs_type| !virtual_types.contains(&fs_type))
}

/// util-linux `strtosize`: an integer with an optional `K`/`M`/`G`/`T`/`P`/`E`
/// suffix, binary as is or with `iB`, decimal with `B` (`4KiB`, `4K`, `4KB`).
fn parse_size(value: &str) -> Result<u64, Failure> {
    let invalid = || usage(format!("invalid size '{value}'"));
    let digits = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let number: u64 = value[..digits].parse().map_err(|_| invalid())?;
    let suffix = &value[digits..];
    let (unit, base) = match suffix.len() {
        0 => return Ok(number),
        1 => (suffix, 1024u64),
        2 if suffix.ends_with('B') => (&suffix[..1], 1000),
        3 if suffix.ends_with("iB") => (&suffix[..1], 1024),
        _ => return Err(invalid()),
    };
    let exponent = match unit.to_ascii_uppercase().as_str() {
        "K" => 1,
        "M" => 2,
        "G" => 3,
        "T" => 4,
        "P" => 5,
        "E" => 6,
        _ => return Err(invalid()),
    };
    number.checked_mul(base.pow(exponent)).ok_or_else(invalid)
}

/// Resolves `LABEL=`, `UUID=`, `PARTLABEL=`, `PARTUUID=` through udev links and
/// canonicalizes existing paths; other sources (`tmpfs`, `host:/export`) pass through.
fn resolve_source(spec: &OsStr, target: &Path, args: &Args) -> Result<OsString, Failure> {
    let bytes = spec.as_bytes();
    for (tag, directory) in [
        ("LABEL=", "/dev/disk/by-label"),
        ("UUID=", "/dev/disk/by-uuid"),
        ("PARTLABEL=", "/dev/disk/by-partlabel"),
        ("PARTUUID=", "/dev/disk/by-partuuid"),
    ] {
        if let Some(value) = bytes.strip_prefix(tag.as_bytes()) {
            let value = value
                .strip_prefix(b"\"")
                .and_then(|value| value.strip_suffix(b"\""))
                .unwrap_or(value);
            let link = Path::new(directory).join(OsStr::from_bytes(value));
            return fs::canonicalize(&link)
                .map(PathBuf::into_os_string)
                .map_err(|_| {
                    usage(format!(
                        "{}: can't find {}.",
                        target.display(),
                        spec.display()
                    ))
                    .missing_source()
                });
        }
    }
    // Like util-linux, device and image paths are canonicalized; other
    // sources (`tmpfs`, `proc`) stay as given even if a same-named file exists.
    if !args.no_canonicalize
        && (bytes.starts_with(b"/")
            || fs::metadata(spec)
                .is_ok_and(|metadata| metadata.is_file() || metadata.file_type().is_block_device()))
        && let Ok(path) = fs::canonicalize(spec)
    {
        return Ok(path.into_os_string());
    }
    Ok(spec.to_owned())
}

fn same_path(left: &Path, right: &Path) -> bool {
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    left == right || canonical(left) == canonical(right)
}

fn same_source(entry: &OsStr, spec: &OsStr) -> bool {
    entry == spec
        || (entry.as_bytes().starts_with(b"/")
            && spec.as_bytes().starts_with(b"/")
            && same_path(Path::new(entry), Path::new(spec)))
}

#[derive(Clone, Copy)]
enum Kind {
    Mount,
    Remount,
    Bind,
    Move,
    Propagation,
}

/// One `mount(2)` request with the context util-linux uses for its diagnostics.
#[derive(Clone, Copy)]
struct Call<'a> {
    target: &'a Path,
    source: Option<&'a OsStr>,
    fs_type: Option<&'a str>,
    kind: Kind,
    fake: bool,
    /// Snapshot for diagnostics during `--all`; `None` reads the live table.
    mount_table: Option<&'a [MountEntry]>,
}

impl Call<'_> {
    fn mount(&self, flags: c_ulong, data: Option<&str>) -> Result<(), Failure> {
        self.syscall(flags, data)
            .map_err(|error| self.failure(error))
    }

    /// Creates and attaches a new filesystem the way util-linux does
    /// (`fsopen`, `fsconfig`, `fsmount`, `move_mount`), so option errors carry
    /// the kernel's fs-context message. Kernels without the API use `mount(2)`.
    fn create(&self, options: &MountOptions, flags: c_ulong) -> Result<(), AttachError> {
        if self.fake {
            return Ok(());
        }
        let fs_type = CString::new(self.fs_type.unwrap_or_default()).map_err(io::Error::from)?;
        // SAFETY: fs_type is a NUL-terminated string that outlives the call.
        let raw =
            unsafe { libc::syscall(libc::SYS_fsopen, fs_type.as_ptr(), libc::FSOPEN_CLOEXEC) };
        if raw < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOSYS) {
                return Ok(self.syscall(flags, options.data().as_deref())?);
            }
            return Err(error.into());
        }
        // SAFETY: fsopen returned a new descriptor owned by nobody else.
        let context = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
        let config = |command: libc::c_uint, key: Option<&str>, value: Option<&[u8]>| {
            let key = key.map(CString::new).transpose().map_err(io::Error::from)?;
            let value = value
                .map(CString::new)
                .transpose()
                .map_err(io::Error::from)?;
            // SAFETY: the descriptor is live; key and value are null or
            // NUL-terminated strings that outlive the call.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_fsconfig,
                    context.as_raw_fd(),
                    command,
                    key.as_ref().map_or(std::ptr::null(), |key| key.as_ptr()),
                    value
                        .as_ref()
                        .map_or(std::ptr::null(), |value| value.as_ptr()),
                    0,
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                return Err(AttachError {
                    error,
                    kernel_message: fs_context_error(&context)
                        .map(|message| format!("fsconfig() failed: {message}")),
                });
            }
            Ok(())
        };
        if let Some(source) = self.source {
            config(
                libc::FSCONFIG_SET_STRING,
                Some("source"),
                Some(source.as_bytes()),
            )?;
        }
        for option in &options.data {
            match option.split_once('=') {
                Some((key, value)) => {
                    let value = value
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                        .unwrap_or(value);
                    config(libc::FSCONFIG_SET_STRING, Some(key), Some(value.as_bytes()))?;
                }
                None => config(libc::FSCONFIG_SET_FLAG, Some(option), None)?,
            }
        }
        for (flag, name) in [
            (libc::MS_RDONLY, "ro"),
            (libc::MS_SYNCHRONOUS, "sync"),
            (libc::MS_DIRSYNC, "dirsync"),
            (libc::MS_LAZYTIME, "lazytime"),
            (libc::MS_MANDLOCK, "mand"),
        ] {
            if flags & flag != 0 {
                config(libc::FSCONFIG_SET_FLAG, Some(name), None)?;
            }
        }
        config(libc::FSCONFIG_CMD_CREATE, None, None)?;

        let mut attributes = 0;
        for (flag, attribute) in [
            (libc::MS_RDONLY, libc::MOUNT_ATTR_RDONLY),
            (libc::MS_NOSUID, libc::MOUNT_ATTR_NOSUID),
            (libc::MS_NODEV, libc::MOUNT_ATTR_NODEV),
            (libc::MS_NOEXEC, libc::MOUNT_ATTR_NOEXEC),
            (libc::MS_NOATIME, libc::MOUNT_ATTR_NOATIME),
            (libc::MS_STRICTATIME, libc::MOUNT_ATTR_STRICTATIME),
            (libc::MS_NODIRATIME, libc::MOUNT_ATTR_NODIRATIME),
            (libc::MS_NOSYMFOLLOW, libc::MOUNT_ATTR_NOSYMFOLLOW),
        ] {
            if flags & flag != 0 {
                attributes |= attribute;
            }
        }
        // SAFETY: the context descriptor is live; fsmount takes plain integers.
        let raw = unsafe {
            libc::syscall(
                libc::SYS_fsmount,
                context.as_raw_fd(),
                libc::FSMOUNT_CLOEXEC,
                attributes as libc::c_uint,
            )
        };
        if raw < 0 {
            let error = io::Error::last_os_error();
            return Err(AttachError {
                error,
                kernel_message: fs_context_error(&context)
                    .map(|message| format!("fsmount() failed: {message}")),
            });
        }
        // SAFETY: fsmount returned a new descriptor owned by nobody else.
        let mounted = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
        let target = CString::new(self.target.as_os_str().as_bytes()).map_err(io::Error::from)?;
        // SAFETY: the mount descriptor is live and both paths are NUL-terminated.
        let result = unsafe {
            libc::syscall(
                libc::SYS_move_mount,
                mounted.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::MOVE_MOUNT_F_EMPTY_PATH,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn attach_failure(&self, error: AttachError) -> Failure {
        match error.kernel_message {
            Some(message) => mount_failed(format!("{}: {message}.", self.target.display())),
            None => self.failure(error.error),
        }
    }

    fn syscall(&self, flags: c_ulong, data: Option<&str>) -> io::Result<()> {
        if self.fake {
            return Ok(());
        }
        let source = self
            .source
            .map(|source| CString::new(source.as_bytes()))
            .transpose()?;
        let target = CString::new(self.target.as_os_str().as_bytes())?;
        let fs_type = self.fs_type.map(CString::new).transpose()?;
        let data = data.map(CString::new).transpose()?;
        // SAFETY: each pointer is null or a NUL-terminated string owned by
        // this frame for the duration of the call; text mount data is a C string.
        let result = unsafe {
            libc::mount(
                source
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                target.as_ptr(),
                fs_type
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                flags,
                data.as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr().cast()),
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// util-linux wording for common `mount(2)` errors.
    fn failure(&self, error: io::Error) -> Failure {
        let target = self.target.display();
        let source = self.source.unwrap_or(OsStr::new("none")).display();
        let generic = || {
            format!(
                "wrong fs type, bad option, bad superblock on {source}, missing codepage or helper program, or other error."
            )
        };
        let message = match (error.raw_os_error(), self.kind) {
            // SAFETY: geteuid has no preconditions.
            (Some(libc::EPERM), _) if unsafe { libc::geteuid() } != 0 => {
                "must be superuser to use mount.".to_owned()
            }
            (Some(libc::EPERM | libc::EACCES), _) => "permission denied.".to_owned(),
            (Some(libc::EBUSY), Kind::Mount | Kind::Bind | Kind::Move) => {
                let mounted_on = self.source.and_then(|source| {
                    let find = |table: &[MountEntry]| {
                        table
                            .iter()
                            .find(|entry| entry.source == source)
                            .map(|entry| entry.target.clone())
                    };
                    match self.mount_table {
                        Some(table) => find(table),
                        None => find(&mount_table().ok()?),
                    }
                });
                match mounted_on {
                    Some(mounted_on) => {
                        format!("{source} already mounted on {}.", mounted_on.display())
                    }
                    None => format!("{source} already mounted or mount point busy."),
                }
            }
            (Some(libc::EBUSY), _) => "mount point is busy.".to_owned(),
            (Some(libc::ENOENT), _) if !self.target.exists() => {
                "mount point does not exist.".to_owned()
            }
            (Some(libc::ENOENT), _) => {
                return mount_failed(format!("{target}: special device {source} does not exist."))
                    .missing_source();
            }
            // util-linux attaches through move_mount(2), which reports a
            // directory/file mismatch as EINVAL.
            (Some(libc::ENOTDIR), Kind::Mount | Kind::Bind) => generic(),
            (Some(libc::ENOTDIR), _) => "mount point is not a directory.".to_owned(),
            (Some(libc::EINVAL), Kind::Remount) => {
                "mount point not mounted or bad option.".to_owned()
            }
            (Some(libc::EINVAL), Kind::Propagation) => "not mount point or bad option.".to_owned(),
            (Some(libc::EINVAL), Kind::Move) if self.under_shared_mount() => {
                "bad option; moving a mount residing under a shared mount is unsupported."
                    .to_owned()
            }
            (Some(libc::EINVAL), _) => generic(),
            (Some(libc::ENODEV), _) => {
                format!(
                    "unknown filesystem type '{}'.",
                    self.fs_type.unwrap_or("none")
                )
            }
            (Some(libc::ENOTBLK), _) => format!("{source} is not a block device."),
            (Some(libc::ENXIO), _) => format!("{source} is not a valid block device."),
            (Some(libc::EROFS), _) => {
                format!("cannot mount {source} read-write, is write-protected.")
            }
            _ => format!("{}.", strerror(&error)),
        };
        mount_failed(format!("{target}: {message}"))
    }

    /// Whether the mount holding the target's directory is shared.
    fn under_shared_mount(&self) -> bool {
        let Some(parent) = self.target.parent() else {
            return false;
        };
        mount_table().is_ok_and(|table| {
            table
                .iter()
                .filter(|entry| parent.starts_with(&entry.target))
                .max_by_key(|entry| entry.target.components().count())
                .is_some_and(|entry| entry.shared)
        })
    }
}

/// Last error the kernel logged on an fs-context descriptor (`e tmpfs: ...`).
fn fs_context_error(context: &OwnedFd) -> Option<String> {
    let mut buffer = [0u8; 8192];
    let mut last = None;
    loop {
        // SAFETY: the buffer is writable for its full length and the descriptor is live.
        let read = unsafe {
            libc::read(
                context.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        // Errors (ENODATA once drained) and EOF end the log.
        let Ok(read @ 1..) = usize::try_from(read) else {
            break;
        };
        if let Some(message) = buffer[..read].strip_prefix(b"e ") {
            last = Some(String::from_utf8_lossy(message).trim_end().to_owned());
        }
    }
    last
}

fn mount_all(args: &Args) -> Result<i32, Failure> {
    let entries = read_fstab(FSTAB)?;
    let mut table = mount_table()?;
    let initial_table = mount_table()?;
    let (mut attempted, mut failures) = (0, 0);
    for entry in entries {
        let report = |status: &str| {
            if args.verbose {
                println!("{:<25}: {status}", entry.target.display());
            }
        };
        let options = MountOptions::parse([entry.options.as_str()])?;
        if options.noauto
            || entry.fs_type == "swap"
            || matches!(entry.target.as_os_str().as_bytes(), b"none" | b"swap")
            || args
                .types
                .as_deref()
                .is_some_and(|types| !matches_types(types, &entry.fs_type))
            || args
                .test_opts
                .as_deref()
                .is_some_and(|pattern| !matches_options(&entry.options, pattern))
        {
            report("ignored");
            continue;
        }
        if table
            .iter()
            .any(|mounted| same_path(&mounted.target, &entry.target))
        {
            report("already mounted");
            continue;
        }
        attempted += 1;
        let mut plan_options = vec![entry.options.clone()];
        plan_options.extend(args.options.iter().cloned());
        let plan = Plan {
            source: Some(entry.source.clone()),
            target: entry.target.clone().into_os_string(),
            fs_type: Some(entry.fs_type.clone()),
            type_pattern: false,
            options: plan_options,
        };
        // `-a -v` reports through the per-entry status table only.
        match execute(&plan, args, false, Some(&initial_table)) {
            Ok(()) => {
                report("successfully mounted");
                table = mount_table()?;
            }
            Err(failure) if options.nofail && failure.missing_source => {
                // `nofail` only silences a device that does not exist.
                report("ignored");
            }
            Err(failure) => {
                failures += 1;
                if !failure.message.is_empty() {
                    eprintln!("mount: {}", failure.message);
                }
            }
        }
    }
    Ok(match failures {
        0 => 0,
        failures if failures == attempted => EXIT_FAILURE,
        _ => EXIT_SOME_FAILED,
    })
}

/// util-linux `-t` matching: a leading `no` negates the whole list (`-t nonfs,smbfs`).
fn matches_types(types: &str, fs_type: &str) -> bool {
    let negated = types.starts_with("no");
    let listed = types.split(',').any(|candidate| {
        let candidate = if negated {
            candidate.strip_prefix("no").unwrap_or(candidate)
        } else {
            candidate
        };
        candidate == fs_type
    });
    listed != negated
}

/// util-linux `-O` matching: every pattern item must be present; a `no`
/// prefix requires absence, and `+` forces a literal `no...` option.
fn matches_options(options: &str, pattern: &str) -> bool {
    let present = |wanted: &str| {
        split_options(options).any(|option| {
            if wanted.contains('=') {
                option == wanted
            } else {
                option.split_once('=').map_or(option, |(name, _)| name) == wanted
            }
        })
    };
    split_options(pattern).all(|item| {
        if let Some(literal) = item.strip_prefix('+') {
            present(literal)
        } else if let Some(absent) = item.strip_prefix("no") {
            !present(absent)
        } else {
            present(item)
        }
    })
}

fn list(args: &Args) -> Result<(), Failure> {
    let mut stdout = io::stdout().lock();
    for entry in mount_table()? {
        if args
            .types
            .as_deref()
            .is_some_and(|types| !matches_types(types, &entry.fs_type))
        {
            continue;
        }
        let mut line = Vec::new();
        line.extend_from_slice(entry.source.as_bytes());
        line.extend_from_slice(b" on ");
        // Like util-linux, keep one mount per line: control bytes in the target become `?`.
        line.extend(entry.target.as_os_str().as_bytes().iter().map(|&byte| {
            if byte < 0x20 || byte == 0x7f {
                b'?'
            } else {
                byte
            }
        }));
        // The superblock's own ro/rw duplicates the mount-point flag; util-linux omits it.
        let mut options = entry.mount_options.clone();
        for option in entry
            .super_options
            .split(',')
            .filter(|option| !option.is_empty() && !matches!(*option, "ro" | "rw"))
        {
            options.push(',');
            options.push_str(option);
        }
        line.extend_from_slice(format!(" type {} ({options})", entry.fs_type).as_bytes());
        if args.show_labels
            && let Ok(Some(signature)) = probe(Path::new(&entry.source))
            && let Some(label) = signature.label
        {
            line.extend_from_slice(format!(" [{label}]").as_bytes());
        }
        line.push(b'\n');
        if let Err(error) = stdout.write_all(&line) {
            if error.kind() == io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(failed(format!("write error: {}", strerror(&error))));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MountDevice {
    major: u32,
    minor: u32,
}

struct MountEntry {
    id: u64,
    parent_id: u64,
    device: MountDevice,
    source: OsString,
    target: PathBuf,
    fs_type: String,
    mount_options: String,
    super_options: String,
    /// Has a `shared:N` peer group.
    shared: bool,
}

impl MountEntry {
    /// Per-mount-point flags; the kernel omits strictatime, so no atime option means strictatime.
    fn mount_flags(&self) -> c_ulong {
        let mut flags = self
            .mount_options
            .split(',')
            .map(|option| match option {
                "ro" => libc::MS_RDONLY,
                "nosuid" => libc::MS_NOSUID,
                "nodev" => libc::MS_NODEV,
                "noexec" => libc::MS_NOEXEC,
                "noatime" => libc::MS_NOATIME,
                "nodiratime" => libc::MS_NODIRATIME,
                "relatime" => libc::MS_RELATIME,
                "nosymfollow" => libc::MS_NOSYMFOLLOW,
                _ => 0,
            })
            .fold(0, |flags, flag| flags | flag);
        if flags & ATIME_FLAGS == 0 {
            flags |= libc::MS_STRICTATIME;
        }
        flags
    }

    /// Superblock flags that a plain remount would otherwise reset.
    fn super_flags(&self) -> c_ulong {
        self.super_options
            .split(',')
            .map(|option| match option {
                "ro" => libc::MS_RDONLY,
                "sync" => libc::MS_SYNCHRONOUS,
                "dirsync" => libc::MS_DIRSYNC,
                "mand" => libc::MS_MANDLOCK,
                "lazytime" => libc::MS_LAZYTIME,
                _ => 0,
            })
            .fold(0, |flags, flag| flags | flag)
    }
}

fn mount_table() -> Result<Vec<MountEntry>, Failure> {
    let content = fs::read(MOUNTINFO)
        .map_err(|error| failed(format!("{MOUNTINFO}: {}", strerror(&error))))?;
    Ok(content
        .split(|&byte| byte == b'\n')
        .filter_map(parse_mountinfo_line)
        .collect())
}

fn parse_mountinfo_line(line: &[u8]) -> Option<MountEntry> {
    let separator = line.windows(3).position(|window| window == b" - ")?;
    let left: Vec<&[u8]> = line[..separator].split(|&byte| byte == b' ').collect();
    let right: Vec<&[u8]> = line[separator + 3..].split(|&byte| byte == b' ').collect();
    if left.len() < 6 || right.len() < 3 {
        return None;
    }
    Some(MountEntry {
        id: std::str::from_utf8(left[0]).ok()?.parse().ok()?,
        parent_id: std::str::from_utf8(left[1]).ok()?.parse().ok()?,
        device: {
            let (major, minor) = std::str::from_utf8(left[2]).ok()?.split_once(':')?;
            MountDevice {
                major: major.parse().ok()?,
                minor: minor.parse().ok()?,
            }
        },
        source: OsString::from_vec(unescape(right[1])),
        target: PathBuf::from(OsString::from_vec(unescape(left[4]))),
        fs_type: String::from_utf8_lossy(&unescape(right[0])).into_owned(),
        mount_options: String::from_utf8_lossy(left[5]).into_owned(),
        super_options: String::from_utf8_lossy(right[2]).into_owned(),
        shared: left[6..].iter().any(|field| field.starts_with(b"shared:")),
    })
}

struct FstabEntry {
    source: OsString,
    target: PathBuf,
    fs_type: String,
    options: String,
}

fn read_fstab(path: &str) -> Result<Vec<FstabEntry>, Failure> {
    let content = match fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(failed(format!("{path}: {}", strerror(&error)))),
    };
    let mut entries = Vec::new();
    for (index, line) in content.split(|&byte| byte == b'\n').enumerate() {
        match parse_fstab_line(line) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => {}
            Err(()) => eprintln!(
                "mount: {path}: parse error at line {} -- ignored",
                index + 1
            ),
        }
    }
    Ok(entries)
}

/// `Ok(None)` for blank and comment lines; `Err` for lines without a type.
fn parse_fstab_line(line: &[u8]) -> Result<Option<FstabEntry>, ()> {
    let mut fields = line
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let Some(source) = fields.next().filter(|field| !field.starts_with(b"#")) else {
        return Ok(None);
    };
    let (Some(target), Some(fs_type)) = (fields.next(), fields.next()) else {
        return Err(());
    };
    let text = |field: &[u8]| String::from_utf8_lossy(&unescape(field)).into_owned();
    Ok(Some(FstabEntry {
        source: OsString::from_vec(unescape(source)),
        target: PathBuf::from(OsString::from_vec(unescape(target))),
        fs_type: text(fs_type),
        options: fields.next().map_or_else(|| "defaults".to_owned(), text),
    }))
}

/// Decodes the `\ooo` octal escapes used by mountinfo and fstab.
fn unescape(field: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\'
            && let Some(digits) = field.get(index + 1..index + 4)
            && digits.iter().all(|digit| (b'0'..=b'7').contains(digit))
        {
            let value = digits
                .iter()
                .fold(0u16, |value, digit| value * 8 + u16::from(digit - b'0'));
            if let Ok(byte) = u8::try_from(value) {
                decoded.push(byte);
                index += 4;
                continue;
            }
        }
        decoded.push(field[index]);
        index += 1;
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(list: &str) -> MountOptions {
        MountOptions::parse([list]).expect("valid options")
    }

    #[test]
    fn bind_remount_changes_only_requested_flags() {
        let current = parse_mountinfo_line(
            b"36 25 0:32 / /srv/data rw,nosuid,nodev,noexec,relatime shared:5 - tmpfs tmpfs rw,size=1024k",
        )
        .expect("valid mountinfo line");
        let options = options("remount,bind,ro");
        assert_eq!(options.operation, libc::MS_REMOUNT | libc::MS_BIND);
        assert_eq!(
            options.merged(current.mount_flags()),
            libc::MS_RDONLY
                | libc::MS_NOSUID
                | libc::MS_NODEV
                | libc::MS_NOEXEC
                | libc::MS_RELATIME
        );
        let ro = MountEntry {
            mount_options: "ro,nosuid".into(),
            ..current
        };
        assert_eq!(
            MountOptions::parse(["remount,bind", "rw"])
                .unwrap()
                .merged(ro.mount_flags()),
            libc::MS_NOSUID | libc::MS_STRICTATIME
        );
    }

    #[test]
    fn later_options_override_earlier_ones() {
        let options = MountOptions::parse(["defaults,noatime,ro,nofail", "rw,relatime"]).unwrap();
        assert_eq!(options.set, libc::MS_RELATIME);
        assert_eq!(
            options.merged(libc::MS_RDONLY | libc::MS_NOATIME),
            libc::MS_RELATIME
        );
        assert!(options.nofail);
    }

    #[test]
    fn filesystem_options_become_data_and_userspace_options_are_dropped() {
        let options = options(
            r#"rw,size=10m,mode="0750",context="system_u:object_r:tmp_t:s0:c127,c456",x-systemd.automount,_netdev,user=alice,rshared"#,
        );
        assert_eq!(
            options.data().as_deref(),
            Some(r#"size=10m,mode=0750,context="system_u:object_r:tmp_t:s0:c127,c456",user=alice"#)
        );
        assert_eq!(options.propagation, [libc::MS_SHARED | libc::MS_REC]);
        assert!(!options.propagation_only());
        assert!(
            MountOptions::parse(["rprivate"])
                .unwrap()
                .propagation_only()
        );
    }

    #[test]
    fn user_implies_restrictions_that_later_options_override() {
        assert_eq!(
            options("user").set,
            libc::MS_NOEXEC | libc::MS_NOSUID | libc::MS_NODEV
        );
        assert_eq!(options("users,exec").set, libc::MS_NOSUID | libc::MS_NODEV);
    }

    #[test]
    fn loop_options_build_a_request_and_stay_out_of_fs_data() {
        let options = options("ro,loop=/dev/loop7,offset=1MiB,sizelimit=4KB,size=1m");
        assert_eq!(
            options.loop_request,
            Some(LoopRequest {
                device: Some(PathBuf::from("/dev/loop7")),
                offset: 1 << 20,
                size_limit: 4000,
            })
        );
        assert_eq!(options.data, ["size=1m"]);
        assert_eq!(
            MountOptions::parse(["offset=4096"]).unwrap().loop_request,
            Some(LoopRequest {
                offset: 4096,
                ..LoopRequest::default()
            })
        );
        assert!(MountOptions::parse(["offset=4X"]).is_err());
    }

    #[test]
    fn short_options_cluster_and_take_attached_values() {
        let Parsed::Run(args) = Args::parse(
            [
                "mount",
                "-vnt",
                "ext4",
                "-oro",
                "--rb",
                "--make-rslave",
                "/a",
                "/b",
            ]
            .map(OsString::from)
            .to_vec(),
        )
        .unwrap() else {
            panic!("expected a mount request");
        };
        assert!(args.verbose && args.operation);
        assert_eq!(args.types.as_deref(), Some("ext4"));
        assert_eq!(args.options, ["ro", "rbind", "rslave"]);
        assert_eq!(args.operands, [OsString::from("/a"), OsString::from("/b")]);
    }

    #[test]
    fn long_option_abbreviations_follow_getopt() {
        let long_option =
            |name| getopt::long_option(OPTIONS, Applet::Mount, name).map(|option| option.name);
        assert_eq!(long_option("ty").unwrap(), "types");
        assert_eq!(long_option("rw").unwrap(), "rw");
        let ambiguous = long_option("t").unwrap_err().message;
        assert!(ambiguous.starts_with(
            "option '--t' is ambiguous; possibilities: '--test-opts' '--types' '--target' '--target-prefix'\n"
        ));
        assert!(long_option("frobnicate").is_err());
    }

    #[test]
    fn mountinfo_and_fstab_escapes_are_decoded() {
        let entry = parse_mountinfo_line(
            b"36 25 8:1 / /mnt/a\\040b rw,relatime - ext4 /dev/sda1 rw,errors=remount-ro",
        )
        .unwrap();
        assert_eq!(entry.target, Path::new("/mnt/a b"));
        assert_eq!(entry.super_options, "rw,errors=remount-ro");
        let fstab = parse_fstab_line(b"  LABEL=data\t/srv/my\\040data  ext4 noatime 0 2")
            .unwrap()
            .unwrap();
        assert_eq!(fstab.target, Path::new("/srv/my data"));
        assert_eq!(fstab.options, "noatime");
        assert!(matches!(
            parse_fstab_line(b"# /dev/sdb1 /mnt ext4"),
            Ok(None)
        ));
        assert!(parse_fstab_line(b"tmpfs /mnt").is_err());
    }

    #[test]
    fn option_patterns_require_presence_or_absence() {
        assert!(matches_options("defaults,_netdev,size=1m", "_netdev,size"));
        assert!(!matches_options("defaults,_netdev", "no_netdev"));
        assert!(matches_options("defaults", "no_netdev"));
        assert!(matches_options("noatime", "+noatime"));
    }

    #[test]
    fn type_lists_support_negation() {
        assert!(matches_types("ext4,xfs", "xfs"));
        assert!(!matches_types("ext4,xfs", "nfs"));
        assert!(!matches_types("nonfs,smbfs", "smbfs"));
        assert!(matches_types("nonfs,smbfs", "ext4"));
    }
}
