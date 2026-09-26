use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use std::os::unix::process::CommandExt as _;

/// Internal environment handoff for descriptor-backed AXE children.
pub const INHERITED_EXECUTABLE_FD: &str = "__AXE_EXECUTABLE_FD";
/// Internal handoff for the path-only fallback paired with the descriptor.
pub const INHERITED_EXECUTABLE_FALLBACK: &str = "__AXE_EXECUTABLE_FALLBACK";
/// Internal handoff for AXE's process-wide runtime root.
pub const INHERITED_RUNTIME_ROOT: &str = "__AXE_RUNTIME_ROOT";

/// A retained Linux executable descriptor with an optional checked path fallback.
#[derive(Clone)]
pub struct DescriptorExecutable {
    descriptor: Arc<OwnedFd>,
    fallback: Option<FallbackExecutable>,
    runtime_root: Option<PathBuf>,
}

#[derive(Clone)]
struct FallbackExecutable {
    path: PathBuf,
    c_path: Arc<CString>,
    device: u64,
    inode: u64,
}

struct PreparedExec {
    _arguments: Vec<CString>,
    argument_pointers: Vec<usize>,
    _environment: Vec<CString>,
    environment_pointers: Vec<usize>,
}

impl DescriptorExecutable {
    /// Creates a descriptor target with optional checked path and runtime-root handoffs.
    pub fn new(
        descriptor: OwnedFd,
        fallback: Option<PathBuf>,
        runtime_root: Option<PathBuf>,
    ) -> io::Result<Self> {
        let descriptor = duplicate_descriptor(descriptor.as_raw_fd())?;
        let fallback = fallback
            .map(|path| FallbackExecutable::new(&path))
            .transpose()?;

        Ok(Self {
            descriptor: Arc::new(descriptor),
            fallback,
            runtime_root,
        })
    }

    /// Returns the checked filesystem fallback, when one exists.
    #[must_use]
    pub fn fallback_path(&self) -> Option<&Path> {
        self.fallback
            .as_ref()
            .map(|fallback| fallback.path.as_path())
    }

    /// Adds descriptor execution as the final pre-exec action.
    ///
    /// The command must contain a complete explicit environment: inherited
    /// variables must already have been expanded with `env_clear` plus `envs`.
    pub fn prepare_command(&self, command: &mut Command, argv0: &OsStr) -> io::Result<()> {
        command.env(
            INHERITED_EXECUTABLE_FD,
            self.descriptor.as_raw_fd().to_string(),
        );
        if let Some(path) = self.fallback_path() {
            command.env(INHERITED_EXECUTABLE_FALLBACK, path);
        }
        if let Some(root) = &self.runtime_root {
            command.env(INHERITED_RUNTIME_ROOT, root);
        }
        let prepared = Arc::new(PreparedExec::new(command, argv0)?);
        let descriptor = Arc::clone(&self.descriptor);
        let fallback = self.fallback.clone();

        // SAFETY: the closure captures only immutable, process-owned buffers and
        // descriptors. It calls async-signal-safe Linux/POSIX process syscalls,
        // performs no allocation, and runs after every previously registered
        // pre-exec action has completed.
        unsafe {
            command.pre_exec(move || {
                prepare_inherited_descriptor(descriptor.as_raw_fd())?;
                exec_descriptor(descriptor.as_raw_fd(), &prepared, fallback.as_ref())
            });
        }

        Ok(())
    }
}

fn duplicate_descriptor(descriptor: libc::c_int) -> io::Result<OwnedFd> {
    // Prefer a high descriptor to avoid colliding with application redirections.
    // A low RLIMIT_NOFILE makes that minimum invalid, so retry from the first
    // non-standard descriptor.
    for minimum in [256, 3] {
        // SAFETY: descriptor is live; F_DUPFD_CLOEXEC duplicates it or returns
        // an error without changing ownership of the original.
        let duplicated = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, minimum) };
        if duplicated >= 0 {
            // SAFETY: a successful F_DUPFD_CLOEXEC returns a new owned descriptor.
            return Ok(unsafe { OwnedFd::from_raw_fd(duplicated) });
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINVAL) {
            return Err(error);
        }
    }

    // F_DUPFD_CLOEXEC itself is unavailable on old kernels.
    // SAFETY: F_DUPFD follows the same duplication contract.
    let duplicated = unsafe { libc::fcntl(descriptor, libc::F_DUPFD, 3) };
    if duplicated == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: duplicated is live and F_GETFD/F_SETFD operate on its flags.
    let flags = unsafe { libc::fcntl(duplicated, libc::F_GETFD) };
    if flags == -1
        // SAFETY: duplicated remains live after F_GETFD.
        || unsafe { libc::fcntl(duplicated, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1
    {
        let error = io::Error::last_os_error();
        // SAFETY: duplicated is owned locally and has not escaped.
        unsafe {
            libc::close(duplicated);
        }
        return Err(error);
    }

    // SAFETY: duplicated is a new descriptor with close-on-exec set.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

impl FallbackExecutable {
    fn new(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "AXE executable fallback is not an executable regular file",
            ));
        }
        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(nul_error)?;

        Ok(Self {
            path: path.to_owned(),
            c_path: Arc::new(c_path),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl PreparedExec {
    fn new(command: &Command, argv0: &OsStr) -> io::Result<Self> {
        let mut arguments = Vec::with_capacity(command.get_args().len() + 1);
        arguments.push(CString::new(argv0.as_bytes()).map_err(nul_error)?);
        for argument in command.get_args() {
            arguments.push(CString::new(argument.as_bytes()).map_err(nul_error)?);
        }
        let argument_pointers = nul_terminated_pointers(&arguments);

        let mut environment = Vec::new();
        for (name, value) in command.get_envs() {
            let Some(value) = value else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "descriptor exec requires a complete explicit environment",
                ));
            };
            let name = name.as_bytes();
            if name.contains(&b'=') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "environment variable name contains '='",
                ));
            }

            let value = value.as_bytes();
            let mut entry = Vec::with_capacity(name.len() + value.len() + 1);
            entry.extend_from_slice(name);
            entry.push(b'=');
            entry.extend_from_slice(value);
            environment.push(CString::new(entry).map_err(nul_error)?);
        }
        let environment_pointers = nul_terminated_pointers(&environment);

        Ok(Self {
            _arguments: arguments,
            argument_pointers,
            _environment: environment,
            environment_pointers,
        })
    }
}

fn nul_terminated_pointers(values: &[CString]) -> Vec<usize> {
    let mut pointers = Vec::with_capacity(values.len() + 1);
    pointers.extend(values.iter().map(|value| value.as_ptr() as usize));
    pointers.push(0);
    pointers
}

fn prepare_inherited_descriptor(descriptor: libc::c_int) -> io::Result<()> {
    // SAFETY: descriptor is live. F_GETFD has no pointer arguments.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: descriptor is live. Clearing FD_CLOEXEC preserves it across the
    // imminent exec so the next AXE process can continue the capability chain.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn exec_descriptor(
    descriptor: libc::c_int,
    prepared: &PreparedExec,
    fallback: Option<&FallbackExecutable>,
) -> io::Result<()> {
    let argv = prepared
        .argument_pointers
        .as_ptr()
        .cast::<*const libc::c_char>();
    let envp = prepared
        .environment_pointers
        .as_ptr()
        .cast::<*const libc::c_char>();

    // SAFETY: descriptor is retained by the captured Arc; c"" is a valid empty
    // path for AT_EMPTY_PATH; argv/envp are immutable, null-terminated arrays
    // backed by captured CStrings for the lifetime of every call.
    unsafe {
        libc::syscall(
            libc::SYS_execveat,
            descriptor,
            c"".as_ptr(),
            argv,
            envp,
            libc::AT_EMPTY_PATH,
        );
    }
    let descriptor_error = io::Error::last_os_error();

    // SAFETY: the same descriptor and pointer invariants apply. fexecve is the
    // POSIX fallback for libc/kernel combinations where execveat is absent.
    unsafe {
        libc::fexecve(descriptor, argv, envp);
    }

    if let Some(fallback) = fallback {
        exec_checked_fallback(fallback, descriptor, argv, envp)?;
    }

    Err(descriptor_error)
}

fn exec_checked_fallback(
    fallback: &FallbackExecutable,
    inherited_descriptor: libc::c_int,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
) -> io::Result<()> {
    // SAFETY: c_path is NUL-terminated. O_NOFOLLOW rejects a substituted
    // symlink, and the returned descriptor is closed on every returning path.
    let descriptor = unsafe {
        libc::open(
            fallback.c_path.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }

    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: descriptor is live and metadata points to writable storage for a
    // libc::stat. A successful fstat initializes the complete value.
    if unsafe { libc::fstat(descriptor, metadata.as_mut_ptr()) } == -1 {
        let error = io::Error::last_os_error();
        // SAFETY: descriptor was returned by open and has not been closed.
        unsafe { libc::close(descriptor) };
        return Err(error);
    }
    // SAFETY: the successful fstat above initialized metadata.
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_dev as u64 != fallback.device || metadata.st_ino as u64 != fallback.inode {
        // SAFETY: descriptor was returned by open and has not been closed.
        unsafe { libc::close(descriptor) };
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }

    if descriptor != inherited_descriptor {
        // SAFETY: both descriptors are live. dup2 atomically replaces the
        // inherited capability with the exact image about to be execed.
        if unsafe { libc::dup2(descriptor, inherited_descriptor) } == -1 {
            let error = io::Error::last_os_error();
            // SAFETY: descriptor was returned by open and has not been closed.
            unsafe { libc::close(descriptor) };
            return Err(error);
        }
        // SAFETY: descriptor was returned by open and duplicated above.
        unsafe { libc::close(descriptor) };
    }

    // SAFETY: c_path, argv, and envp retain the validity described above. The
    // inode check narrows the unavoidable path race on kernels that lack usable
    // descriptor exec; callers should place generated fallbacks in private dirs.
    unsafe {
        libc::execve(fallback.c_path.as_ptr(), argv, envp);
    }
    Err(io::Error::last_os_error())
}

fn nul_error(error: std::ffi::NulError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error)
}
