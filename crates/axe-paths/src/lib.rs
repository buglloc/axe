use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountExecutionPolicy {
    Supported,
    NoExec,
    Unknown,
    Unsupported,
}

pub fn store_candidate_roots(
    explicit: Option<PathBuf>,
    persistent_roots: &[PathBuf],
    tmpfs_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(path) = explicit {
        push_unique(&mut roots, path);
    }
    for root in persistent_roots {
        push_unique(&mut roots, root.clone());
    }

    if let Some(root) = store_platform_cache_root() {
        push_unique(&mut roots, root);
    }

    for root in detected_store_mount_roots(false) {
        push_unique(&mut roots, root);
    }

    for root in tmpfs_roots {
        push_unique(&mut roots, root.clone());
    }

    for root in detected_store_mount_roots(true) {
        push_unique(&mut roots, root);
    }

    push_unique(&mut roots, std::env::temp_dir().join("axe-store"));
    roots
}

pub fn select_runtime_root(
    explicit: Option<PathBuf>,
    inherited: Option<PathBuf>,
) -> io::Result<PathBuf> {
    select_runtime_root_from(explicit, inherited, automatic_runtime_roots())
}

pub fn runtime_relay_fallback_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = absolute_environment_path("XDG_RUNTIME_DIR") {
        push_unique(&mut roots, root.join("axe"));
    }
    push_unique(&mut roots, temporary_runtime_root());
    roots
}

fn select_runtime_root_from(
    explicit: Option<PathBuf>,
    inherited: Option<PathBuf>,
    automatic: Vec<PathBuf>,
) -> io::Result<PathBuf> {
    if let Some(root) = explicit {
        ensure_writable_root(&root, 0)?;
        return Ok(root.canonicalize().unwrap_or(root));
    }
    if let Some(root) = inherited
        && root.is_absolute()
        && ensure_inherited_runtime_root(&root).is_ok()
    {
        return Ok(root.canonicalize().unwrap_or(root));
    }

    automatic
        .into_iter()
        .find_map(|root| {
            ensure_private_writable_root(&root, 0)
                .ok()
                .map(|()| root.canonicalize().unwrap_or(root))
        })
        .ok_or_else(|| io::Error::other("no private writable AXE runtime root is available"))
}

fn ensure_inherited_runtime_root(root: &Path) -> io::Result<()> {
    ensure_writable_root(root, 0)?;
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    Ok(())
}

fn automatic_runtime_roots() -> Vec<PathBuf> {
    automatic_runtime_roots_from(
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        std::env::temp_dir(),
    )
}

fn automatic_runtime_roots_from(
    xdg_cache_home: Option<PathBuf>,
    home: Option<PathBuf>,
    xdg_runtime_dir: Option<PathBuf>,
    temporary_dir: PathBuf,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(cache) = xdg_cache_home.filter(|path| path.is_absolute()) {
        push_unique(&mut roots, cache.join("axe"));
    } else if let Some(home) = home.filter(|path| path.is_absolute()) {
        #[cfg(target_os = "macos")]
        push_unique(&mut roots, home.join("Library/Caches/axe"));
        #[cfg(not(target_os = "macos"))]
        push_unique(&mut roots, home.join(".cache/axe"));
    }

    if let Some(runtime) = xdg_runtime_dir.filter(|path| path.is_absolute()) {
        push_unique(&mut roots, runtime.join("axe"));
    }

    let temporary = if temporary_dir.is_absolute() {
        temporary_dir
    } else {
        default_temporary_directory()
    };
    push_unique(
        &mut roots,
        temporary.join(format!("axe-{}", effective_user_id())),
    );
    roots
}

pub fn ensure_writable_root(root: &Path, required_bytes: u64) -> io::Result<()> {
    fs::create_dir_all(root)?;

    let probe = root.join(format!(
        ".axe-write-probe-{}-{}",
        std::process::id(),
        NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|file| file.sync_all());
    let _ = fs::remove_file(&probe);
    result?;

    check_available_space(root, required_bytes)
}

#[cfg(unix)]
pub fn ensure_private_writable_root(root: &Path, required_bytes: u64) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    fs::create_dir_all(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    if metadata.uid() != effective_user_id() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by another user", root.display()),
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }

    ensure_writable_root(root, required_bytes)
}

#[cfg(not(unix))]
pub fn ensure_private_writable_root(root: &Path, required_bytes: u64) -> io::Result<()> {
    ensure_writable_root(root, required_bytes)
}

fn store_platform_cache_root() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Some(path) = std::env::var_os("XDG_CACHE_HOME").filter(|path| !path.is_empty()) {
            return Some(PathBuf::from(path).join("axe/store"));
        }
        if let Some(path) = std::env::var_os("HOME").filter(|path| !path.is_empty()) {
            return Some(PathBuf::from(path).join(".cache/axe/store"));
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(path) = std::env::var_os("HOME").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path).join("Library/Caches/axe/store"));
    }

    None
}

fn absolute_environment_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn temporary_runtime_root() -> PathBuf {
    let temporary = std::env::temp_dir();
    let temporary = if temporary.is_absolute() {
        temporary
    } else {
        default_temporary_directory()
    };
    temporary.join(format!("axe-{}", effective_user_id()))
}

#[cfg(unix)]
fn default_temporary_directory() -> PathBuf {
    PathBuf::from("/tmp")
}

#[cfg(not(unix))]
fn default_temporary_directory() -> PathBuf {
    PathBuf::from(".")
}

#[cfg(unix)]
fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no preconditions and does not access Rust memory.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn effective_user_id() -> u32 {
    0
}

#[cfg(target_os = "linux")]
fn detected_store_mount_roots(tmpfs_only: bool) -> Vec<PathBuf> {
    let Ok(bytes) = fs::read("/proc/self/mountinfo") else {
        return Vec::new();
    };

    let mut roots = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        let fields = line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        let Some(separator) = fields.iter().position(|field| *field == b"-") else {
            continue;
        };
        if fields.len() <= separator + 3 || fields.len() <= 5 {
            continue;
        }

        let is_tmpfs = fields[separator + 1] == b"tmpfs";
        if is_tmpfs != tmpfs_only || !has_option(fields[5], b"rw") {
            continue;
        }

        let mount = decode_mount(fields[4]);
        if !mount.is_dir() {
            continue;
        }

        let root = if mount == Path::new("/") {
            std::env::temp_dir().join("axe-store")
        } else {
            mount.join(".axe-store")
        };
        push_unique(&mut roots, root);
    }
    roots
}

#[cfg(not(target_os = "linux"))]
fn detected_store_mount_roots(_tmpfs_only: bool) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "linux")]
pub fn has_option(options: &[u8], expected: &[u8]) -> bool {
    options
        .split(|byte| *byte == b',')
        .any(|option| option == expected)
}

/// Reports the execution policy of the most specific mount containing `path`.
#[cfg(target_os = "linux")]
pub fn mount_execution_policy(path: &Path) -> MountExecutionPolicy {
    let Ok(bytes) = fs::read("/proc/self/mountinfo") else {
        return MountExecutionPolicy::Unknown;
    };
    let mut best = None;

    for line in bytes.split(|byte| *byte == b'\n') {
        let fields = line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        let Some(separator) = fields.iter().position(|field| *field == b"-") else {
            continue;
        };
        if fields.len() <= separator + 3 || fields.len() <= 5 {
            continue;
        }

        let mount = decode_mount(fields[4]);
        if path.starts_with(&mount)
            && best
                .as_ref()
                .is_none_or(|(candidate, _): &(PathBuf, bool)| {
                    mount.components().count() > candidate.components().count()
                })
        {
            let noexec =
                has_option(fields[5], b"noexec") || has_option(fields[separator + 3], b"noexec");
            best = Some((mount, noexec));
        }
    }

    match best {
        Some((_, true)) => MountExecutionPolicy::NoExec,
        Some((_, false)) => MountExecutionPolicy::Supported,
        None => MountExecutionPolicy::Unknown,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn mount_execution_policy(_path: &Path) -> MountExecutionPolicy {
    MountExecutionPolicy::Unsupported
}

#[cfg(target_os = "linux")]
pub fn path_is_noexec(path: &Path) -> bool {
    mount_execution_policy(path) == MountExecutionPolicy::NoExec
}

#[cfg(not(target_os = "linux"))]
pub fn path_is_noexec(_path: &Path) -> bool {
    false
}

#[cfg(target_os = "linux")]
pub fn decode_mount(encoded: &[u8]) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut bytes = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] == b'\\'
            && index + 3 < encoded.len()
            && encoded[index + 1..index + 4]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            let value = (encoded[index + 1] - b'0') * 64
                + (encoded[index + 2] - b'0') * 8
                + encoded[index + 3]
                - b'0';
            bytes.push(value);
            index += 4;
        } else {
            bytes.push(encoded[index]);
            index += 1;
        }
    }
    OsString::from_vec(bytes).into()
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

#[cfg(unix)]
pub fn available_bytes(root: &Path) -> io::Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let encoded = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "storage path contains NUL"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: encoded is NUL-terminated and stat points to writable storage.
    if unsafe { libc::statvfs(encoded.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs returned success and initialized the structure.
    let stat = unsafe { stat.assume_init() };
    let available = u128::from(stat.f_bavail) * u128::from(stat.f_frsize);

    Ok(u64::try_from(available).unwrap_or(u64::MAX))
}

#[cfg(not(unix))]
pub fn available_bytes(_root: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "available filesystem bytes are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn check_available_space(root: &Path, required_bytes: u64) -> io::Result<()> {
    if available_bytes(root)? < required_bytes {
        return Err(io::Error::other(format!(
            "{} has insufficient free space",
            root.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_available_space(_root: &Path, _required_bytes: u64) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_candidates_ignore_mounts_and_use_fixed_order() {
        let roots = automatic_runtime_roots_from(
            Some(PathBuf::from("/cache")),
            Some(PathBuf::from("/home/user")),
            Some(PathBuf::from("/run/user/1000")),
            PathBuf::from("/tmp"),
        );

        assert_eq!(
            roots,
            [
                PathBuf::from("/cache/axe"),
                PathBuf::from("/run/user/1000/axe"),
                PathBuf::from(format!("/tmp/axe-{}", effective_user_id())),
            ]
        );
    }

    #[test]
    fn invalid_explicit_runtime_root_does_not_fall_back() {
        let root = std::env::temp_dir().join(format!(
            "axe-paths-explicit-{}-{}",
            std::process::id(),
            NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create selector fixture");
        let blocker = root.join("blocker");
        fs::write(&blocker, b"not a directory").expect("create explicit blocker");
        let automatic = root.join("automatic");

        let result = select_runtime_root_from(Some(blocker), None, vec![automatic.clone()]);

        assert!(result.is_err());
        assert!(!automatic.exists());
        fs::remove_dir_all(root).expect("remove selector fixture");
    }

    #[test]
    fn inherited_runtime_root_precedes_automatic_candidates() {
        let root = std::env::temp_dir().join(format!(
            "axe-paths-inherited-{}-{}",
            std::process::id(),
            NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create selector fixture");
        let inherited = root.join("inherited");
        let automatic = root.join("automatic");

        let selected =
            select_runtime_root_from(None, Some(inherited.clone()), vec![automatic.clone()])
                .expect("select inherited root");

        assert_eq!(
            selected,
            inherited.canonicalize().expect("canonical inherited root")
        );
        assert!(!automatic.exists());
        fs::remove_dir_all(root).expect("remove selector fixture");
    }

    #[cfg(unix)]
    #[test]
    fn relative_automatic_roots_are_ignored() {
        let roots = automatic_runtime_roots_from(
            Some(PathBuf::from("cache")),
            Some(PathBuf::from("home")),
            Some(PathBuf::from("runtime")),
            PathBuf::from("tmp"),
        );

        assert_eq!(
            roots,
            [PathBuf::from(format!("/tmp/axe-{}", effective_user_id()))]
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_runtime_root_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = std::env::temp_dir().join(format!(
            "axe-paths-private-{}-{}",
            std::process::id(),
            NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create selector fixture");
        let runtime = root.join("runtime");

        let selected = select_runtime_root_from(None, None, vec![runtime.clone()])
            .expect("select automatic runtime root");

        assert_eq!(
            selected,
            runtime.canonicalize().expect("canonical runtime root")
        );
        assert_eq!(
            fs::metadata(&runtime)
                .expect("read runtime root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        fs::remove_dir_all(root).expect("remove selector fixture");
    }
}
