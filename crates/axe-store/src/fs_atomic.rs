use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use rand::RngExt as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallMode {
    Replace,
    NoReplace,
}

pub(crate) fn write(
    path: &Path,
    mode: InstallMode,
    unix_mode: Option<u32>,
    write_content: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = parent(path);
    fs::create_dir_all(parent)?;
    let temporary = temporary_path(path)?;
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(mode) = unix_mode {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = unix_mode;

        let mut file = options.open(&temporary)?;
        write_content(&mut file)?;
        file.sync_all()?;
        install(&temporary, path, mode)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn install(source: &Path, destination: &Path, mode: InstallMode) -> io::Result<()> {
    let parent = parent(destination);
    fs::create_dir_all(parent)?;
    match mode {
        InstallMode::Replace => fs::rename(source, destination)?,
        InstallMode::NoReplace => {
            fs::hard_link(source, destination)?;
            fs::remove_file(source)?;
        }
    }
    File::open(parent)?.sync_all()
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) fn copy(source: &Path, destination: &Path, install: InstallMode) -> io::Result<()> {
    write(destination, install, None, |output| {
        let mut input = File::open(source)?;
        io::copy(&mut input, output)?;
        Ok(())
    })
}

fn temporary_path(path: &Path) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_os_string();
    name.push(temporary_suffix());
    Ok(path.with_file_name(name))
}

fn temporary_suffix() -> OsString {
    format!(
        ".tmp-{}-{:016x}",
        std::process::id(),
        rand::rng().random::<u64>()
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "axe-store-atomic-{}-{:016x}",
                std::process::id(),
                rand::rng().random::<u64>()
            ));
            fs::create_dir(&path).expect("create atomic fixture directory");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn no_replace_preserves_existing_file_and_cleans_temporary() {
        let scratch = Scratch::new();
        let path = scratch.0.join("output");
        fs::write(&path, b"original").expect("write existing output");

        let error = write(&path, InstallMode::NoReplace, None, |file| {
            file.write_all(b"replacement")
        })
        .expect_err("reject replacement");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).expect("read existing output"), b"original");
        assert_eq!(
            fs::read_dir(&scratch.0)
                .expect("read atomic fixture directory")
                .count(),
            1,
            "temporary output was not removed"
        );
    }

    #[test]
    fn replace_publishes_complete_file() {
        let scratch = Scratch::new();
        let path = scratch.0.join("output");
        fs::write(&path, b"original").expect("write existing output");

        write(&path, InstallMode::Replace, None, |file| {
            file.write_all(b"replacement")
        })
        .expect("replace output");

        assert_eq!(
            fs::read(path).expect("read replaced output"),
            b"replacement"
        );
    }
}
