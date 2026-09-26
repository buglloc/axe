#[cfg(target_os = "linux")]
use memmap2::Mmap;
#[cfg(target_os = "linux")]
use object::Object as _;
#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
#[cfg(target_os = "linux")]
use std::fmt::Write as _;
use std::fs::{self, Metadata};
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(target_os = "linux")]
use std::io::Write as _;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Output;
use std::process::{Child, Command as StdCommand, ExitStatus, Stdio};
use std::sync::{Arc, LazyLock, OnceLock};

#[cfg(target_os = "linux")]
use std::ffi::CStr;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt as _;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

#[cfg(not(target_os = "linux"))]
use brush_shell::bundled::BundledExecutable;
#[cfg(target_os = "linux")]
use brush_shell::bundled::{
    BundledExecutable, DescriptorExecutable, INHERITED_EXECUTABLE_FALLBACK,
    INHERITED_EXECUTABLE_FD, INHERITED_RUNTIME_ROOT,
};
use serde::Serialize;
use vzik::environment::{Failure, Observation};

static EXECUTABLE: LazyLock<Arc<Executable>> = LazyLock::new(|| Arc::new(Executable::discover()));
#[cfg(target_os = "linux")]
static NEXT_RELAY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(target_os = "linux")]
const MAX_RELAY_IMAGES: usize = 2;
#[cfg(target_os = "linux")]
const RELAY_DIRECTORY_NAME: &str = ".axe-self";
#[cfg(target_os = "linux")]
const NO_LOCK_RELAY_PREFIX: &str = "nolock-";
const NOT_FOUND: &str = "cannot locate AXE executable for re-exec";
pub(crate) const RELAY_PROBE_FLAG: &str = "--axe-internal-self-exec-probe";
pub(crate) const DOCTOR_PROBE_FLAG: &str = "--axe-internal-doctor-probe";

#[derive(Clone, Debug)]
pub(crate) struct ExecutableStartupFacts {
    pub argv0: OsString,
    pub startup_directory: Result<PathBuf, Failure>,
    pub discovery_failures: Vec<Failure>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecutableCandidate {
    pub kind: &'static str,
    pub source: &'static str,
    pub path: Option<PathBuf>,
    pub usable: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum RelayObservation {
    NotAttempted,
    #[cfg(target_os = "linux")]
    Available(PathBuf),
    #[cfg(target_os = "linux")]
    Failed(Failure),
}

impl RelayObservation {
    pub(crate) fn failure(&self) -> Option<&Failure> {
        match self {
            Self::NotAttempted => None,
            #[cfg(target_os = "linux")]
            Self::Available(_) => None,
            #[cfg(target_os = "linux")]
            Self::Failed(failure) => Some(failure),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ExecutableObservation {
    pub candidates: Vec<ExecutableCandidate>,
    pub relay: RelayObservation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SelfExecProbe {
    pub success: bool,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn matches(self, metadata: &Metadata) -> bool {
        self == Self::from_metadata(metadata)
    }

    fn matches_path(self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|metadata| self.matches(&metadata))
    }
}

struct FilesystemExecutable {
    path: PathBuf,
    identity: FileIdentity,
    #[cfg(target_os = "linux")]
    descriptor: Option<File>,
}

#[cfg(target_os = "linux")]
struct RetainedExecutable {
    descriptor: File,
    identity: FileIdentity,
}

#[cfg(target_os = "linux")]
struct ProcExecutable {
    retained: RetainedExecutable,
    reexec_path: PathBuf,
    bridge_path: PathBuf,
}

/// Owns every capability for locating and launching the running AXE image.
pub(crate) struct Executable {
    startup_facts: ExecutableStartupFacts,
    requested_runtime_root: Option<PathBuf>,
    inherited_runtime_root: Option<PathBuf>,
    runtime_root: OnceLock<Result<PathBuf, Failure>>,
    #[cfg(target_os = "linux")]
    relay_fallback_roots: Vec<PathBuf>,
    #[cfg(target_os = "linux")]
    inherited: Option<RetainedExecutable>,
    #[cfg(target_os = "linux")]
    proc: Option<ProcExecutable>,
    filesystem: Option<FilesystemExecutable>,
    #[cfg(target_os = "linux")]
    relay: OnceLock<Result<FilesystemExecutable, Failure>>,
}

impl Executable {
    fn discover() -> Self {
        let startup_directory = std::env::current_dir()
            .map_err(|error| Failure::from_io("read startup current directory", &error));
        let startup_directory_path = startup_directory.as_deref().ok();
        let argv0 = std::env::args_os()
            .next()
            .unwrap_or_else(|| OsString::from("axe"));
        let mut discovery_failures = Vec::new();
        let requested_runtime_root = std::env::var_os("AXE_WORK_DIR")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        #[cfg(target_os = "linux")]
        let inherited_runtime_root = take_inherited_runtime_root();
        #[cfg(not(target_os = "linux"))]
        let inherited_runtime_root = None;
        #[cfg(target_os = "linux")]
        let relay_fallback_roots = axe_paths::runtime_relay_fallback_roots();
        #[cfg(target_os = "linux")]
        let mut inherited = inherited_executable();
        #[cfg(target_os = "linux")]
        let proc = ProcExecutable::open();
        #[cfg(target_os = "linux")]
        if inherited.as_ref().is_some_and(|inherited| {
            proc.as_ref()
                .is_some_and(|proc| inherited.identity != proc.retained.identity)
        }) {
            inherited = None;
        }
        #[cfg(target_os = "linux")]
        let inherited_runtime_root = inherited
            .is_some()
            .then_some(inherited_runtime_root)
            .flatten();
        #[cfg(target_os = "linux")]
        let required_identity = proc
            .as_ref()
            .map(|executable| executable.retained.identity)
            .or_else(|| inherited.as_ref().map(|executable| executable.identity));
        #[cfg(not(target_os = "linux"))]
        let required_identity = None;

        let filesystem = filesystem_candidates(startup_directory_path).find_map(|path| {
            match FilesystemExecutable::open(&path, required_identity) {
                Some(executable) => Some(executable),
                None => {
                    discovery_failures.push(Failure {
                        operation: format!("validate executable candidate {}", path.display()),
                        class: "candidate_rejected".into(),
                        code: None,
                        errno: None,
                        message: "candidate is missing, non-executable, or names a different image"
                            .into(),
                    });
                    None
                }
            }
        });

        #[cfg(target_os = "linux")]
        let relay = {
            let relay = OnceLock::new();
            if let Some(path) = inherited_fallback_path() {
                match FilesystemExecutable::open(&path, None) {
                    Some(executable) => {
                        let _ = relay.set(Ok(executable));
                    }
                    None => discovery_failures.push(Failure {
                        operation: format!("validate inherited relay {}", path.display()),
                        class: "candidate_rejected".into(),
                        code: None,
                        errno: None,
                        message: "inherited relay is not a usable executable".into(),
                    }),
                }
            }
            relay
        };

        Self {
            startup_facts: ExecutableStartupFacts {
                argv0,
                startup_directory,
                discovery_failures,
            },
            requested_runtime_root,
            inherited_runtime_root,
            runtime_root: OnceLock::new(),
            #[cfg(target_os = "linux")]
            relay_fallback_roots,
            #[cfg(target_os = "linux")]
            inherited,
            #[cfg(target_os = "linux")]
            proc,
            filesystem,
            #[cfg(target_os = "linux")]
            relay,
        }
    }

    /// Returns a conventional path when one still names a usable AXE image.
    #[cfg(test)]
    pub(crate) fn reexec_path(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        if let Some(path) = self.proc_reexec_path() {
            return Some(path);
        }

        self.filesystem_path().or_else(|| {
            #[cfg(target_os = "linux")]
            {
                self.relay_path()
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        })
    }

    /// Returns the canonical original filesystem path while it still names the
    /// same inode.
    pub(crate) fn filesystem_path(&self) -> Option<PathBuf> {
        self.filesystem
            .as_ref()
            .and_then(FilesystemExecutable::checked_path)
    }

    /// Returns a conventional path selected for child-process publication.
    pub(crate) fn bridge_path(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            if let Some(path) = self.selected_path_fallback(true) {
                return Some(path);
            }
            if let Some(proc) = &self.proc
                && proc.retained.identity.matches_path(&proc.bridge_path)
            {
                return Some(proc.bridge_path.clone());
            }
            None
        }
        #[cfg(not(target_os = "linux"))]
        self.filesystem_path()
    }

    /// Reports whether `path` currently resolves to one of this owner's images.
    pub(crate) fn matches(&self, path: &Path) -> bool {
        let Ok(metadata) = fs::metadata(path) else {
            return false;
        };

        #[cfg(target_os = "linux")]
        if self
            .retained()
            .is_some_and(|retained| retained.identity.matches(&metadata))
        {
            return true;
        }

        if self.filesystem.as_ref().is_some_and(|filesystem| {
            filesystem.identity.matches_path(&filesystem.path)
                && filesystem.identity.matches(&metadata)
        }) {
            return true;
        }

        #[cfg(target_os = "linux")]
        {
            self.relay.get().is_some_and(|relay| {
                relay.as_ref().is_ok_and(|relay| {
                    relay.identity.matches_path(&relay.path) && relay.identity.matches(&metadata)
                })
            })
        }
        #[cfg(not(target_os = "linux"))]
        false
    }

    /// Whether at least one process launch backend is currently usable.
    pub(crate) fn can_reexec(&self) -> bool {
        #[cfg(target_os = "linux")]
        if self.proc_reexec_path().is_some() || self.retained().is_some() {
            return true;
        }

        self.filesystem_path().is_some()
    }

    pub(crate) fn startup_facts(&self) -> &ExecutableStartupFacts {
        &self.startup_facts
    }

    pub(crate) fn runtime_root(&self) -> Result<&Path, Failure> {
        self.runtime_root
            .get_or_init(|| {
                axe_paths::select_runtime_root(
                    self.requested_runtime_root.clone(),
                    self.inherited_runtime_root.clone(),
                )
                .map_err(|error| Failure::from_io("select AXE runtime root", &error))
            })
            .as_ref()
            .map(PathBuf::as_path)
            .map_err(Clone::clone)
    }

    /// Takes a strictly passive snapshot. It never initializes the lazy relay.
    pub(crate) fn observe(&self) -> ExecutableObservation {
        let mut candidates = Vec::new();
        #[cfg(target_os = "linux")]
        {
            candidates.push(ExecutableCandidate {
                kind: "procfd_path",
                source: "procfs",
                path: self.proc.as_ref().map(|proc| proc.reexec_path.clone()),
                usable: self.proc_reexec_path().is_some(),
            });
            candidates.push(ExecutableCandidate {
                kind: "retained_descriptor",
                source: if self.proc.is_some() {
                    "procfs"
                } else if self.inherited.is_some() {
                    "inherited"
                } else {
                    "filesystem"
                },
                path: None,
                usable: self.retained().is_some(),
            });
        }
        candidates.push(ExecutableCandidate {
            kind: "filesystem_path",
            source: "startup_discovery",
            path: self.filesystem.as_ref().map(|value| value.path.clone()),
            usable: self.filesystem_path().is_some(),
        });
        #[cfg(target_os = "linux")]
        candidates.push(ExecutableCandidate {
            kind: "lazy_relay",
            source: "runtime_filesystem",
            path: self
                .relay
                .get()
                .and_then(|result| result.as_ref().ok())
                .map(|relay| relay.path.clone()),
            usable: self.relay.get().is_some_and(|result| {
                result
                    .as_ref()
                    .is_ok_and(|relay| relay.checked_path().is_some())
            }),
        });

        #[cfg(target_os = "linux")]
        let relay = match self.relay.get() {
            None => RelayObservation::NotAttempted,
            Some(Ok(relay)) => relay.checked_path().map_or_else(
                || {
                    RelayObservation::Failed(Failure {
                        operation: "revalidate executable relay".into(),
                        class: "stable_restriction".into(),
                        code: None,
                        errno: None,
                        message: "relay path no longer names the retained image".into(),
                    })
                },
                RelayObservation::Available,
            ),
            Some(Err(failure)) => RelayObservation::Failed(failure.clone()),
        };
        #[cfg(not(target_os = "linux"))]
        let relay = RelayObservation::NotAttempted;

        ExecutableObservation { candidates, relay }
    }

    pub(crate) fn probe_self_exec(&self) -> Observation<SelfExecProbe> {
        let mut command = match self.command() {
            Ok(command) => command,
            Err(error) => {
                return Observation::unavailable(Failure::from_io(
                    "construct AXE self-exec command",
                    &error,
                ));
            }
        };

        match command.arg(DOCTOR_PROBE_FLAG).status() {
            Ok(status) => Observation::available(SelfExecProbe {
                success: status.success(),
                exit_code: status.code(),
            }),
            Err(error) => {
                Observation::unavailable(Failure::from_io("spawn AXE self-exec probe", &error))
            }
        }
    }

    /// Creates Brush's descriptor-aware launch capability.
    ///
    /// The capability is rebuilt immediately before each bundled launch. This
    /// revalidates the original path and can materialize a relay if the path was
    /// removed or resides on a `noexec` mount.
    pub(crate) fn bundled_executable(&self) -> Option<BundledExecutable> {
        #[cfg(target_os = "linux")]
        if let Some(descriptor) = self.descriptor_executable() {
            let fallback_path = descriptor.fallback_path().map(Path::to_owned);
            return Some(BundledExecutable::Descriptor {
                descriptor,
                fallback_path,
            });
        }

        self.publishable_filesystem_path()
            .map(BundledExecutable::Path)
    }

    pub(crate) fn command(&self) -> io::Result<ExecutableCommand> {
        #[cfg(target_os = "linux")]
        if let Some(descriptor) = self.descriptor_executable() {
            return Ok(ExecutableCommand::descriptor(descriptor));
        }

        self.publishable_filesystem_path()
            .map(ExecutableCommand::path)
            .ok_or_else(executable_not_found)
    }

    #[cfg(feature = "applet-daemons")]
    pub(crate) fn tokio_command(&self) -> io::Result<TokioExecutableCommand> {
        self.command().map(TokioExecutableCommand::new)
    }

    #[cfg(target_os = "linux")]
    fn proc_reexec_path(&self) -> Option<PathBuf> {
        let proc = self.proc.as_ref()?;
        proc.retained
            .identity
            .matches_path(&proc.reexec_path)
            .then(|| proc.reexec_path.clone())
    }

    #[cfg(target_os = "linux")]
    fn retained(&self) -> Option<RetainedExecutableRef<'_>> {
        self.proc
            .as_ref()
            .map(|proc| proc.retained.as_ref())
            .or_else(|| self.inherited.as_ref().map(RetainedExecutable::as_ref))
            .or_else(|| {
                self.filesystem
                    .as_ref()
                    .and_then(FilesystemExecutable::retained_descriptor)
            })
    }

    #[cfg(target_os = "linux")]
    fn descriptor_executable(&self) -> Option<DescriptorExecutable> {
        let retained = self.retained()?;
        let descriptor: OwnedFd = retained.descriptor.try_clone().ok()?.into();
        let fallback = self.selected_path_fallback(true);
        let runtime_root = self.runtime_root().ok().map(Path::to_owned);

        DescriptorExecutable::new(descriptor, fallback, runtime_root).ok()
    }

    fn publishable_filesystem_path(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            self.selected_path_fallback(true)
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.filesystem_path()
        }
    }

    #[cfg(target_os = "linux")]
    fn selected_path_fallback(&self, materialize_relay: bool) -> Option<PathBuf> {
        let original = self.filesystem_path();
        let policy = original.as_deref().map(axe_paths::mount_execution_policy);
        select_path_fallback(original, policy, || {
            if materialize_relay {
                self.relay_path()
            } else {
                self.observed_relay_path()
            }
        })
    }

    #[cfg(target_os = "linux")]
    fn observed_relay_path(&self) -> Option<PathBuf> {
        self.relay
            .get()
            .and_then(|relay| relay.as_ref().ok())
            .and_then(FilesystemExecutable::checked_path)
    }

    #[cfg(target_os = "linux")]
    fn relay_path(&self) -> Option<PathBuf> {
        self.relay
            .get_or_init(|| {
                let retained = self.retained().ok_or_else(|| Failure {
                    operation: "materialize executable relay".into(),
                    class: "unavailable".into(),
                    code: None,
                    errno: None,
                    message: "no retained executable descriptor is available".into(),
                })?;
                let roots = self.relay_roots()?;
                materialize_relay(retained.descriptor, roots)
            })
            .as_ref()
            .ok()
            .and_then(FilesystemExecutable::checked_path)
    }

    #[cfg(target_os = "linux")]
    fn relay_roots(&self) -> Result<Vec<PathBuf>, Failure> {
        let mut roots = vec![self.runtime_root()?.to_owned()];
        if self.requested_runtime_root.is_none() {
            for root in &self.relay_fallback_roots {
                if !roots.contains(root) {
                    roots.push(root.clone());
                }
            }
        }
        Ok(roots)
    }

    #[cfg(test)]
    pub(crate) fn from_filesystem_path(path: &Path) -> io::Result<Self> {
        let filesystem = FilesystemExecutable::open(path, None)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid AXE executable"))?;

        Ok(Self {
            startup_facts: test_startup_facts(),
            requested_runtime_root: None,
            inherited_runtime_root: None,
            runtime_root: OnceLock::new(),
            #[cfg(target_os = "linux")]
            relay_fallback_roots: Vec::new(),
            #[cfg(target_os = "linux")]
            inherited: None,
            #[cfg(target_os = "linux")]
            proc: None,
            filesystem: Some(filesystem),
            #[cfg(target_os = "linux")]
            relay: OnceLock::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn unavailable() -> Self {
        Self {
            startup_facts: test_startup_facts(),
            requested_runtime_root: None,
            inherited_runtime_root: None,
            runtime_root: OnceLock::new(),
            #[cfg(target_os = "linux")]
            relay_fallback_roots: Vec::new(),
            #[cfg(target_os = "linux")]
            inherited: None,
            #[cfg(target_os = "linux")]
            proc: None,
            filesystem: None,
            #[cfg(target_os = "linux")]
            relay: OnceLock::new(),
        }
    }
}
#[cfg(target_os = "linux")]
fn select_path_fallback(
    original: Option<PathBuf>,
    policy: Option<axe_paths::MountExecutionPolicy>,
    relay: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    match policy {
        Some(axe_paths::MountExecutionPolicy::Supported) => original,
        Some(axe_paths::MountExecutionPolicy::NoExec) | None => relay(),
        Some(
            axe_paths::MountExecutionPolicy::Unknown | axe_paths::MountExecutionPolicy::Unsupported,
        ) => relay().or(original),
    }
}

#[cfg(test)]
fn test_startup_facts() -> ExecutableStartupFacts {
    ExecutableStartupFacts {
        argv0: OsString::from("axe"),
        startup_directory: std::env::current_dir()
            .map_err(|error| Failure::from_io("read test current directory", &error)),
        discovery_failures: Vec::new(),
    }
}

#[cfg(all(test, target_os = "linux"))]
fn disabled_relay_failure() -> Failure {
    Failure {
        operation: "test-disabled relay".into(),
        class: "not_applicable".into(),
        code: None,
        errno: None,
        message: "relay disabled by test fixture".into(),
    }
}

impl FilesystemExecutable {
    fn open(path: &Path, required_identity: Option<FileIdentity>) -> Option<Self> {
        let path = path.canonicalize().ok()?;
        let metadata = fs::metadata(&path).ok()?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }

        let identity = FileIdentity::from_metadata(&metadata);
        if required_identity.is_some_and(|required| required != identity) {
            return None;
        }

        Some(Self {
            #[cfg(target_os = "linux")]
            descriptor: File::open(&path).ok(),
            path,
            identity,
        })
    }

    fn checked_path(&self) -> Option<PathBuf> {
        self.identity
            .matches_path(&self.path)
            .then(|| self.path.clone())
    }
}

#[cfg(target_os = "linux")]
impl RetainedExecutable {
    fn as_ref(&self) -> RetainedExecutableRef<'_> {
        RetainedExecutableRef {
            descriptor: &self.descriptor,
            identity: self.identity,
        }
    }
}

#[cfg(target_os = "linux")]
impl FilesystemExecutable {
    fn retained_descriptor(&self) -> Option<RetainedExecutableRef<'_>> {
        self.descriptor
            .as_ref()
            .map(|descriptor| RetainedExecutableRef {
                descriptor,
                identity: self.identity,
            })
    }
}

#[cfg(target_os = "linux")]
struct RetainedExecutableRef<'a> {
    descriptor: &'a File,
    identity: FileIdentity,
}

#[cfg(target_os = "linux")]
impl ProcExecutable {
    fn open() -> Option<Self> {
        let descriptor = File::open("/proc/self/exe").ok()?;
        let metadata = descriptor.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }

        let identity = FileIdentity::from_metadata(&metadata);
        let reexec_path = PathBuf::from(format!("/proc/self/fd/{}", descriptor.as_raw_fd()));
        if !identity.matches_path(&reexec_path) {
            return None;
        }

        Some(Self {
            retained: RetainedExecutable {
                descriptor,
                identity,
            },
            reexec_path,
            bridge_path: PathBuf::from(format!("/proc/{}/exe", std::process::id())),
        })
    }

    #[cfg(test)]
    fn from_file(file: File, bridge_path: PathBuf) -> io::Result<Self> {
        let identity = FileIdentity::from_metadata(&file.metadata()?);
        let reexec_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));

        Ok(Self {
            retained: RetainedExecutable {
                descriptor: file,
                identity,
            },
            reexec_path,
            bridge_path,
        })
    }
}

pub(crate) fn initialize() -> Arc<Executable> {
    current()
}

pub(crate) fn current() -> Arc<Executable> {
    Arc::clone(&EXECUTABLE)
}

pub(crate) fn is_relay_probe(args: &[OsString]) -> bool {
    args == [OsStr::new(RELAY_PROBE_FLAG)]
}

pub(crate) fn is_doctor_probe(args: &[OsString]) -> bool {
    args == [OsStr::new(DOCTOR_PROBE_FLAG)]
}

fn filesystem_candidates(startup_directory: Option<&Path>) -> impl Iterator<Item = PathBuf> {
    let current = std::env::current_exe()
        .ok()
        .and_then(|path| absolutize(path, startup_directory));

    #[cfg(target_os = "linux")]
    let execfn = linux_execfn().and_then(|path| absolutize(path, startup_directory));
    #[cfg(not(target_os = "linux"))]
    let execfn = None;

    current.into_iter().chain(execfn)
}

fn absolutize(path: PathBuf, startup_directory: Option<&Path>) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path)
    } else {
        Some(startup_directory?.join(path))
    }
}

#[cfg(target_os = "linux")]
fn inherited_executable() -> Option<RetainedExecutable> {
    let value = std::env::var_os(INHERITED_EXECUTABLE_FD);
    // SAFETY: initialization runs before AXE starts any threads.
    unsafe { std::env::remove_var(INHERITED_EXECUTABLE_FD) };
    let descriptor = value?.to_str()?.parse::<libc::c_int>().ok()?;
    if descriptor <= libc::STDERR_FILENO {
        return None;
    }

    // SAFETY: F_GETFD validates that the inherited integer still names a live fd.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags == -1 {
        return None;
    }
    // SAFETY: descriptor is live and was deliberately transferred to this AXE
    // process. From this point the returned File owns and eventually closes it.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return None;
    }
    // SAFETY: file owns a live descriptor. Restoring CLOEXEC prevents accidental
    // leakage through path-based launches; descriptor launches clear it again.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return None;
    }

    Some(RetainedExecutable {
        identity: FileIdentity::from_metadata(&metadata),
        descriptor: file,
    })
}

#[cfg(target_os = "linux")]
fn inherited_fallback_path() -> Option<PathBuf> {
    let path = std::env::var_os(INHERITED_EXECUTABLE_FALLBACK).map(PathBuf::from);
    // SAFETY: initialization runs before AXE starts any threads.
    unsafe { std::env::remove_var(INHERITED_EXECUTABLE_FALLBACK) };
    path
}

#[cfg(target_os = "linux")]
fn take_inherited_runtime_root() -> Option<PathBuf> {
    let path = std::env::var_os(INHERITED_RUNTIME_ROOT)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    // SAFETY: initialization runs before AXE starts any threads.
    unsafe { std::env::remove_var(INHERITED_RUNTIME_ROOT) };
    path
}

#[cfg(target_os = "linux")]
fn linux_execfn() -> Option<PathBuf> {
    // SAFETY: AT_EXECFN is supplied by the Linux kernel as a process-lifetime,
    // NUL-terminated byte string. A null result means the entry is unavailable.
    let pointer = unsafe { libc::getauxval(libc::AT_EXECFN) as *const libc::c_char };
    if pointer.is_null() {
        return None;
    }

    // SAFETY: the non-null AT_EXECFN pointer has the kernel-owned lifetime and
    // NUL termination described above; CStr only reads through that terminator.
    let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
    if bytes.is_empty() {
        return None;
    }

    Some(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(target_os = "linux")]
fn materialize_relay(source: &File, roots: Vec<PathBuf>) -> Result<FilesystemExecutable, Failure> {
    let source_metadata = source
        .metadata()
        .map_err(|error| Failure::from_io("read retained executable metadata", &error))?;
    let build_id = elf_build_id(source)
        .map_err(|error| Failure::from_io("read retained executable build ID", &error))?;
    let size = source_metadata.len();
    let name = relay_name(&build_id);
    let mut last_failure = None;

    for root in roots {
        if let Err(error) = axe_paths::ensure_writable_root(&root, size) {
            last_failure = Some(Failure::from_io(
                format!("prepare relay root {}", root.display()),
                &error,
            ));
            continue;
        }

        let base = root.join(RELAY_DIRECTORY_NAME);
        if let Err(error) = crate::path_bridge::ensure_private_directory(&base) {
            last_failure = Some(Failure::from_io(
                format!("prepare relay directory {}", base.display()),
                &error,
            ));
            continue;
        }
        let base = base.canonicalize().unwrap_or(base);
        let directory_lock = match acquire_relay_directory_lock(&base) {
            Ok(lock) => lock,
            Err(error) => {
                last_failure = Some(Failure::from_io(
                    format!("lock relay directory {}", base.display()),
                    &error,
                ));
                continue;
            }
        };
        let locking_supported = directory_lock.is_some();
        let mut path = if locking_supported {
            base.join(&name)
        } else {
            base.join(format!("{NO_LOCK_RELAY_PREFIX}{name}"))
        };
        if let Some(executable) = open_relay(&path, size, &build_id, locking_supported) {
            if relay_usable(&path) {
                if locking_supported {
                    let _ = prune_relays(&base, &path);
                }
                return Ok(executable);
            }
            last_failure = Some(Failure {
                operation: format!("validate executable relay {}", path.display()),
                class: "stable_restriction".into(),
                code: None,
                errno: None,
                message: "existing relay could not execute the AXE probe child".into(),
            });
            continue;
        }

        let nonce = NEXT_RELAY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match remove_unused_relay(&path, locking_supported) {
            Ok(true) => {}
            Ok(false) if !locking_supported => {
                path = base.join(format!(
                    "{NO_LOCK_RELAY_PREFIX}{name}-{}-{nonce}",
                    std::process::id()
                ));
            }
            Ok(false) => {
                last_failure = Some(Failure {
                    operation: format!("replace executable relay {}", path.display()),
                    class: "stable_restriction".into(),
                    code: None,
                    errno: None,
                    message: "existing relay is invalid and may still be in use".into(),
                });
                continue;
            }
            Err(error) => {
                last_failure = Some(Failure::from_io(
                    format!("remove invalid executable relay {}", path.display()),
                    &error,
                ));
                continue;
            }
        }

        let temporary = if locking_supported {
            base.join(format!(".axe-{}-{nonce}.tmp", std::process::id()))
        } else {
            base.join(format!(".nolock-{}-{nonce}.tmp", std::process::id()))
        };
        if let Err(error) = write_relay(source, &temporary, &source_metadata) {
            let _ = fs::remove_file(&temporary);
            last_failure = Some(Failure::from_io(
                format!("write executable relay {}", temporary.display()),
                &error,
            ));
            continue;
        }
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {
                let _ = fs::remove_file(&temporary);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temporary);
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                last_failure = Some(Failure::from_io(
                    format!("publish executable relay {}", path.display()),
                    &error,
                ));
                continue;
            }
        }

        if let Some(executable) = open_relay(&path, size, &build_id, locking_supported)
            && relay_usable(&path)
        {
            if locking_supported {
                let _ = prune_relays(&base, &path);
            }
            return Ok(executable);
        }
        last_failure = Some(Failure {
            operation: format!("execute relay probe {}", path.display()),
            class: "stable_restriction".into(),
            code: None,
            errno: None,
            message: "relay could not execute the AXE probe child".into(),
        });
    }

    Err(last_failure.unwrap_or_else(|| Failure {
        operation: "materialize executable relay".into(),
        class: "unavailable".into(),
        code: None,
        errno: None,
        message: "no relay root candidates were available".into(),
    }))
}

#[cfg(target_os = "linux")]
fn write_relay(source: &File, path: &Path, expected: &Metadata) -> io::Result<()> {
    let size = expected.len();
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(path)?;
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < size {
        let count = source.read_at(&mut buffer, offset)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "AXE executable changed while creating relay",
            ));
        }
        target.write_all(&buffer[..count])?;
        offset = offset.saturating_add(count as u64);
    }
    if !same_relay_source(expected, &source.metadata()?) {
        return Err(io::Error::other(
            "AXE executable changed while creating relay",
        ));
    }
    target.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o500))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn relay_name(build_id: &[u8]) -> String {
    let mut name = String::with_capacity(build_id.len() * 2);
    for byte in build_id {
        write!(name, "{byte:02x}").expect("writing to a String cannot fail");
    }
    name
}

#[cfg(target_os = "linux")]
fn same_relay_source(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

#[cfg(target_os = "linux")]
fn elf_build_id(file: &File) -> io::Result<Vec<u8>> {
    // SAFETY: the mapping is read-only and the executable descriptor remains open
    // for the lifetime of the mapping. Running executables cannot be truncated.
    let bytes = unsafe { Mmap::map(file) }?;
    let object = object::File::parse(&*bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let build_id = object
        .build_id()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
        .filter(|build_id| !build_id.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ELF build ID is missing"))?;
    Ok(build_id.to_vec())
}

#[cfg(target_os = "linux")]
fn open_relay(
    path: &Path,
    expected_size: u64,
    expected_build_id: &[u8],
    acquire_lease: bool,
) -> Option<FilesystemExecutable> {
    let descriptor = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = descriptor.metadata().ok()?;
    if !metadata.is_file()
        || metadata.len() != expected_size
        || metadata.permissions().mode() & 0o111 == 0
    {
        return None;
    }
    if acquire_lease && lock_relay_shared(&descriptor).is_err() {
        return None;
    }

    let identity = FileIdentity::from_metadata(&metadata);
    if !identity.matches_path(path)
        || elf_build_id(&descriptor).ok().as_deref() != Some(expected_build_id)
    {
        return None;
    }

    Some(FilesystemExecutable {
        path: path.to_owned(),
        identity,
        descriptor: Some(descriptor),
    })
}

#[cfg(target_os = "linux")]
fn acquire_relay_directory_lock(base: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(base.join(".lock"))?;
    // SAFETY: file owns a live descriptor and flock does not access Rust memory.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
        return Ok(Some(file));
    }

    let error = io::Error::last_os_error();
    if lock_unavailable(&error) {
        Ok(None)
    } else {
        Err(error)
    }
}

#[cfg(target_os = "linux")]
fn lock_relay_shared(file: &File) -> io::Result<()> {
    // SAFETY: file owns a live descriptor and flock does not access Rust memory.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn try_lock_relay_exclusive(file: &File) -> io::Result<bool> {
    // SAFETY: file owns a live descriptor and flock does not access Rust memory.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(false),
        _ => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn lock_unavailable(error: &io::Error) -> bool {
    match error.raw_os_error() {
        Some(code) => matches!(
            code,
            libc::ENOSYS | libc::EOPNOTSUPP | libc::ENOLCK | libc::EPERM | libc::EACCES
        ),
        None => false,
    }
}

#[cfg(target_os = "linux")]
fn remove_unused_relay(path: &Path, locking_supported: bool) -> io::Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    if !locking_supported {
        return Ok(false);
    }
    let metadata = file.metadata()?;
    if !metadata.is_file() || !try_lock_relay_exclusive(&file)? {
        return Ok(false);
    }

    let identity = FileIdentity::from_metadata(&metadata);
    if !identity.matches_path(path) {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}

#[cfg(target_os = "linux")]
fn prune_relays(base: &Path, current: &Path) -> io::Result<()> {
    let mut obsolete = Vec::new();
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(".axe-") && name.ends_with(".tmp") {
            let _ = fs::remove_file(entry.path());
            continue;
        }
        if !managed_relay_name(name) || entry.path() == current {
            continue;
        }

        let metadata = entry.metadata()?;
        if metadata.is_file() {
            obsolete.push((
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                entry.path(),
            ));
        }
    }

    obsolete.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, path) in obsolete
        .into_iter()
        .skip(MAX_RELAY_IMAGES.saturating_sub(1))
    {
        let _ = remove_unused_relay(&path, true);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn managed_relay_name(name: &str) -> bool {
    name.len() == 40 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(target_os = "linux")]
fn relay_usable(path: &Path) -> bool {
    StdCommand::new(path)
        .arg(RELAY_PROBE_FLAG)
        .env_remove(INHERITED_EXECUTABLE_FD)
        .env_remove(INHERITED_EXECUTABLE_FALLBACK)
        .env_remove(INHERITED_RUNTIME_ROOT)
        .status()
        .is_ok_and(|status| status.success())
}

fn executable_not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, NOT_FOUND)
}

/// A std process command that installs descriptor exec only after every caller
/// pre-exec action, redirection, cwd, process-group, and environment change.
pub(crate) struct ExecutableCommand {
    inner: StdCommand,
    #[cfg(target_os = "linux")]
    descriptor: Option<DescriptorExecutable>,
    #[cfg(target_os = "linux")]
    argv0: OsString,
    #[cfg(target_os = "linux")]
    environment: BTreeMap<OsString, OsString>,
    #[cfg(target_os = "linux")]
    prepared: bool,
}

impl std::fmt::Debug for ExecutableCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(formatter)
    }
}

impl ExecutableCommand {
    fn path(path: PathBuf) -> Self {
        Self {
            inner: StdCommand::new(&path),
            #[cfg(target_os = "linux")]
            descriptor: None,
            #[cfg(target_os = "linux")]
            argv0: path.into_os_string(),
            #[cfg(target_os = "linux")]
            environment: BTreeMap::new(),
            #[cfg(target_os = "linux")]
            prepared: false,
        }
    }

    #[cfg(target_os = "linux")]
    fn descriptor(descriptor: DescriptorExecutable) -> Self {
        let program = descriptor
            .fallback_path()
            .unwrap_or_else(|| Path::new("/dev/null"))
            .to_owned();

        Self {
            inner: StdCommand::new(&program),
            descriptor: Some(descriptor),
            argv0: program.into_os_string(),
            environment: BTreeMap::new(),
            prepared: false,
        }
    }

    pub(crate) fn arg(&mut self, argument: impl AsRef<OsStr>) -> &mut Self {
        self.inner.arg(argument);
        self
    }

    pub(crate) fn args<I, S>(&mut self, arguments: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.inner.args(arguments);
        self
    }

    pub(crate) fn env(&mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        let name = name.as_ref().to_owned();
        let value = value.as_ref().to_owned();
        #[cfg(target_os = "linux")]
        if self.descriptor.is_some() {
            self.environment.insert(name, value);
            return self;
        }
        self.inner.env(name, value);
        self
    }

    pub(crate) fn current_dir(&mut self, directory: impl AsRef<Path>) -> &mut Self {
        self.inner.current_dir(directory);
        self
    }

    pub(crate) fn stdin(&mut self, configuration: Stdio) -> &mut Self {
        self.inner.stdin(configuration);
        self
    }

    pub(crate) fn stdout(&mut self, configuration: Stdio) -> &mut Self {
        self.inner.stdout(configuration);
        self
    }

    pub(crate) fn stderr(&mut self, configuration: Stdio) -> &mut Self {
        self.inner.stderr(configuration);
        self
    }

    #[cfg(unix)]
    pub(crate) unsafe fn pre_exec<F>(&mut self, callback: F) -> &mut Self
    where
        F: FnMut() -> io::Result<()> + Send + Sync + 'static,
    {
        // SAFETY: the caller accepts CommandExt::pre_exec's after-fork contract.
        unsafe { self.inner.pre_exec(callback) };
        self
    }

    pub(crate) fn spawn(&mut self) -> io::Result<Child> {
        self.prepare()?;
        self.inner.spawn()
    }

    pub(crate) fn status(&mut self) -> io::Result<ExitStatus> {
        self.prepare()?;
        self.inner.status()
    }

    fn prepare(&mut self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        let Some(descriptor) = self.descriptor.as_ref() else {
            return Ok(());
        };
        #[cfg(not(target_os = "linux"))]
        return Ok(());

        #[cfg(target_os = "linux")]
        {
            if self.prepared {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "descriptor command cannot be launched more than once",
                ));
            }

            let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
            environment.extend(
                self.environment
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
            environment.remove(OsStr::new(INHERITED_EXECUTABLE_FD));
            environment.remove(OsStr::new(INHERITED_EXECUTABLE_FALLBACK));
            environment.remove(OsStr::new(INHERITED_RUNTIME_ROOT));
            self.inner.env_clear().envs(environment);
            descriptor.prepare_command(&mut self.inner, &self.argv0)?;
            self.prepared = true;
            Ok(())
        }
    }

    #[cfg(feature = "applet-daemons")]
    fn into_prepared(mut self) -> io::Result<StdCommand> {
        self.prepare()?;
        Ok(self.inner)
    }
}

#[cfg(feature = "applet-daemons")]
pub(crate) struct TokioExecutableCommand {
    command: Option<ExecutableCommand>,
    kill_on_drop: bool,
}

#[cfg(feature = "applet-daemons")]
impl TokioExecutableCommand {
    fn new(command: ExecutableCommand) -> Self {
        Self {
            command: Some(command),
            kill_on_drop: false,
        }
    }

    fn command(&mut self) -> &mut ExecutableCommand {
        self.command
            .as_mut()
            .expect("Tokio executable command is consumed only by spawn/output")
    }

    pub(crate) fn arg(&mut self, argument: impl AsRef<OsStr>) -> &mut Self {
        self.command().arg(argument);
        self
    }

    pub(crate) fn env(&mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.command().env(name, value);
        self
    }

    pub(crate) fn current_dir(&mut self, directory: impl AsRef<Path>) -> &mut Self {
        self.command().current_dir(directory);
        self
    }

    pub(crate) fn stdin(&mut self, configuration: Stdio) -> &mut Self {
        self.command().stdin(configuration);
        self
    }

    pub(crate) fn stdout(&mut self, configuration: Stdio) -> &mut Self {
        self.command().stdout(configuration);
        self
    }

    pub(crate) fn stderr(&mut self, configuration: Stdio) -> &mut Self {
        self.command().stderr(configuration);
        self
    }

    pub(crate) fn kill_on_drop(&mut self, enabled: bool) -> &mut Self {
        self.kill_on_drop = enabled;
        self
    }

    #[cfg(unix)]
    pub(crate) unsafe fn pre_exec<F>(&mut self, callback: F) -> &mut Self
    where
        F: FnMut() -> io::Result<()> + Send + Sync + 'static,
    {
        // SAFETY: the caller accepts CommandExt::pre_exec's after-fork contract.
        unsafe { self.command().pre_exec(callback) };
        self
    }

    pub(crate) fn spawn(&mut self) -> io::Result<tokio::process::Child> {
        let command = self
            .command
            .take()
            .ok_or_else(|| io::Error::other("Tokio executable command already consumed"))?;
        let mut command = tokio::process::Command::from(command.into_prepared()?);
        command.kill_on_drop(self.kill_on_drop);
        command.spawn()
    }

    #[cfg(test)]
    pub(crate) async fn output(&mut self) -> io::Result<Output> {
        let command = self
            .command
            .take()
            .ok_or_else(|| io::Error::other("Tokio executable command already consumed"))?;
        let mut command = tokio::process::Command::from(command.into_prepared()?);
        command.kill_on_drop(self.kill_on_drop);
        command.output().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "axe-executable-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after epoch")
                    .as_nanos()
            ));
            fs::create_dir_all(&path).expect("create scratch directory");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn current_executable() -> PathBuf {
        std::env::current_exe()
            .expect("locate test executable")
            .canonicalize()
            .expect("canonicalize test executable")
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_command_carries_runtime_root_handoff() {
        let descriptor: OwnedFd = File::open(current_executable())
            .expect("open test executable")
            .into();
        let runtime_root = PathBuf::from("/private/axe-runtime");
        let executable = DescriptorExecutable::new(descriptor, None, Some(runtime_root.clone()))
            .expect("construct descriptor executable");
        let mut command = StdCommand::new(current_executable());
        command.env(INHERITED_RUNTIME_ROOT, "/untrusted");

        executable
            .prepare_command(&mut command, OsStr::new("axe"))
            .expect("prepare descriptor command");

        let inherited = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(INHERITED_RUNTIME_ROOT))
            .and_then(|(_, value)| value);
        assert_eq!(inherited, Some(runtime_root.as_os_str()));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn relay_identity_uses_sha1_elf_build_id() {
        let executable = File::open(current_executable()).expect("open test executable");
        let build_id = elf_build_id(&executable).expect("read test executable build ID");
        let name = relay_name(&build_id);

        assert_eq!(build_id.len(), 20);
        assert_eq!(name.len(), 40);
        assert!(managed_relay_name(&name));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn relay_gc_preserves_current_and_leased_images() {
        let scratch = Scratch::new("relay-gc");
        let base = scratch.0.join(RELAY_DIRECTORY_NAME);
        fs::create_dir(&base).expect("create relay directory");
        let current = base.join(relay_name(&[0x00; 20]));
        let newest = base.join(relay_name(&[0x11; 20]));
        let active = base.join(relay_name(&[0x22; 20]));
        let stale = base.join(relay_name(&[0x33; 20]));
        let unmanaged = base.join(format!("{NO_LOCK_RELAY_PREFIX}{}", relay_name(&[0x44; 20])));
        let legacy = base.join("axe-legacy");
        for (seconds, path) in [
            (1, &current),
            (4, &newest),
            (2, &active),
            (3, &stale),
            (0, &unmanaged),
            (0, &legacy),
        ] {
            fs::write(path, b"relay").expect("write relay fixture");
            let file = File::open(path).expect("open relay fixture");
            file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
            )
            .expect("set relay fixture time");
        }
        let active_lease = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(&active)
            .expect("open active relay");
        lock_relay_shared(&active_lease).expect("lease active relay");

        prune_relays(&base, &current).expect("prune relays with an active lease");

        assert!(current.exists());
        assert!(newest.exists());
        assert!(active.exists());
        assert!(!stale.exists());
        assert!(legacy.exists());
        assert!(unmanaged.exists());

        drop(active_lease);
        prune_relays(&base, &current).expect("prune relays after releasing the lease");

        assert!(current.exists());
        assert!(newest.exists());
        assert!(!active.exists());
        assert!(legacy.exists());
        assert!(unmanaged.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn retained_descriptor_and_filesystem_publication_are_selected_independently() {
        let executable = Executable::discover();
        let reexec = executable.reexec_path().expect("locate re-exec path");
        let current = current_executable();

        assert!(reexec.starts_with("/proc/self/fd"));
        assert_eq!(
            executable.filesystem_path().as_deref(),
            Some(current.as_path())
        );
        assert_eq!(executable.bridge_path().as_deref(), Some(current.as_path()));
    }

    #[test]
    fn forced_filesystem_capability_uses_canonical_path() {
        let current = current_executable();
        let executable = Executable::from_filesystem_path(&current).expect("open executable");

        assert_eq!(executable.reexec_path().as_deref(), Some(current.as_path()));
        assert_eq!(executable.bridge_path().as_deref(), Some(current.as_path()));
        assert!(executable.matches(&current));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_proc_path_falls_back_to_filesystem_capability() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("proc-loss");
        let current = current_executable();
        let proc_path = scratch.0.join("proc-exe");
        symlink(&current, &proc_path).expect("create simulated proc path");
        let proc = ProcExecutable::from_file(
            File::open(&current).expect("open current executable"),
            proc_path.clone(),
        )
        .expect("create proc capability");
        let executable = Executable {
            startup_facts: test_startup_facts(),
            requested_runtime_root: None,
            inherited_runtime_root: None,
            runtime_root: OnceLock::new(),
            relay_fallback_roots: Vec::new(),
            inherited: None,
            proc: Some(ProcExecutable {
                reexec_path: proc_path.clone(),
                ..proc
            }),
            filesystem: FilesystemExecutable::open(&current, None),
            relay: OnceLock::new(),
        };

        assert_eq!(
            executable.reexec_path().as_deref(),
            Some(proc_path.as_path())
        );
        fs::remove_file(&proc_path).expect("remove simulated proc path");
        assert_eq!(executable.reexec_path().as_deref(), Some(current.as_path()));
        assert_eq!(executable.bridge_path().as_deref(), Some(current.as_path()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn noexec_original_selects_relay_path() {
        let original = PathBuf::from("/mnt/noexec/axe");
        let relay = PathBuf::from("/run/user/1000/axe");

        assert_eq!(
            select_path_fallback(
                Some(original),
                Some(axe_paths::MountExecutionPolicy::NoExec),
                || Some(relay.clone()),
            ),
            Some(relay)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn path_fallback_is_refreshed_after_original_unlink() {
        let scratch = Scratch::new("refresh-fallback");
        let original = scratch.0.join("original");
        let relay = scratch.0.join("relay");
        fs::copy(current_executable(), &original).expect("copy original executable");
        fs::copy(current_executable(), &relay).expect("copy relay executable");
        let original = fs::canonicalize(original).expect("canonicalize original executable");
        let relay = fs::canonicalize(relay).expect("canonicalize relay executable");
        let executable =
            Executable::from_filesystem_path(&original).expect("open original executable");
        assert!(
            executable
                .relay
                .set(Ok(
                    FilesystemExecutable::open(&relay, None).expect("open relay executable"),
                ))
                .is_ok(),
            "install relay"
        );

        assert_eq!(
            executable.bridge_path().as_deref(),
            Some(original.as_path())
        );
        fs::remove_file(&original).expect("unlink original executable");
        assert_eq!(executable.bridge_path().as_deref(), Some(relay.as_path()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn retained_descriptor_rejects_replacement_and_runs_original_image() {
        let scratch = Scratch::new("replacement");
        let path = scratch.0.join("axe");
        fs::copy(current_executable(), &path).expect("copy executable");
        let executable = Executable::from_filesystem_path(&path).expect("open copied executable");
        let _ = executable.relay.set(Err(disabled_relay_failure()));

        fs::remove_file(&path).expect("remove original inode");
        fs::copy(current_executable(), &path).expect("replace executable inode");

        assert_eq!(executable.filesystem_path(), None);
        let status = executable
            .command()
            .expect("retain original executable")
            .arg("--list")
            .status()
            .expect("run retained image");
        assert!(status.success(), "retained executable failed: {status}");
    }

    #[test]
    fn unavailable_capability_fails_only_when_reexec_is_requested() {
        let executable = Executable::unavailable();

        assert_eq!(executable.reexec_path(), None);
        assert_eq!(executable.filesystem_path(), None);
        assert_eq!(executable.bridge_path(), None);
        assert!(!executable.matches(&current_executable()));
        assert!(!executable.can_reexec());
        let error = executable.command().expect_err("re-exec must fail");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(error.to_string(), NOT_FOUND);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_capability_survives_unlink_without_proc_path() {
        let scratch = Scratch::new("unlink");
        let path = scratch.0.join("axe");
        fs::copy(current_executable(), &path).expect("copy executable");
        let executable = Executable::from_filesystem_path(&path).expect("open copied executable");
        let _ = executable.relay.set(Err(disabled_relay_failure()));
        fs::remove_file(&path).expect("unlink copied executable");

        let status = executable
            .command()
            .expect("create descriptor command")
            .arg("--list")
            .status()
            .expect("execute unlinked image through descriptor");
        assert!(status.success(), "unlinked executable failed: {status}");
    }
}
