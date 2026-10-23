//! Loop devices for `mount -o loop` and regular-file sources, following
//! util-linux: reuse an identical attachment of the same file, refuse an
//! overlapping one, and create new devices with autoclear so the kernel
//! detaches them after the last unmount (or when a failed mount drops the fd).

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use linux_raw_sys::loop_device::{
    LO_FLAGS_AUTOCLEAR, LO_FLAGS_READ_ONLY, LOOP_CLR_FD, LOOP_CONFIGURE, LOOP_CTL_GET_FREE,
    LOOP_GET_STATUS64, LOOP_SET_FD, LOOP_SET_STATUS64, loop_config, loop_info64,
};

const LOOP_CONTROL: &str = "/dev/loop-control";
/// Attempts when another process claims the free device first.
const CLAIM_ATTEMPTS: usize = 16;

/// `loop[=/dev/loopN]`, `offset=`, and `sizelimit=` from the mount options.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct LoopRequest {
    pub(super) device: Option<PathBuf>,
    pub(super) offset: u64,
    pub(super) size_limit: u64,
}

/// An attached loop device. The open descriptor keeps a fresh autoclear
/// device alive until the mount holds it.
pub(super) struct Attached {
    pub(super) device: PathBuf,
    /// Mount read-only: requested, or the backing file is write-protected.
    pub(super) read_only: bool,
    /// An existing attachment of the same file and range.
    pub(super) reused: bool,
    handle: File,
}

impl Attached {
    /// Detaches a device created for a mount that failed, as util-linux does;
    /// autoclear alone may detach only later, asynchronously.
    pub(super) fn release_unused(self) {
        if !self.reused {
            // SAFETY: the descriptor is live; LOOP_CLR_FD takes no argument.
            unsafe { libc::ioctl(self.handle.as_raw_fd(), LOOP_CLR_FD as _) };
        }
    }
}

pub(super) enum LoopError {
    Setup(io::Error),
    Overlap,
}

impl From<io::Error> for LoopError {
    fn from(error: io::Error) -> Self {
        Self::Setup(error)
    }
}

pub(super) fn attach(
    file: &Path,
    request: &LoopRequest,
    read_only: bool,
) -> Result<Attached, LoopError> {
    let (backing, read_only) = open_backing(file, read_only)?;
    let metadata = backing.metadata()?;
    if !metadata.is_file() && !metadata.file_type().is_block_device() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
    }
    if request.device.is_none()
        && let Some(attached) = find_attached(&metadata, request, read_only)?
    {
        return Ok(attached);
    }
    let mut flags = LO_FLAGS_AUTOCLEAR as u32;
    if read_only {
        flags |= LO_FLAGS_READ_ONLY as u32;
    }
    if let Some(device) = &request.device {
        return configure(device, &backing, file, request, flags, read_only);
    }
    let control = OpenOptions::new()
        .read(true)
        .write(true)
        .open(LOOP_CONTROL)?;
    for _ in 0..CLAIM_ATTEMPTS {
        // SAFETY: LOOP_CTL_GET_FREE takes no argument and returns a device number.
        let number = unsafe { libc::ioctl(control.as_raw_fd(), LOOP_CTL_GET_FREE as _) };
        if number < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let device = PathBuf::from(format!("/dev/loop{number}"));
        match configure(&device, &backing, file, request, flags, read_only) {
            Err(LoopError::Setup(error)) if error.raw_os_error() == Some(libc::EBUSY) => continue,
            result => return result,
        }
    }
    Err(io::Error::from_raw_os_error(libc::EBUSY).into())
}

/// Opens the backing file read-write unless that is impossible; like
/// util-linux, a write-protected file silently yields a read-only device.
fn open_backing(file: &Path, read_only: bool) -> io::Result<(File, bool)> {
    if !read_only {
        match OpenOptions::new().read(true).write(true).open(file) {
            Ok(backing) => return Ok((backing, false)),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EROFS | libc::EACCES | libc::EPERM)
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok((File::open(file)?, true))
}

/// Finds a loop device already backed by this file: an identical range is
/// reused, any other overlapping range is refused.
fn find_attached(
    backing: &fs::Metadata,
    request: &LoopRequest,
    read_only: bool,
) -> Result<Option<Attached>, LoopError> {
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return Ok(None);
    };
    let wanted = range(request.offset, request.size_limit, backing.len());
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.as_bytes().starts_with(b"loop") || !entry.path().join("loop").is_dir() {
            continue;
        }
        let device = Path::new("/dev").join(&name);
        let Ok(handle) = File::open(&device) else {
            continue;
        };
        let Some(info) = status(&handle) else {
            continue;
        };
        if info.lo_device != backing.dev() || info.lo_inode != backing.ino() {
            continue;
        }
        if info.lo_offset == request.offset && info.lo_sizelimit == request.size_limit {
            // A read-only device does not force a read-only mount: like
            // util-linux, the rw attempt fails and retries read-only with a warning.
            return Ok(Some(Attached {
                device,
                read_only,
                reused: true,
                handle,
            }));
        }
        let existing = range(info.lo_offset, info.lo_sizelimit, backing.len());
        if existing.0 < wanted.1 && wanted.0 < existing.1 {
            return Err(LoopError::Overlap);
        }
    }
    Ok(None)
}

fn range(offset: u64, size_limit: u64, file_size: u64) -> (u64, u64) {
    let end = if size_limit == 0 {
        file_size
    } else {
        offset.saturating_add(size_limit)
    };
    (offset, end.max(offset))
}

fn status(device: &File) -> Option<loop_info64> {
    // SAFETY: loop_info64 is plain old data; all-zero is a valid value.
    let mut info: loop_info64 = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is live and info is a writable loop_info64.
    let result = unsafe { libc::ioctl(device.as_raw_fd(), LOOP_GET_STATUS64 as _, &raw mut info) };
    (result == 0).then_some(info)
}

fn configure(
    device: &Path,
    backing: &File,
    file: &Path,
    request: &LoopRequest,
    flags: u32,
    read_only: bool,
) -> Result<Attached, LoopError> {
    let handle = OpenOptions::new()
        .read(true)
        .write(!read_only)
        .open(device)?;
    // SAFETY: loop_config is plain old data; all-zero is a valid value.
    let mut config: loop_config = unsafe { std::mem::zeroed() };
    config.fd = backing.as_raw_fd() as u32;
    config.info.lo_offset = request.offset;
    config.info.lo_sizelimit = request.size_limit;
    config.info.lo_flags = flags;
    let name = file.as_os_str().as_bytes();
    let length = name.len().min(config.info.lo_file_name.len() - 1);
    config.info.lo_file_name[..length].copy_from_slice(&name[..length]);

    // SAFETY: the descriptor is live and config is a fully initialized loop_config.
    let configured =
        unsafe { libc::ioctl(handle.as_raw_fd(), LOOP_CONFIGURE as _, &raw const config) };
    if configured != 0 {
        // Kernels before 5.8 lack LOOP_CONFIGURE: bind the file, then set the status.
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTTY)) {
            return Err(error.into());
        }
        // SAFETY: both descriptors are live; LOOP_SET_FD takes the backing fd.
        if unsafe { libc::ioctl(handle.as_raw_fd(), LOOP_SET_FD as _, backing.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: the descriptor is live and info is a fully initialized loop_info64.
        let status = unsafe {
            libc::ioctl(
                handle.as_raw_fd(),
                LOOP_SET_STATUS64 as _,
                &raw const config.info,
            )
        };
        if status != 0 {
            let error = io::Error::last_os_error();
            // SAFETY: the descriptor is live; LOOP_CLR_FD takes no argument.
            unsafe { libc::ioctl(handle.as_raw_fd(), LOOP_CLR_FD as _) };
            return Err(error.into());
        }
    }
    Ok(Attached {
        device: device.to_owned(),
        read_only,
        reused: false,
        handle,
    })
}

#[cfg(test)]
mod tests {
    use super::range;

    #[test]
    fn ranges_cover_the_file_tail_without_a_size_limit() {
        assert_eq!(range(4096, 0, 65536), (4096, 65536));
        assert_eq!(range(0, 1024, 65536), (0, 1024));
        assert_eq!(range(70000, 0, 65536), (70000, 70000));
    }
}
