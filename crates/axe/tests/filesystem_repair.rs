#![cfg(unix)]

use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "axe-filesystem-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create fixture: {error}"),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(directory: &Path, applet: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .args(["--applet", applet, "--"])
        .current_dir(directory)
        .env_clear()
        .env("HOME", directory)
        .env("PATH", "/nonexistent")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    command
}

fn run(directory: &Path, applet: &str, args: &[&str]) -> Output {
    command(directory, applet).args(args).output().unwrap()
}

fn success(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn mv_preserves_symlink_entries_for_no_clobber_and_backup() {
    for target in ["missing", "destination"] {
        for option in ["-n", "-b"] {
            let scratch = Scratch::new("mv-symlink");
            fs::write(scratch.path().join("source"), b"payload\0\n").unwrap();
            symlink(target, scratch.path().join("destination")).unwrap();
            success(&run(
                scratch.path(),
                "mv",
                &[option, "source", "destination"],
            ));
            if option == "-n" {
                assert_eq!(fs::read(scratch.path().join("source")).unwrap(), b"payload\0\n");
                assert_eq!(fs::read_link(scratch.path().join("destination")).unwrap(), Path::new(target));
            } else {
                assert!(!scratch.path().join("source").exists());
                assert_eq!(fs::read(scratch.path().join("destination")).unwrap(), b"payload\0\n");
                assert_eq!(fs::read_link(scratch.path().join("destination~")).unwrap(), Path::new(target));
            }
        }
    }
}
