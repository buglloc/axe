use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(crate) fn open(path: &Path, follow_symlinks: bool) -> Result<(PathBuf, File, Metadata)> {
    let destination = if follow_symlinks {
        fs::canonicalize(path).map_err(Error::Input)?
    } else {
        path.to_path_buf()
    };
    let metadata = fs::metadata(&destination).map_err(Error::Input)?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file").into());
    }
    let file = File::open(&destination).map_err(Error::Input)?;
    let metadata = file.metadata().map_err(Error::Input)?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file").into());
    }
    Ok((destination, file, metadata))
}

pub(crate) fn temporary(path: &Path) -> Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(tempfile::NamedTempFile::new_in(parent)?)
}

pub(crate) fn publish(
    temporary: tempfile::NamedTempFile,
    path: &Path,
    metadata: &Metadata,
    suffix: &OsStr,
) -> Result<()> {
    let file = temporary.as_file();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, fchown};
        // An unprivileged owner may be unable to preserve the original group.
        // Apply the mode after chown, which can clear set-ID bits.
        let _ = fchown(file, Some(metadata.uid()), Some(metadata.gid()));
    }
    file.set_permissions(metadata.permissions())?;
    file.sync_all()?;

    let backup = (!suffix.is_empty()).then(|| backup_name(path, suffix));
    let backup = backup.as_deref().filter(|backup| *backup != path);
    if let Some(backup) = backup {
        fs::rename(path, backup)?
    }
    if let Err(error) = temporary.persist(path) {
        if let Some(backup) = backup {
            if let Err(restore) = fs::rename(backup, path) {
                return Err(io::Error::new(
                    restore.kind(),
                    format!("could not publish {} ({error}) or restore it; original remains at {}: {restore}",
                        path.display(), backup.display()),
                ).into());
            }
        }
        return Err(Error::TempfilePersist(error));
    }
    Ok(())
}

fn backup_name(path: &Path, suffix: &OsStr) -> PathBuf {
    let suffix_bytes = suffix.as_encoded_bytes();
    if !suffix_bytes.contains(&b'*') {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        return PathBuf::from(name);
    }
    let mut name = OsString::new();
    for (index, part) in suffix_bytes.split(|&byte| byte == b'*').enumerate() {
        if index != 0 {
            name.push(path.as_os_str())
        }
        // SAFETY: splitting an OsStr at ASCII '*' preserves encoding boundaries.
        name.push(unsafe { OsStr::from_encoded_bytes_unchecked(part) });
    }
    PathBuf::from(name)
}
