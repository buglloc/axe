use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vzik::environment::Failure;

static NEXT_LINK: AtomicU64 = AtomicU64::new(0);
static BRIDGE_OBSERVATION: OnceLock<Result<BridgeState, Failure>> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) enum BridgeState {
    Reused(PathBuf),
    Published(PathBuf),
    Disabled,
}

const BRIDGE_DIRECTORY_NAME: &str = ".axe-bridge";

pub(crate) fn install(
    names: &[String],
    executable: Option<&Path>,
    runtime_root: Result<&Path, Failure>,
) -> io::Result<Option<PathBuf>> {
    let reusable = executable.and_then(reusable_bridge);
    let reused = reusable.is_some();
    let result = install_impl(names, executable, runtime_root, reusable);
    let observation = match &result {
        Ok(Some(path)) if reused => Ok(BridgeState::Reused(path.clone())),
        Ok(Some(path)) => Ok(BridgeState::Published(path.clone())),
        Ok(None) => Ok(BridgeState::Disabled),
        Err(error) => Err(Failure::from_io("install AXE PATH bridge", error)),
    };
    let _ = BRIDGE_OBSERVATION.set(observation);
    result
}

pub(crate) fn observe() -> Option<&'static Result<BridgeState, Failure>> {
    BRIDGE_OBSERVATION.get()
}

fn install_impl(
    names: &[String],
    executable: Option<&Path>,
    runtime_root: Result<&Path, Failure>,
    reusable: Option<PathBuf>,
) -> io::Result<Option<PathBuf>> {
    clear_environment()?;

    let Some(executable) = executable else {
        return Ok(None);
    };
    if let Some(bin) = reusable {
        prepend_path(&bin)?;
        return Ok(Some(bin));
    }
    validate_names(names)?;

    let root = runtime_root.map_err(|failure| {
        io::Error::other(format!("{}: {}", failure.operation, failure.message))
    })?;
    install_at_root(root, names, executable).map(Some)
}

/// Publish the server's executable path and prepend its bridge to `PATH`.
/// A new server must replace a previous `/proc/<pid>/exe` target even if both
/// processes run the same executable image.
pub(crate) fn publish_server_bridge(
    root: &Path,
    names: &[String],
    executable: &Path,
) -> io::Result<PathBuf> {
    clear_environment()?;
    validate_names(names)?;
    let base = root.join(BRIDGE_DIRECTORY_NAME);
    ensure_private_directory(&base)?;
    let _lock = acquire_lock(&base.join(".lock"))?;
    let bin = publish_bridge(&base, names, executable)?;
    prepend_path(&bin)?;
    Ok(bin)
}

fn install_at_root(root: &Path, names: &[String], executable: &Path) -> io::Result<PathBuf> {
    let base = root.join(BRIDGE_DIRECTORY_NAME);
    ensure_private_directory(&base)?;
    let _lock = acquire_lock(&base.join(".lock"))?;

    let bin = if bridge_matches(&base, names, executable)? {
        base.join("bin")
    } else {
        publish_bridge(&base, names, executable)?
    };
    prepend_path(&bin)?;
    Ok(bin)
}

/// Reuse a bridge published by a trusted parent process (the sshd applet or an
/// outer shell) that exported `AXE_APPLET_DIR`.
///
/// The private bridge contains one `axe` symlink and applet links to it.
/// Checking `axe` against the running image allows trusted children to reuse
/// a bridge without touching the filesystem.
fn reusable_bridge(executable: &Path) -> Option<PathBuf> {
    let directory = std::env::var_os("AXE_APPLET_DIR");
    reusable_bridge_from(directory.as_deref(), executable)
}

fn reusable_bridge_from(directory: Option<&OsStr>, executable: &Path) -> Option<PathBuf> {
    let directory = directory.filter(|directory| !directory.is_empty())?;
    let bin = PathBuf::from(directory);
    if bin.file_name()? != "bin" || bin.parent()?.file_name()? != BRIDGE_DIRECTORY_NAME {
        return None;
    }
    let metadata = fs::symlink_metadata(&bin).ok()?;
    if !metadata.is_dir() {
        return None;
    }
    let target = fs::read_link(bin.join("axe")).ok()?;
    same_inode(&target, executable).then_some(bin)
}

/// Whether both paths resolve to the same file, comparing device and inode so
/// that `/proc/<pid>/exe` references are recognized as the running binary.
#[cfg(unix)]
pub(crate) fn same_inode(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    match (fs::metadata(left), fs::metadata(right)) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        (Err(_), _) | (_, Err(_)) => false,
    }
}

fn validate_names(names: &[String]) -> io::Result<()> {
    for name in names {
        let mut components = Path::new(name).components();
        if !matches!(components.next(), Some(Component::Normal(_)))
            || components.next().is_some()
            || name == "."
            || name == ".."
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid applet name {name:?}"),
            ));
        }
    }
    Ok(())
}

pub(crate) fn ensure_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(io::Error::other(format!(
                    "{} is not a directory",
                    path.display()
                )));
            }
            // SAFETY: geteuid has no preconditions and does not access Rust memory.
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} is owned by another user", path.display()),
                ));
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return ensure_private_directory(path);
            }
            Err(error) => return Err(error),
        },
        Err(error) => return Err(error),
    }
    Ok(())
}

fn acquire_lock(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    for _ in 0..200 {
        // SAFETY: file owns a live descriptor and flock does not access Rust memory.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(file);
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(10)),
            io::ErrorKind::Interrupted => continue,
            _ => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("timed out waiting for {}", path.display()),
    ))
}

fn bridge_matches(base: &Path, names: &[String], executable: &Path) -> io::Result<bool> {
    let bin = base.join("bin");
    let metadata = match fs::symlink_metadata(&bin) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() {
        return Ok(false);
    }

    let axe = bin.join("axe");
    let target = match fs::read_link(&axe) {
        Ok(target) => target,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if !same_inode(&target, executable) {
        return Ok(false);
    }

    let mut expected = HashSet::with_capacity(names.len() + 1);
    expected.insert("axe");
    expected.extend(names.iter().map(String::as_str));
    for entry in fs::read_dir(&bin)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            return Ok(false);
        };
        if !expected.remove(name)
            || !entry.file_type()?.is_symlink()
            || (name != "axe" && fs::read_link(entry.path())? != axe)
        {
            return Ok(false);
        }
    }
    Ok(expected.is_empty())
}

fn publish_bridge(base: &Path, names: &[String], executable: &Path) -> io::Result<PathBuf> {
    let bin = base.join("bin");
    ensure_private_directory(&bin)?;
    let axe = bin.join("axe");
    let expected: HashSet<&str> = names.iter().map(String::as_str).collect();

    for name in names {
        if name == "axe" {
            continue;
        }
        let path = bin.join(name);
        if fs::read_link(&path).ok().as_deref() != Some(axe.as_path()) {
            if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
                fs::remove_dir_all(&path)?;
            }
            publish_link(&axe, &path)?;
        }
    }
    if fs::read_link(&axe).ok().as_deref() != Some(executable) {
        if fs::symlink_metadata(&axe).is_ok_and(|metadata| metadata.is_dir()) {
            fs::remove_dir_all(&axe)?;
        }
        publish_link(executable, &axe)?;
    }
    for entry in fs::read_dir(&bin)? {
        let entry = entry?;
        let name = entry.file_name();
        if name != "axe" && !name.to_str().is_some_and(|name| expected.contains(name)) {
            remove_path(&entry.path())?;
        }
    }
    sync_directory(&bin)?;
    Ok(bin)
}

fn publish_link(target: &Path, path: &Path) -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let parent = path.parent().expect("bridge link has parent");
    let next = parent.join(format!(".axe-link-{}", link_suffix()));
    symlink(target, &next)?;
    let result = fs::rename(&next, path);
    if result.is_err() {
        let _ = fs::remove_file(&next);
    }
    result
}

fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn link_suffix() -> String {
    let nonce = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{}-{time}-{nonce}", std::process::id())
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

pub(crate) fn clear_environment() -> io::Result<()> {
    let marker = std::env::var_os("AXE_APPLET_DIR")
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from);
    let path = std::env::var_os("PATH");
    let cleaned = path_without(marker.as_deref(), path.as_deref())?;

    // SAFETY: callers run before Brush or daemon background threads start.
    unsafe {
        std::env::remove_var("AXE_APPLET_DIR");
        if let Some(path) = cleaned {
            std::env::set_var("PATH", path);
        }
    }
    Ok(())
}

fn path_without(marker: Option<&Path>, path: Option<&OsStr>) -> io::Result<Option<OsString>> {
    path.map(|path| {
        std::env::join_paths(
            std::env::split_paths(path).filter(|candidate| marker != Some(candidate.as_path())),
        )
    })
    .transpose()
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn prepend_path(bin: &Path) -> io::Result<()> {
    let current = std::env::var_os("PATH");
    let joined = path_with_front(bin, current.as_deref())?;

    // SAFETY: the top-level shell has not started Brush or any background threads yet.
    unsafe {
        std::env::set_var("AXE_APPLET_DIR", bin);
        std::env::set_var("PATH", joined);
    }
    Ok(())
}

fn path_with_front(bin: &Path, current: Option<&OsStr>) -> io::Result<OsString> {
    let mut paths = Vec::new();
    paths.push(bin.to_owned());
    if let Some(current) = current {
        paths.extend(std::env::split_paths(current).filter(|path| path != bin));
    }
    std::env::join_paths(paths).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "axe-path-bridge-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create scratch directory");
        directory
    }

    fn names() -> Vec<String> {
        vec![String::from("ls"), String::from("grep")]
    }

    #[test]
    fn proc_exe_bridge_is_reused_by_inode() {
        let root = scratch("proc-reuse");
        let base = root.join(BRIDGE_DIRECTORY_NAME);

        ensure_private_directory(&base).expect("create bridge base");
        let bin = publish_bridge(&base, &names(), Path::new("/proc/self/exe"))
            .expect("publish /proc/self/exe bridge");
        assert_eq!(
            fs::read_link(bin.join("axe")).expect("read axe symlink"),
            Path::new("/proc/self/exe")
        );

        // A shell resolving its canonical executable can reuse a live server
        // target because both references point to the same inode.
        let shell_executable = std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable");
        assert!(
            bridge_matches(&base, &names(), &shell_executable).expect("validate server bridge")
        );

        // A different binary cannot reuse the bridge.
        let foreign = root.join("foreign");
        fs::write(&foreign, b"not the same executable").expect("write foreign file");
        assert!(
            !bridge_matches(&base, &names(), &foreign)
                .expect("validate against foreign executable")
        );

        fs::remove_dir_all(&root).expect("clean scratch directory");
    }

    #[test]
    fn dangling_proc_reference_fails_validation() {
        let root = scratch("dangling");
        let base = root.join(BRIDGE_DIRECTORY_NAME);
        ensure_private_directory(&base).expect("create bridge base");
        let shell_executable = std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable");

        publish_bridge(&base, &names(), Path::new("/proc/99999999/exe"))
            .expect("publish bridge for a dead process");
        assert!(
            !bridge_matches(&base, &names(), &shell_executable).expect("validate dangling bridge")
        );

        fs::remove_dir_all(&root).expect("clean scratch directory");
    }

    #[test]
    fn marker_reuse_requires_running_executable() {
        let root = scratch("marker");
        let base = root.join(BRIDGE_DIRECTORY_NAME);
        ensure_private_directory(&base).expect("create bridge base");
        let bin =
            publish_bridge(&base, &names(), Path::new("/proc/self/exe")).expect("publish bridge");

        let marker = bin.clone().into_os_string();
        let shell_executable = std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable");
        assert_eq!(
            reusable_bridge_from(Some(&marker), &shell_executable).as_deref(),
            Some(bin.as_path())
        );

        let foreign = root.join("foreign");
        fs::write(&foreign, b"not the same executable").expect("write foreign file");
        assert_eq!(reusable_bridge_from(Some(&marker), &foreign), None);

        let legacy = root.join(".axe-0123456789abcdef");
        ensure_private_directory(&legacy).expect("create old bridge base");
        let old_bin = publish_bridge(&legacy, &names(), Path::new("/proc/self/exe"))
            .expect("publish old bridge");
        assert_eq!(
            reusable_bridge_from(Some(old_bin.as_os_str()), &shell_executable),
            None
        );

        fs::remove_dir_all(&root).expect("clean scratch directory");
    }

    #[test]
    fn canonical_filesystem_target_is_published_verbatim() {
        let root = scratch("filesystem-target");
        let base = root.join(BRIDGE_DIRECTORY_NAME);
        let executable = std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable");

        ensure_private_directory(&base).expect("create bridge base");
        let bin = publish_bridge(&base, &names(), &executable).expect("publish filesystem bridge");

        assert_eq!(
            fs::read_link(bin.join("axe")).expect("read axe symlink"),
            executable
        );
        fs::remove_dir_all(&root).expect("clean scratch directory");
    }

    #[test]
    fn republishing_switches_the_executable_and_removes_stale_applets() {
        use std::os::unix::fs::{MetadataExt as _, symlink};

        let root = scratch("republish");
        let base = root.join(BRIDGE_DIRECTORY_NAME);
        let original = std::env::current_exe().expect("locate test executable");
        ensure_private_directory(&base).expect("create bridge base");
        let bin = publish_bridge(&base, &names(), &original).expect("publish first bridge");
        let applet = bin.join("ls");
        assert_eq!(
            fs::read_link(&applet).expect("read applet link"),
            bin.join("axe")
        );

        let abandoned = bin.join(".axe-link-abandoned");
        symlink(&original, &abandoned).expect("create interrupted publication");
        let replacement = root.join("replacement");
        fs::write(&replacement, b"replacement executable").expect("write replacement image");
        let new_names = vec![String::from("ls"), String::from("cat")];
        publish_bridge(&base, &new_names, &replacement).expect("replace bridge");

        assert_eq!(
            fs::metadata(&applet).expect("resolve applet").ino(),
            fs::metadata(&replacement).expect("read replacement").ino()
        );
        assert!(!bin.join("grep").exists(), "obsolete applet survived");
        assert!(bin.join("cat").exists(), "new applet was not published");
        assert!(!abandoned.exists(), "abandoned link survived");
        assert!(bridge_matches(&base, &new_names, &replacement).expect("validate replacement"));
        fs::remove_dir_all(root).expect("clean scratch directory");
    }

    #[test]
    fn failed_publish_preserves_existing_path_commands() {
        use std::os::unix::fs::MetadataExt as _;

        let root = scratch("failed-publish");
        let base = root.join(BRIDGE_DIRECTORY_NAME);
        let executable = std::env::current_exe().expect("locate test executable");
        ensure_private_directory(&base).expect("create bridge base");
        let bin = publish_bridge(&base, &names(), &executable).expect("publish first bridge");
        let invalid_name = "x".repeat(4096);
        let replacement = vec![String::from("ls"), String::from("cat"), invalid_name];

        assert!(
            publish_bridge(&base, &replacement, &executable).is_err(),
            "overlong applet name unexpectedly published"
        );
        let old_command = fs::metadata(bin.join("grep")).expect("existing command remains usable");
        let original = fs::metadata(&executable).expect("read executable");
        assert_eq!(
            (old_command.dev(), old_command.ino()),
            (original.dev(), original.ino())
        );
        fs::remove_dir_all(root).expect("clean scratch directory");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_timestamp_cannot_steal_an_active_bridge_lock() {
        let root = scratch("active-lock");
        let base = root.join(BRIDGE_DIRECTORY_NAME);
        let executable = std::env::current_exe().expect("locate test executable");
        ensure_private_directory(&base).expect("create bridge base");
        publish_bridge(&base, &names(), &executable).expect("publish first bridge");
        let lock_path = base.join(".lock");
        let first = acquire_lock(&lock_path).expect("lock published bridge");
        let old_time = SystemTime::now() - Duration::from_secs(3600);
        File::open(&lock_path)
            .expect("open active lock")
            .set_times(fs::FileTimes::new().set_modified(old_time))
            .expect("age active lock");

        let replacement = vec![String::from("ls"), String::from("cat")];
        let error = install_at_root(&root, &replacement, &executable)
            .expect_err("second publisher must not acquire active lock");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(bridge_matches(&base, &names(), &executable).expect("verify original commands"));
        drop(first);
        install_at_root(&root, &replacement, &executable)
            .expect("released lock permits next publisher");
        assert!(bridge_matches(&base, &replacement, &executable).expect("verify replacement"));
        fs::remove_dir_all(root).expect("clean scratch directory");
    }

    #[test]
    fn concurrent_publishers_leave_one_complete_inventory() {
        use std::os::unix::fs::MetadataExt as _;

        let root = scratch("concurrent-publishers");
        let base = root.join(BRIDGE_DIRECTORY_NAME);
        let executable = std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable");
        ensure_private_directory(&base).expect("create bridge base");

        let barrier = std::sync::Barrier::new(2);
        thread::scope(|scope| {
            for extra in ["grep", "cat"] {
                let base = &base;
                let executable = &executable;
                let barrier = &barrier;
                scope.spawn(move || {
                    let names = vec![String::from("ls"), extra.to_owned()];
                    barrier.wait();
                    let _lock = acquire_lock(&base.join(".lock")).expect("lock bridge");
                    if !bridge_matches(base, &names, executable).expect("validate bridge") {
                        publish_bridge(base, &names, executable).expect("publish bridge");
                    }
                });
            }
        });

        let bin = base.join("bin");
        let mut entries = fs::read_dir(&bin)
            .expect("read bridge")
            .map(|entry| {
                entry
                    .expect("read bridge entry")
                    .file_name()
                    .into_string()
                    .expect("UTF-8 applet name")
            })
            .collect::<Vec<_>>();
        entries.sort();
        assert!(
            entries == ["axe", "cat", "ls"] || entries == ["axe", "grep", "ls"],
            "bridge contains a partial inventory: {entries:?}"
        );
        let applet = fs::metadata(bin.join("ls")).expect("resolve applet");
        let executable = fs::metadata(&executable).expect("read executable");
        assert_eq!(
            (applet.dev(), applet.ino()),
            (executable.dev(), executable.ino())
        );
        fs::remove_dir_all(root).expect("clean scratch directory");
    }

    #[test]
    fn environment_projection_removes_stale_marker_and_deduplicates_path() {
        let stale = PathBuf::from("/stale/bin");
        let bin = PathBuf::from("/current/bin");
        let other = PathBuf::from("/other/bin");
        let initial = std::env::join_paths([
            stale.as_path(),
            bin.as_path(),
            stale.as_path(),
            bin.as_path(),
            other.as_path(),
        ])
        .expect("join initial PATH");

        let cleaned = path_without(Some(&stale), Some(&initial))
            .expect("remove stale marker")
            .expect("PATH remains present");
        let projected = path_with_front(&bin, Some(&cleaned)).expect("prepend selected bridge");
        let paths: Vec<PathBuf> = std::env::split_paths(&projected).collect();

        assert_eq!(paths, [bin, other]);
        assert!(!paths.contains(&stale));
    }
}
