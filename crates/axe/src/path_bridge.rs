use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vzik::environment::Failure;

use sha2::{Digest, Sha256};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);
static BRIDGE_OBSERVATION: OnceLock<Result<BridgeState, Failure>> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) enum BridgeState {
    Reused(PathBuf),
    Published(PathBuf),
    Disabled,
}

const LOCK_STALE_AFTER: Duration = Duration::from_secs(120);
const MAX_GENERATIONS: usize = 2;

pub(crate) fn install(
    names: &[String],
    executable: Option<&Path>,
    runtime_root: Result<&Path, Failure>,
) -> io::Result<Option<PathBuf>> {
    let reusable = executable.and_then(reusable_bridge);
    let result = install_impl(names, executable, runtime_root);
    let observation = match &result {
        Ok(Some(path)) if reusable.as_deref() == Some(path) => {
            Ok(BridgeState::Reused(path.clone()))
        }
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
) -> io::Result<Option<PathBuf>> {
    let reusable = executable.and_then(reusable_bridge);
    clear_environment()?;

    let Some(executable) = executable else {
        return Ok(None);
    };
    validate_names(names)?;
    if let Some(bin) = reusable {
        prepend_path(&bin)?;
        return Ok(Some(bin));
    }

    let root = runtime_root.map_err(|failure| {
        io::Error::other(format!("{}: {}", failure.operation, failure.message))
    })?;
    install_at_root(root, names, executable).map(Some)
}

/// Publish a fresh bridge generation for a long-lived server process and
/// prepend it to `PATH` for the server's children.
///
/// Unlike [`install`] this never reuses an existing generation: the
/// `executable` is expected to reference the live server process (for example
/// `/proc/<pid>/exe`), so every server start must republish under its own
/// process reference.
pub(crate) fn publish_server_bridge(
    root: &Path,
    names: &[String],
    executable: &Path,
) -> io::Result<PathBuf> {
    clear_environment()?;
    validate_names(names)?;
    let base = root.join(format!(".axe-{}", registry_pin(names)));
    ensure_private_directory(&base)?;
    let _lock = acquire_lock(&base.join(".lock"))?;
    let bin = publish_generation(&base, names, executable)?;
    prepend_path(&bin)?;
    Ok(bin)
}

fn install_at_root(root: &Path, names: &[String], executable: &Path) -> io::Result<PathBuf> {
    let base = root.join(format!(".axe-{}", registry_pin(names)));
    ensure_private_directory(&base)?;
    let _lock = acquire_lock(&base.join(".lock"))?;

    if let Some(bin) = valid_current_bin(&base, names, executable)? {
        prepend_path(&bin)?;
        return Ok(bin);
    }

    let bin = publish_generation(&base, names, executable)?;
    prepend_path(&bin)?;
    Ok(bin)
}

/// Reuse a bridge published by a trusted parent process (the sshd applet or an
/// outer shell) that exported `AXE_APPLET_DIR`.
///
/// A generation is written atomically with a single symlink target, so
/// confirming that its `axe` entry resolves to the running executable is
/// enough to trust the whole directory without revalidating every entry.
fn reusable_bridge(executable: &Path) -> Option<PathBuf> {
    let directory = std::env::var_os("AXE_APPLET_DIR");
    reusable_bridge_from(directory.as_deref(), executable)
}

fn reusable_bridge_from(directory: Option<&OsStr>, executable: &Path) -> Option<PathBuf> {
    let directory = directory.filter(|directory| !directory.is_empty())?;
    let bin = PathBuf::from(directory);
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

fn registry_pin(names: &[String]) -> String {
    use std::fmt::Write as _;

    let mut hash = Sha256::new();
    for name in names {
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
    }
    let digest = hash.finalize();
    // 16 hex characters (64 bits) keep registry pins collision-free for any
    // realistic set of coexisting builds; a collision only causes generations
    // of two registries to republish over each other, never a wrong exec.
    let mut encoded = String::with_capacity(16);
    for byte in &digest[..8] {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
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

fn acquire_lock(path: &Path) -> io::Result<LockGuard> {
    for _ in 0..200 {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => {
                file.sync_all()?;
                return Ok(LockGuard(path.to_owned()));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if lock_stale(path) {
                    let _ = fs::remove_file(path);
                } else {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("timed out waiting for {}", path.display()),
    ))
}

fn lock_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .and_then(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .map_err(io::Error::other)
        })
        .is_ok_and(|age| age > LOCK_STALE_AFTER)
}

fn valid_current_bin(
    base: &Path,
    names: &[String],
    executable: &Path,
) -> io::Result<Option<PathBuf>> {
    let target = match fs::read_link(base.join("current")) {
        Ok(target) => target,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let mut components = target.components();
    if components.next() != Some(Component::Normal(OsStr::new("generations")))
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Ok(None);
    }

    let bin = base.join(target).join("bin");
    if generation_matches(&bin, names, executable)? {
        Ok(Some(base.join("current/bin")))
    } else {
        Ok(None)
    }
}

fn generation_matches(bin: &Path, names: &[String], executable: &Path) -> io::Result<bool> {
    let metadata = match fs::symlink_metadata(bin) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() {
        return Ok(false);
    }

    let mut expected = HashSet::with_capacity(names.len() + 1);
    expected.insert("axe");
    expected.extend(names.iter().map(String::as_str));
    // One generation publishes every entry with the same symlink target; that
    // target is validated by inode so `/proc/<pid>/exe` references published
    // by a server are recognized while the process is alive.
    let mut target: Option<PathBuf> = None;
    for entry in fs::read_dir(bin)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            return Ok(false);
        };
        if !expected.remove(name) {
            return Ok(false);
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_symlink() {
            return Ok(false);
        }
        let link = fs::read_link(entry.path())?;
        match &target {
            None => {
                if !same_inode(&link, executable) {
                    return Ok(false);
                }
                target = Some(link);
            }
            Some(target) if *target != link => return Ok(false),
            _ => {}
        }
    }
    Ok(expected.is_empty())
}

fn publish_generation(base: &Path, names: &[String], executable: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let generations = base.join("generations");
    ensure_private_directory(&generations)?;

    let suffix = generation_suffix();
    let staging = generations.join(format!(".tmp-{suffix}"));
    let generation_name = format!("g-{suffix}");
    let generation = generations.join(&generation_name);
    fs::create_dir(&staging)?;
    let result = (|| {
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))?;
        let bin = staging.join("bin");
        fs::create_dir(&bin)?;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;

        symlink(executable, bin.join("axe"))?;
        for name in names {
            if name != "axe" {
                symlink(executable, bin.join(name))?;
            }
        }

        sync_directory(&bin)?;
        sync_directory(&staging)?;
        fs::rename(&staging, &generation)?;
        sync_directory(&generations)?;

        let current = base.join("current");
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.is_dir()) {
            fs::remove_dir_all(&current)?;
        }
        let target = Path::new("generations").join(&generation_name);
        let next = base.join(format!(".current-{suffix}"));
        symlink(target, &next)?;
        fs::rename(&next, &current)?;
        sync_directory(base)?;
        prune_generations(&generations, &generation_name)?;
        Ok(base.join("current/bin"))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn prune_generations(generations: &Path, current: &str) -> io::Result<()> {
    let mut obsolete = Vec::new();
    for entry in fs::read_dir(generations)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(".tmp-") {
            remove_path(&entry.path())?;
            continue;
        }
        if !name.starts_with("g-") || name == current {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        obsolete.push((modified, entry.path()));
    }
    obsolete.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, path) in obsolete.into_iter().skip(MAX_GENERATIONS.saturating_sub(1)) {
        remove_path(&path)?;
    }
    sync_directory(generations)
}

fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn generation_suffix() -> String {
    let nonce = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
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

struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
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
    fn proc_exe_generation_is_reused_by_inode() {
        let root = scratch("proc-reuse");
        let base = root.join(".axe-test");

        ensure_private_directory(&base).expect("create bridge base");
        let bin = publish_generation(&base, &names(), Path::new("/proc/self/exe"))
            .expect("publish /proc/self/exe generation");
        assert_eq!(
            fs::read_link(bin.join("axe")).expect("read axe symlink"),
            Path::new("/proc/self/exe")
        );

        // A shell resolving its own canonical executable reuses the server
        // generation because both reference the same inode.
        let shell_executable = std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable");
        let reused = valid_current_bin(&base, &names(), &shell_executable)
            .expect("validate current generation");
        assert_eq!(reused, Some(base.join("current/bin")));

        // A different binary must not reuse the generation.
        let foreign = root.join("foreign");
        fs::write(&foreign, b"not the same executable").expect("write foreign file");
        assert_eq!(
            valid_current_bin(&base, &names(), &foreign)
                .expect("validate against foreign executable"),
            None
        );

        fs::remove_dir_all(&root).expect("clean scratch directory");
    }

    #[test]
    fn dangling_proc_reference_fails_validation() {
        let root = scratch("dangling");
        let base = root.join(".axe-test");
        ensure_private_directory(&base).expect("create bridge base");
        let shell_executable = std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable");

        publish_generation(&base, &names(), Path::new("/proc/99999999/exe"))
            .expect("publish generation for a dead process");
        assert_eq!(
            valid_current_bin(&base, &names(), &shell_executable)
                .expect("validate dangling generation"),
            None
        );

        fs::remove_dir_all(&root).expect("clean scratch directory");
    }

    #[test]
    fn marker_reuse_requires_running_executable() {
        let root = scratch("marker");
        let base = root.join(".axe-test");
        ensure_private_directory(&base).expect("create bridge base");
        let bin = publish_generation(&base, &names(), Path::new("/proc/self/exe"))
            .expect("publish generation");

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

        fs::remove_dir_all(&root).expect("clean scratch directory");
    }

    #[test]
    fn canonical_filesystem_target_is_published_verbatim() {
        let root = scratch("filesystem-target");
        let base = root.join(".axe-test");
        let executable = std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable");

        ensure_private_directory(&base).expect("create bridge base");
        let bin = publish_generation(&base, &names(), &executable)
            .expect("publish filesystem generation");

        assert_eq!(
            fs::read_link(bin.join("axe")).expect("read axe symlink"),
            executable
        );
        fs::remove_dir_all(&root).expect("clean scratch directory");
    }

    #[test]
    fn publication_bounds_generation_retention() {
        let root = scratch("generation-retention");
        let base = root.join(".axe-test");
        let executable = std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable");

        ensure_private_directory(&base).expect("create bridge base");
        for _ in 0..5 {
            publish_generation(&base, &names(), &executable).expect("publish generation");
        }
        let generations = base.join("generations");
        fs::create_dir(generations.join(".tmp-abandoned")).expect("create abandoned staging");
        publish_generation(&base, &names(), &executable).expect("publish after abandoned staging");
        let retained = fs::read_dir(&generations)
            .expect("read generations")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("g-"))
            .count();

        assert_eq!(retained, MAX_GENERATIONS);
        assert!(!generations.join(".tmp-abandoned").exists());
        assert!(
            valid_current_bin(&base, &names(), &executable)
                .expect("validate retained generation")
                .is_some()
        );
        fs::remove_dir_all(&root).expect("clean scratch directory");
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
