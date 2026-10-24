use std::collections::HashMap;
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::getopt::{Applet, Getopt, Item, OptionSpec};
use super::{
    EXIT_FAILURE, Failure, HELPER_DIRS, MountDevice, MountEntry, failed, matches_options,
    matches_types, mount_table, split_options, strerror,
};

const USAGE: &str = "\
Usage: umount [-hV]
       umount -a [options]
       umount [options] <source> | <directory>...

Unmount filesystems.

Options:
 -a, --all               unmount all filesystems except the standard virtual types
 -A, --all-targets       unmount all mountpoints for the given filesystem
 -c, --no-canonicalize   don't canonicalize paths
 -d, --detach-loop       accepted for compatibility (loop devices detach automatically)
     --fake              dry run; skip helpers and the umount(2) syscall
 -f, --force             force unmount (MNT_FORCE)
 -i, --internal-only     don't call the umount.<type> helpers
 -l, --lazy              detach now, clean up references later (MNT_DETACH)
 -n, --no-mtab           accepted for compatibility (no mtab is written)
 -N, --namespace <ns>    perform umount in another mount namespace (PID or path)
 -O, --test-opts <list>  limit the set of filesystems (use with -a)
 -q, --quiet             suppress 'not mounted' error messages
 -R, --recursive         unmount a target with all its children and overmounts
 -r, --read-only         in case unmounting fails, try to remount read-only
 -t, --types <list>      limit filesystem types (use with -a)
 -v, --verbose           report unmounted targets and their sources on stderr
     --source <source>   select a mounted source explicitly
     --target <target>   select a mountpoint explicitly
 -h, --help              display this help
 -V, --version           display version information";

const OPTIONS: &[OptionSpec] = &[
    OptionSpec::flag("all", b'a'),
    OptionSpec::flag("all-targets", b'A'),
    OptionSpec::flag("detach-loop", b'd'),
    OptionSpec::flag("fake", 0),
    OptionSpec::flag("force", b'f'),
    OptionSpec::flag("help", b'h'),
    OptionSpec::flag("internal-only", b'i'),
    OptionSpec::flag("lazy", b'l'),
    OptionSpec::flag("no-canonicalize", b'c'),
    OptionSpec::flag("no-mtab", b'n'),
    OptionSpec::flag("quiet", b'q'),
    OptionSpec::flag("read-only", b'r'),
    OptionSpec::flag("recursive", b'R'),
    OptionSpec::value("test-opts", b'O'),
    OptionSpec::value("types", b't'),
    OptionSpec::flag("verbose", b'v'),
    OptionSpec::flag("version", b'V'),
    OptionSpec::value("namespace", b'N'),
    OptionSpec::value("source", 0),
    OptionSpec::value("target", 0),
];

const SKIP_TYPES: [&str; 7] = [
    "proc",
    "devfs",
    "devpts",
    "sysfs",
    "rpc_pipefs",
    "nfsd",
    "selinuxfs",
];

#[derive(Clone, Copy, PartialEq)]
enum OperandKind {
    Either,
    Source,
    Target,
}

struct Operand {
    spec: OsString,
    kind: OperandKind,
}

#[derive(Default)]
struct Args {
    all: bool,
    all_targets: bool,
    recursive: bool,
    lazy: bool,
    force: bool,
    read_only: bool,
    verbose: bool,
    fake: bool,
    quiet: bool,
    no_canonicalize: bool,
    internal_only: bool,
    no_mtab: bool,
    types: Option<String>,
    test_opts: Option<String>,
    namespace: Option<OsString>,
    operands: Vec<Operand>,
}

enum Parsed {
    Run(Args),
    Help,
    Version,
}

impl Args {
    fn parse(args: Vec<OsString>) -> Result<Parsed, Failure> {
        let mut parsed = Self::default();
        let mut options = Getopt::new(args, Applet::Umount, OPTIONS);
        while let Some(item) = options.next()? {
            let (name, value) = match item {
                Item::Operand(spec) => {
                    parsed.operands.push(Operand {
                        spec,
                        kind: OperandKind::Either,
                    });
                    continue;
                }
                Item::Option(name, value, _) => (name, value),
            };
            let text = |value: Option<OsString>| {
                value.unwrap_or_default().into_string().map_err(|value| {
                    Applet::Umount
                        .bad_usage(format!("argument is not valid UTF-8: {}", value.display()))
                })
            };
            match name {
                "all" => parsed.all = true,
                "all-targets" => parsed.all_targets = true,
                "recursive" => parsed.recursive = true,
                "lazy" => parsed.lazy = true,
                "force" => parsed.force = true,
                "read-only" => parsed.read_only = true,
                "verbose" => parsed.verbose = true,
                "fake" => parsed.fake = true,
                "quiet" => parsed.quiet = true,
                "no-canonicalize" => parsed.no_canonicalize = true,
                "internal-only" => parsed.internal_only = true,
                "no-mtab" => parsed.no_mtab = true,
                "detach-loop" => {}
                "types" => parsed.types = Some(text(value)?),
                "test-opts" => parsed.test_opts = Some(text(value)?),
                "namespace" => parsed.namespace = value,
                "source" | "target" => parsed.operands.push(Operand {
                    spec: value.unwrap_or_default(),
                    kind: if name == "source" {
                        OperandKind::Source
                    } else {
                        OperandKind::Target
                    },
                }),
                "help" => return Ok(Parsed::Help),
                "version" => return Ok(Parsed::Version),
                _ => unreachable!("options are defined by the umount table"),
            }
        }
        if parsed.all && (parsed.all_targets || parsed.recursive)
            || parsed.recursive
                && (parsed.read_only || parsed.types.is_some() || parsed.test_opts.is_some())
        {
            return Err(Applet::Umount.bad_usage("mutually exclusive arguments"));
        }
        if parsed.all && !parsed.operands.is_empty() {
            return Err(Applet::Umount.bad_usage("unexpected number of arguments"));
        }
        if !parsed.all && parsed.operands.is_empty() {
            return Err(Applet::Umount.bad_usage("bad usage"));
        }
        Ok(Parsed::Run(parsed))
    }

    fn selects(&self, entry: &MountEntry) -> bool {
        let types_match = self.types.as_deref().map_or_else(
            || !SKIP_TYPES.contains(&entry.fs_type.as_str()),
            |types| matches_types(types, &entry.fs_type),
        );
        entry.target != Path::new("/")
            && types_match
            && self.test_opts.as_deref().is_none_or(|pattern| {
                split_options(pattern).all(|item| {
                    let mount = matches_options(&entry.mount_options, item);
                    let superblock = matches_options(&entry.super_options, item);
                    if item.starts_with("no") {
                        mount && superblock
                    } else {
                        mount || superblock
                    }
                })
            })
    }
}

pub(super) fn run(args: Vec<OsString>) -> i32 {
    match run_inner(args) {
        Ok(code) => code,
        Err(failure) => {
            if !failure.message.is_empty() {
                let _ = writeln!(io::stderr().lock(), "umount: {}", failure.message);
            }
            failure.code
        }
    }
}

fn run_inner(args: Vec<OsString>) -> Result<i32, Failure> {
    let args = match Args::parse(args)? {
        Parsed::Run(args) => args,
        parsed => {
            let text = match parsed {
                Parsed::Help => USAGE.to_owned(),
                Parsed::Version => format!("umount from axe {}", env!("CARGO_PKG_VERSION")),
                Parsed::Run(_) => unreachable!(),
            };
            return match writeln!(io::stdout().lock(), "{text}") {
                Ok(()) => Ok(0),
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(0),
                Err(error) => Err(failed(format!("write error: {}", strerror(&error)))),
            };
        }
    };
    let mut namespace = MountNamespace::enter(args.namespace.as_deref())?;
    let result = (|| {
        let table = mount_table()?;
        let mut output = io::BufWriter::new(io::stderr().lock());
        let code = process(&table, &args, umount_one, &mut output)?;
        output
            .flush()
            .map_err(|error| failed(format!("write error: {}", strerror(&error))))?;
        Ok(code)
    })();
    namespace.restore()?;
    result
}

struct MountNamespace {
    original: Option<OwnedFd>,
}

impl MountNamespace {
    fn enter(spec: Option<&OsStr>) -> Result<Self, Failure> {
        let Some(spec) = spec else {
            return Ok(Self { original: None });
        };
        let path = match spec.to_str().and_then(|value| value.parse::<u32>().ok()) {
            Some(pid) if pid != 0 => PathBuf::from(format!("/proc/{pid}/ns/mnt")),
            _ => PathBuf::from(spec),
        };
        let namespace_error = |error: io::Error| Failure {
            code: 2,
            message: format!(
                "failed to set target namespace to {}: {}",
                path.display(),
                strerror(&error)
            ),
            missing_source: false,
        };
        let target = fs::File::open(&path).map_err(namespace_error)?;
        let original = fs::File::open("/proc/self/ns/mnt").map_err(namespace_error)?;
        let target_stat = target.metadata().map_err(namespace_error)?;
        let original_stat = original.metadata().map_err(namespace_error)?;
        if target_stat.dev() == original_stat.dev() && target_stat.ino() == original_stat.ino() {
            return Ok(Self { original: None });
        }
        // SAFETY: target owns a live mount-namespace descriptor throughout the call.
        if unsafe { libc::setns(target.as_raw_fd(), libc::CLONE_NEWNS) } != 0 {
            return Err(namespace_error(io::Error::last_os_error()));
        }
        Ok(Self {
            original: Some(original.into()),
        })
    }

    fn restore(&mut self) -> Result<(), Failure> {
        let Some(original) = &self.original else {
            return Ok(());
        };
        // SAFETY: original is the live namespace descriptor saved before setns.
        if unsafe { libc::setns(original.as_raw_fd(), libc::CLONE_NEWNS) } != 0 {
            return Err(Failure {
                code: 2,
                message: format!(
                    "failed to switch namespace back: {}",
                    strerror(&io::Error::last_os_error())
                ),
                missing_source: false,
            });
        }
        self.original = None;
        Ok(())
    }
}

impl Drop for MountNamespace {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Only the command-line operand is resolved; mountinfo paths are never stat'ed.
struct PreparedSpec<'a> {
    operand: &'a Operand,
    resolved: Option<PathBuf>,
    missing: bool,
}

impl<'a> PreparedSpec<'a> {
    fn new(operand: &'a Operand, no_canonicalize: bool) -> Self {
        let mut prepared = Self {
            operand,
            resolved: None,
            missing: false,
        };
        if no_canonicalize {
            if Path::new(&operand.spec).is_relative() {
                prepared.resolved = std::env::current_dir()
                    .ok()
                    .map(|directory| directory.join(&operand.spec));
            }
            return prepared;
        }
        let mut path = None;
        if operand.kind != OperandKind::Target {
            for (tag, directory) in [
                ("LABEL=", "/dev/disk/by-label"),
                ("UUID=", "/dev/disk/by-uuid"),
                ("PARTLABEL=", "/dev/disk/by-partlabel"),
                ("PARTUUID=", "/dev/disk/by-partuuid"),
            ] {
                if let Some(value) = operand.spec.as_bytes().strip_prefix(tag.as_bytes()) {
                    let value = value
                        .strip_prefix(b"\"")
                        .and_then(|value| value.strip_suffix(b"\""))
                        .unwrap_or(value);
                    path = Some(Path::new(directory).join(OsStr::from_bytes(value)));
                    break;
                }
            }
        }
        match fs::canonicalize(path.as_deref().unwrap_or_else(|| Path::new(&operand.spec))) {
            Ok(path) => prepared.resolved = Some(path),
            Err(error) => prepared.missing = error.kind() == io::ErrorKind::NotFound,
        }
        prepared
    }

    fn resolve(&self, table: &[MountEntry], active: &[bool], recursive: bool) -> Option<usize> {
        let target_matches = |entry: &MountEntry| {
            entry.target == Path::new(&self.operand.spec)
                || self.resolved.as_deref() == Some(entry.target.as_path())
        };
        if self.operand.kind != OperandKind::Source {
            if recursive {
                if let Some(index) = table
                    .iter()
                    .enumerate()
                    .find(|(index, entry)| active[*index] && target_matches(entry))
                    .map(|(index, _)| index)
                {
                    return Some(index);
                }
            } else if let Some(index) = table
                .iter()
                .enumerate()
                .rev()
                .find(|(index, entry)| active[*index] && target_matches(entry))
                .map(|(index, _)| index)
            {
                return Some(index);
            }
        }
        if self.operand.kind == OperandKind::Target || recursive {
            return None;
        }
        let by_source = table.iter().enumerate().rev().find(|(index, entry)| {
            active[*index]
                && (entry.source == self.operand.spec
                    || self
                        .resolved
                        .as_ref()
                        .is_some_and(|path| entry.source == path.as_os_str()))
        });
        if let Some((index, _)) = by_source {
            return Some(index);
        }
        let path = self
            .resolved
            .as_deref()
            .unwrap_or_else(|| Path::new(&self.operand.spec));
        let metadata = fs::metadata(path).ok()?;
        if !metadata.file_type().is_block_device() {
            return None;
        }
        let device = MountDevice {
            major: libc::major(metadata.rdev()),
            minor: libc::minor(metadata.rdev()),
        };
        table
            .iter()
            .enumerate()
            .rev()
            .find(|(index, entry)| active[*index] && entry.device == device)
            .map(|(index, _)| index)
    }

    fn missing_message(&self, no_canonicalize: bool) -> &'static str {
        if self.missing
            || (self.resolved.is_none() || no_canonicalize)
                && fs::symlink_metadata(&self.operand.spec)
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        {
            "no mount point specified."
        } else {
            "not mounted."
        }
    }
}

struct MountGraph {
    children: Vec<Vec<usize>>,
}

impl MountGraph {
    fn new(table: &[MountEntry]) -> Self {
        let by_id: HashMap<_, _> = table
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id, index))
            .collect();
        let mut children = vec![Vec::new(); table.len()];
        for (index, entry) in table.iter().enumerate() {
            if let Some(&parent) = by_id.get(&entry.parent_id)
                && parent != index
            {
                children[parent].push(index);
            }
        }
        Self { children }
    }

    fn plan(&self, table: &[MountEntry], root: usize, active: &[bool]) -> Vec<usize> {
        let mut plan = Vec::new();
        let mut pending = vec![(root, false)];
        let mut visited = vec![false; table.len()];
        while let Some((index, expanded)) = pending.pop() {
            if expanded {
                if active[index] {
                    plan.push(index);
                }
                continue;
            }
            if std::mem::replace(&mut visited[index], true) {
                continue;
            }
            pending.push((index, true));
            // LIFO visits later mountinfo entries first, but the overmount must
            // be removed before children hidden beneath it become reachable.
            for &child in &self.children[index] {
                if table[child].target != table[index].target {
                    pending.push((child, false));
                }
            }
            for &child in &self.children[index] {
                if table[child].target == table[index].target {
                    pending.push((child, false));
                }
            }
        }
        plan
    }
}

fn process(
    table: &[MountEntry],
    args: &Args,
    mut unmount: impl FnMut(&MountEntry, &Args) -> Result<Outcome, UmountError>,
    output: &mut impl Write,
) -> Result<i32, Failure> {
    let graph = args.recursive.then(|| MountGraph::new(table));
    let mut active = vec![true; table.len()];
    if args.all {
        let plan = table
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, entry)| args.selects(entry))
            .map(|(index, _)| index);
        let failed = execute_plan(table, plan, None, args, &mut active, &mut unmount, output)?;
        return Ok(if failed { EXIT_FAILURE } else { 0 });
    }

    // Explicit operands contribute their final status, not each step in a chain.
    let mut code = 0;
    for operand in &args.operands {
        let prepared = PreparedSpec::new(operand, args.no_canonicalize);
        let Some(root) = prepared.resolve(table, &active, args.recursive && !args.all_targets)
        else {
            let chain = args.all_targets || args.recursive;
            code = (code + if chain { 1 } else { EXIT_FAILURE }).min(255);
            if !args.quiet {
                let message = if chain {
                    if Path::new(&operand.spec).exists() {
                        "not mounted"
                    } else {
                        "not found"
                    }
                } else {
                    prepared.missing_message(args.no_canonicalize)
                };
                writeln!(output, "umount: {}: {message}", operand.spec.display())
                    .map_err(output_failure)?;
            }
            continue;
        };
        let roots = if args.all_targets {
            0..table.len()
        } else {
            root..root + 1
        };
        let mut failed = false;
        for selected in roots.rev() {
            if args.all_targets && table[selected].device != table[root].device {
                continue;
            }
            if !active[selected] {
                continue;
            }
            let plan = graph
                .as_ref()
                .map(|graph| graph.plan(table, selected, &active));
            let plan = plan.as_deref().unwrap_or(std::slice::from_ref(&selected));
            failed = execute_plan(
                table,
                plan.iter().copied(),
                Some((root, operand.spec.as_os_str())),
                args,
                &mut active,
                &mut unmount,
                output,
            )?;
            if failed {
                break;
            }
        }
        if failed {
            code = (code + EXIT_FAILURE).min(255);
        }
    }
    Ok(code)
}

fn execute_plan(
    table: &[MountEntry],
    plan: impl IntoIterator<Item = usize>,
    operand: Option<(usize, &OsStr)>,
    args: &Args,
    active: &mut [bool],
    unmount: &mut impl FnMut(&MountEntry, &Args) -> Result<Outcome, UmountError>,
    output: &mut impl Write,
) -> Result<bool, Failure> {
    let mut failed = false;
    for index in plan {
        let entry = &table[index];
        match unmount(entry, args) {
            Ok(outcome) => {
                if matches!(outcome, Outcome::Unmounted) {
                    active[index] = false;
                }
                if args.verbose || matches!(outcome, Outcome::ReadOnly) {
                    let action = match outcome {
                        Outcome::Unmounted => "unmounted",
                        Outcome::ReadOnly => "remounted read-only",
                    };
                    writeln!(
                        output,
                        "umount: {} ({}) {action}",
                        entry.target.display(),
                        entry.source.display()
                    )
                    .map_err(output_failure)?;
                }
            }
            Err(error) => {
                failed = true;
                let message = match error {
                    UmountError::Syscall(error)
                        if args.quiet && error.raw_os_error() == Some(libc::EINVAL) =>
                    {
                        None
                    }
                    UmountError::Syscall(error) => Some(error_message(&error)),
                    UmountError::Helper => None,
                    UmountError::HelperStart(helper, error) => Some(format!(
                        "failed to execute {}: {}.",
                        helper.display(),
                        strerror(&error)
                    )),
                };
                if let Some(message) = message {
                    let spec = operand
                        .filter(|(root, _)| *root == index)
                        .map_or(entry.target.as_os_str(), |(_, spec)| spec);
                    writeln!(output, "umount: {}: {message}", spec.display())
                        .map_err(output_failure)?;
                }
                if args.recursive || args.all_targets {
                    break;
                }
            }
        }
    }
    Ok(failed)
}

fn output_failure(error: io::Error) -> Failure {
    failed(format!("write error: {}", strerror(&error)))
}

enum Outcome {
    Unmounted,
    ReadOnly,
}

enum UmountError {
    Syscall(io::Error),
    Helper,
    HelperStart(PathBuf, io::Error),
}

impl From<io::Error> for UmountError {
    fn from(error: io::Error) -> Self {
        Self::Syscall(error)
    }
}

fn find_helper(fs_type: &str) -> Option<PathBuf> {
    let find = |fs_type| {
        let name = format!("umount.{fs_type}");
        HELPER_DIRS
            .iter()
            .map(|directory| Path::new(directory).join(&name))
            .find(|helper| helper.is_file())
    };
    find(fs_type).or_else(|| fs_type.split_once('.').and_then(|(base, _)| find(base)))
}

fn umount_one(entry: &MountEntry, args: &Args) -> Result<Outcome, UmountError> {
    if args.fake {
        return Ok(Outcome::Unmounted);
    }
    if !args.internal_only
        && let Some(helper) = find_helper(&entry.fs_type)
    {
        let mut command = Command::new(&helper);
        command.arg(&entry.target);
        for (enabled, flag) in [
            (args.no_mtab, "-n"),
            (args.lazy, "-l"),
            (args.force, "-f"),
            (args.verbose, "-v"),
            (args.read_only, "-r"),
        ] {
            if enabled {
                command.arg(flag);
            }
        }
        if entry.fs_type.contains('.')
            && !helper
                .as_os_str()
                .as_bytes()
                .ends_with(entry.fs_type.as_bytes())
        {
            command.arg("-t").arg(&entry.fs_type);
        }
        let status = command
            .status()
            .map_err(|error| UmountError::HelperStart(helper, error))?;
        return if status.success() {
            Ok(Outcome::Unmounted)
        } else {
            Err(UmountError::Helper)
        };
    }
    let mut flags = 0;
    if args.lazy {
        flags |= libc::MNT_DETACH;
    }
    if args.force {
        flags |= libc::MNT_FORCE;
    }
    let path = CString::new(entry.target.as_os_str().as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    // SAFETY: path is a NUL-terminated target that outlives the call.
    if unsafe { libc::umount2(path.as_ptr(), flags) } == 0 {
        return Ok(Outcome::Unmounted);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EBUSY) && args.read_only {
        // SAFETY: path remains a valid target, and null source/type/data are
        // valid for a plain read-only remount.
        if unsafe {
            libc::mount(
                std::ptr::null(),
                path.as_ptr(),
                std::ptr::null(),
                libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            )
        } == 0
        {
            return Ok(Outcome::ReadOnly);
        }
        return Err(io::Error::last_os_error().into());
    }
    Err(error.into())
}

fn error_message(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(libc::ENXIO) => "invalid block device.".into(),
        Some(libc::EINVAL) => "not mounted.".into(),
        Some(libc::EIO) => "can't write superblock.".into(),
        Some(libc::EBUSY) => "target is busy.".into(),
        Some(libc::ENOENT) => "no mount point specified.".into(),
        Some(libc::EPERM) => "must be superuser to unmount.".into(),
        Some(libc::EACCES) => "block devices are not permitted on filesystem.".into(),
        _ => format!("umount(2) system call failed: {}.", strerror(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applets::mount::parse_mountinfo_line;

    fn args(options: &[&str]) -> Args {
        let Parsed::Run(args) = Args::parse(
            std::iter::once("umount")
                .chain(options.iter().copied())
                .map(OsString::from)
                .collect(),
        )
        .expect("valid umount arguments") else {
            panic!("expected an unmount request");
        };
        args
    }

    fn table(lines: &str) -> Vec<MountEntry> {
        lines
            .lines()
            .map(|line| parse_mountinfo_line(line.as_bytes()).expect("valid mountinfo"))
            .collect()
    }

    fn record(table: &[MountEntry], args: &Args) -> (i32, Vec<u64>, String) {
        let mut calls = Vec::new();
        let mut output = Vec::new();
        let code = process(
            table,
            args,
            |entry, _| {
                calls.push(entry.id);
                Ok(Outcome::Unmounted)
            },
            &mut output,
        )
        .expect("write diagnostics");
        (code, calls, String::from_utf8(output).unwrap())
    }

    #[test]
    fn all_targets_requires_an_operand_before_reading_the_mount_table() {
        for flags in [
            vec!["umount", "-A"],
            vec!["umount", "--fake", "--all-targets"],
            vec!["umount", "-AR"],
        ] {
            let failure = Args::parse(flags.into_iter().map(OsString::from).collect())
                .err()
                .expect("missing target must be rejected");
            assert_eq!(failure.code, 1);
            assert_eq!(
                failure.message,
                "bad usage\nTry 'umount --help' for more information."
            );
        }
    }

    #[test]
    fn all_targets_matches_the_superblock_not_paths_or_source_names() {
        let table = table(
            "10 1 0:10 / /tree rw - tmpfs tmpfs rw\n\
             11 10 0:11 / /tree/child rw - tmpfs tmpfs rw\n\
             12 1 0:10 /subdir /elsewhere rw - tmpfs tmpfs rw\n\
             13 1 0:13 / /unrelated rw - tmpfs tmpfs rw",
        );
        let (code, calls, _) = record(&table, &args(&["-cA", "/tree"]));
        assert_eq!(code, 0);
        assert_eq!(calls, [12, 10]);

        let (code, calls, _) = record(&table, &args(&["-cA", "/missing"]));
        assert_eq!(code, 1);
        assert!(calls.is_empty());
    }

    #[test]
    fn recursive_order_follows_mount_ids_and_unhides_lower_children() {
        let table = table(
            "10 1 0:10 / /tree rw - tmpfs lower rw\n\
             200 10 0:11 / /tree/hidden rw - tmpfs hidden rw\n\
             12 10 0:12 / /tree rw - tmpfs upper rw\n\
             13 12 0:13 / /tree/visible rw - tmpfs visible rw\n\
             5 10 0:14 / /tree/late rw - tmpfs late rw\n\
             15 1 0:15 / /tree/unrelated rw - tmpfs unrelated rw",
        );
        let (code, calls, _) = record(&table, &args(&["-cR", "/tree"]));
        assert_eq!(code, 0);
        assert_eq!(calls, [13, 12, 5, 200, 10]);
    }

    #[test]
    fn all_and_repeated_operands_preserve_overmount_stacks() {
        let table = table(
            "10 1 0:10 / /tree rw - tmpfs lower rw\n\
             11 10 0:11 / /tree rw - tmpfs upper rw",
        );
        for flags in [vec!["-ac", "-t", "tmpfs"], vec!["-c", "/tree", "/tree"]] {
            let (code, calls, _) = record(&table, &args(&flags));
            assert_eq!(code, 0);
            assert_eq!(calls, [11, 10]);
        }
    }

    #[test]
    fn recursive_failures_count_once_per_operand_and_stop_only_that_chain() {
        let table = table(
            "10 1 0:10 / /tree rw - tmpfs lower rw\n\
             11 10 0:11 / /tree/early rw - tmpfs early rw\n\
             12 10 0:12 / /tree/busy rw - tmpfs busy rw\n\
             13 10 0:13 / /tree/removed rw - tmpfs removed rw\n\
             20 1 0:20 / /independent rw - tmpfs other rw\n\
             30 1 0:30 / /other-busy rw - tmpfs busy rw",
        );
        for (options, expected_code, expected_calls) in [
            (vec!["-cR", "/tree"], EXIT_FAILURE, vec![13, 12]),
            (
                vec!["-cR", "/tree", "/independent"],
                EXIT_FAILURE,
                vec![13, 12, 20],
            ),
            (
                vec!["-cR", "/tree", "/other-busy", "/independent"],
                2 * EXIT_FAILURE,
                vec![13, 12, 30, 20],
            ),
        ] {
            let mut calls = Vec::new();
            let code = process(
                &table,
                &args(&options),
                |entry, _| {
                    calls.push(entry.id);
                    if matches!(entry.id, 12 | 30) {
                        Err(io::Error::from_raw_os_error(libc::EBUSY).into())
                    } else {
                        Ok(Outcome::Unmounted)
                    }
                },
                &mut Vec::new(),
            )
            .unwrap();
            assert_eq!(calls, expected_calls);
            assert_eq!(code, expected_code, "{options:?}");
        }
    }

    #[test]
    fn all_targets_stops_at_failure_without_counting_prior_successes() {
        let table = table(
            "10 1 0:10 / /first rw - tmpfs same rw\n\
             11 1 0:10 / /last rw - tmpfs same rw",
        );
        for options in ["-cA", "-cAR"] {
            for failed_id in [10, 11] {
                let mut calls = Vec::new();
                let code = process(
                    &table,
                    &args(&[options, "/first"]),
                    |entry, _| {
                        calls.push(entry.id);
                        if entry.id == failed_id {
                            Err(io::Error::from_raw_os_error(libc::EBUSY).into())
                        } else {
                            Ok(Outcome::Unmounted)
                        }
                    },
                    &mut Vec::new(),
                )
                .unwrap();
                let expected = if failed_id == 11 {
                    vec![11]
                } else {
                    vec![11, 10]
                };
                assert_eq!(calls, expected);
                assert_eq!(code, EXIT_FAILURE);
            }
        }
    }

    #[test]
    fn explicit_operands_add_failures_but_all_combines_them() {
        let table = table(
            "10 1 0:10 / /busy rw - tmpfs busy rw\n\
             11 1 0:11 / /other-busy rw - tmpfs busy rw\n\
             20 1 0:20 / /present rw - tmpfs present rw",
        );
        for (options, expected_code, expected_calls) in [
            (vec!["-c", "/busy", "/present"], EXIT_FAILURE, vec![10, 20]),
            (
                vec!["-c", "/busy", "/other-busy", "/present"],
                2 * EXIT_FAILURE,
                vec![10, 11, 20],
            ),
            (vec!["-ac", "-t", "tmpfs"], EXIT_FAILURE, vec![20, 11, 10]),
            (
                vec![
                    "-c", "/busy", "/busy", "/busy", "/busy", "/busy", "/busy", "/busy", "/busy",
                ],
                255,
                vec![10; 8],
            ),
        ] {
            let mut calls = Vec::new();
            let code = process(
                &table,
                &args(&options),
                |entry, _| {
                    calls.push(entry.id);
                    if entry.id == 20 {
                        Ok(Outcome::Unmounted)
                    } else {
                        Err(io::Error::from_raw_os_error(libc::EBUSY).into())
                    }
                },
                &mut Vec::new(),
            )
            .unwrap();
            assert_eq!(calls, expected_calls);
            assert_eq!(code, expected_code, "{options:?}");
        }
    }

    #[test]
    fn unresolved_operands_do_not_prevent_independent_work() {
        let table = table("10 1 0:10 / /present rw - tmpfs source rw");
        let (code, calls, errors) = record(&table, &args(&["-c", "/does-not-exist", "/present"]));
        assert_eq!(code, EXIT_FAILURE);
        assert_eq!(calls, [10]);
        assert!(errors.contains("/does-not-exist: no mount point specified."));

        let (code, calls, errors) = record(&table, &args(&["-cq", "/does-not-exist"]));
        assert_eq!(code, EXIT_FAILURE);
        assert!(calls.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn unresolved_chain_operands_contribute_usage_status() {
        let table = table("10 1 0:10 / /present rw - tmpfs source rw");
        for options in ["-cR", "-cA", "-cAR"] {
            let (code, calls, errors) =
                record(&table, &args(&[options, "/does-not-exist", "/present"]));
            assert_eq!(code, 1);
            assert_eq!(calls, [10]);
            assert_eq!(errors, "umount: /does-not-exist: not found\n");

            let (code, calls, errors) = record(
                &table,
                &args(&[options, "-q", "/does-not-exist", "/does-not-exist"]),
            );
            assert_eq!(code, 2);
            assert!(calls.is_empty());
            assert!(errors.is_empty());
        }
    }

    #[test]
    fn fake_short_circuits_helpers_syscalls_and_read_only_fallback() {
        let table = table("10 1 0:10 / /never-mounted rw - tmpfs source rw");
        let args = args(&["--fake", "-cfrv", "/never-mounted"]);
        let mut output = Vec::new();
        let code = process(&table, &args, umount_one, &mut output).unwrap();
        assert_eq!(code, 0);
        assert_eq!(output, b"umount: /never-mounted (source) unmounted\n");
    }

    #[test]
    fn types_filter_replaces_all_exclusions_and_does_not_filter_explicit_targets() {
        let table = table(
            "1 0 0:1 / / rw - ext4 /dev/root rw\n\
             2 1 0:2 / /proc rw - proc proc rw\n\
             3 1 0:3 / /devfs rw - devfs devfs rw\n\
             4 1 0:4 / /pts rw - devpts devpts rw\n\
             5 1 0:5 / /sys rw - sysfs sysfs rw\n\
             6 1 0:6 / /rpc rw - rpc_pipefs rpc_pipefs rw\n\
             7 1 0:7 / /nfsd rw - nfsd nfsd rw\n\
             8 1 0:8 / /selinux rw - selinuxfs selinuxfs rw\n\
             9 1 0:9 / /dev rw - devtmpfs devtmpfs rw\n\
             10 1 0:10 / /mqueue rw - mqueue mqueue rw",
        );
        let (_, calls, _) = record(&table, &args(&["-ac"]));
        assert_eq!(calls, [10, 9]);
        let (_, calls, _) = record(&table, &args(&["-actproc"]));
        assert_eq!(calls, [2]);
        let (_, calls, _) = record(&table, &args(&["-ctext4", "/proc"]));
        assert_eq!(calls, [2]);
    }

    #[test]
    fn all_option_filter_matches_mount_and_superblock_options_without_canonicalizing_targets() {
        let table = table(
            "10 1 0:10 / /stale-fuse rw,nosuid - fuse unreachable rw,size=4096\n\
             11 1 0:11 / /other rw - tmpfs other rw,size=4096\n\
             12 1 0:12 / /third rw,nosuid - tmpfs other rw,size=8192",
        );
        let (code, calls, _) = record(&table, &args(&["-ac", "-O", "rw,size=4096,+nosuid"]));
        assert_eq!(code, 0);
        assert_eq!(calls, [10]);
    }

    #[test]
    fn getopt_handles_end_marker_attached_values_and_named_selectors() {
        let parsed = args(&[
            "-cvitext4",
            "-Orw",
            "-N/proc/self/ns/mnt",
            "--source=/dev/example",
            "--target",
            "/mnt",
            "--",
            "--types=tmpfs",
        ]);
        assert!(parsed.no_canonicalize && parsed.verbose && parsed.internal_only);
        assert_eq!(parsed.types.as_deref(), Some("ext4"));
        assert_eq!(parsed.test_opts.as_deref(), Some("rw"));
        assert_eq!(
            parsed.namespace.as_deref(),
            Some(OsStr::new("/proc/self/ns/mnt"))
        );
        assert_eq!(
            parsed
                .operands
                .iter()
                .map(|operand| operand.spec.as_os_str())
                .collect::<Vec<_>>(),
            ["/dev/example", "/mnt", "--types=tmpfs"].map(OsStr::new)
        );
        assert!(parsed.operands[0].kind == OperandKind::Source);
        assert!(parsed.operands[1].kind == OperandKind::Target);

        let table = table(
            "10 1 0:10 / /mnt rw - tmpfs /dev/example rw\n\
             11 1 0:11 / /dev/example rw - tmpfs other rw",
        );
        let (_, calls, _) = record(&table, &args(&["-c", "--source=/dev/example"]));
        assert_eq!(calls, [10]);
        let (_, calls, _) = record(&table, &args(&["-c", "--target=/dev/example"]));
        assert_eq!(calls, [11]);
    }

    #[test]
    fn parser_errors_use_umount_help_and_do_not_treat_unknown_options_as_targets() {
        for flags in [
            vec!["umount", "-Z"],
            vec!["umount", "--frobnicate"],
            vec!["umount", "-N"],
            vec!["umount", "--types"],
            vec!["umount", "--lazy=yes"],
            vec!["umount", "-aR"],
        ] {
            let failure = Args::parse(flags.into_iter().map(OsString::from).collect())
                .err()
                .expect("invalid invocation");
            assert_eq!(failure.code, 1);
            assert!(
                failure
                    .message
                    .ends_with("Try 'umount --help' for more information.")
            );
            assert!(!failure.message.contains("'mount --help'"));
        }
    }

    #[test]
    fn diagnostics_preserve_operand_spelling_and_do_not_repeat_errno_numbers() {
        let table = table("10 1 0:10 / /target rw - tmpfs source rw");
        let mut output = Vec::new();
        let code = process(
            &table,
            &args(&["-c", "/target/./"]),
            |_, _| Err(io::Error::from_raw_os_error(libc::EFAULT).into()),
            &mut output,
        )
        .unwrap();
        assert_eq!(code, EXIT_FAILURE);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!(
                "umount: /target/./: umount(2) system call failed: {}.\n",
                strerror(&io::Error::from_raw_os_error(libc::EFAULT))
            )
        );
    }

    #[test]
    fn quiet_only_suppresses_not_mounted_errors() {
        let table = table("10 1 0:10 / /target rw - tmpfs source rw");
        for (errno, expected) in [
            (libc::EINVAL, ""),
            (libc::EBUSY, "umount: /target: target is busy.\n"),
            (
                libc::EPERM,
                "umount: /target: must be superuser to unmount.\n",
            ),
        ] {
            let mut output = Vec::new();
            let code = process(
                &table,
                &args(&["-cq", "/target"]),
                |_, _| Err(io::Error::from_raw_os_error(errno).into()),
                &mut output,
            )
            .unwrap();
            assert_eq!(code, EXIT_FAILURE);
            assert_eq!(output, expected.as_bytes());
        }
    }

    #[test]
    fn no_canonicalize_resolves_relative_mountpoints_lexically() {
        let mut table = table("10 1 0:10 / /unused rw - tmpfs source rw");
        table[0].target = std::env::current_dir()
            .unwrap()
            .join("axe-synthetic-stale-mount");
        let (code, calls, _) = record(&table, &args(&["-c", "--target=axe-synthetic-stale-mount"]));
        assert_eq!(code, 0);
        assert_eq!(calls, [10]);
    }
}
