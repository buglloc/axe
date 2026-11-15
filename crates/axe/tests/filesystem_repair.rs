#![cfg(unix)]

use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
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
    // SAFETY: only the child calls the async-signal-safe umask syscall.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o022);
            Ok(())
        });
    }
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

#[test]
fn install_checks_entry_identity_before_backing_up() {
    for directory in ["alias", "alias/"] {
        let scratch = Scratch::new("install-self");
        fs::create_dir(scratch.path().join("source")).unwrap();
        fs::write(scratch.path().join("source/file"), b"original").unwrap();
        symlink("source", scratch.path().join("alias")).unwrap();
        let output = run(scratch.path(), "install", &["-b", "source/file", directory]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(fs::read(scratch.path().join("source/file")).unwrap(), b"original");
        assert!(!scratch.path().join("source/file~").exists());
    }
    for option in ["-C", "-p", "-b"] {
        let scratch = Scratch::new("install-source-link");
        fs::write(scratch.path().join("source"), b"original").unwrap();
        symlink("source", scratch.path().join("destination")).unwrap();
        success(&run(scratch.path(), "install", &[option, "source", "destination"]));
        let source = fs::metadata(scratch.path().join("source")).unwrap();
        let destination = fs::symlink_metadata(scratch.path().join("destination")).unwrap();
        assert!(destination.is_file());
        assert_ne!(source.ino(), destination.ino());
        assert_eq!(destination.permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::read(scratch.path().join("source")).unwrap(), b"original");
        assert_eq!(fs::read(scratch.path().join("destination")).unwrap(), b"original");
        if option == "-b" {
            assert_eq!(fs::read_link(scratch.path().join("destination~")).unwrap(), Path::new("source"));
        }
    }
}

#[test]
fn install_backs_up_dangling_and_cyclic_symlink_entries() {
    for target in ["missing", "destination"] {
        let scratch = Scratch::new("install-backup");
        fs::write(scratch.path().join("source"), b"original").unwrap();
        symlink(target, scratch.path().join("destination")).unwrap();
        success(&run(scratch.path(), "install", &["-b", "source", "destination"]));
        assert_eq!(fs::read(scratch.path().join("source")).unwrap(), b"original");
        assert_eq!(fs::read(scratch.path().join("destination")).unwrap(), b"original");
        assert_eq!(fs::read_link(scratch.path().join("destination~")).unwrap(), Path::new(target));
    }
}

#[test]
fn mv_no_clobber_accepts_same_inode_and_directory_aliases() {
    let scratch = Scratch::new("mv-no-clobber-self");
    fs::create_dir(scratch.path().join("source")).unwrap();
    fs::write(scratch.path().join("source/file"), b"original").unwrap();
    fs::hard_link(scratch.path().join("source/file"), scratch.path().join("hard")).unwrap();
    symlink("source", scratch.path().join("alias")).unwrap();
    for destination in ["source/file", "hard", "alias", "alias/"] {
        success(&run(scratch.path(), "mv", &["-n", "source/file", destination]));
        assert_eq!(fs::read(scratch.path().join("source/file")).unwrap(), b"original");
        assert_eq!(fs::metadata(scratch.path().join("source/file")).unwrap().ino(), fs::metadata(scratch.path().join("hard")).unwrap().ino());
    }
}

#[test]
fn install_refuses_overwriting_its_fresh_destination() {
    let scratch = Scratch::new("install-duplicate");
    fs::write(scratch.path().join("source"), b"original").unwrap();
    fs::create_dir(scratch.path().join("destination")).unwrap();
    let output = run(scratch.path(), "install", &["source", "source", "destination"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("just-created"));
    assert_eq!(fs::read(scratch.path().join("destination/source")).unwrap(), b"original");
}

#[test]
fn cp_recursive_h_follows_only_the_command_line_symlink() {
    let scratch = Scratch::new("cp-recursive-h");
    fs::create_dir(scratch.path().join("source")).unwrap();
    fs::write(scratch.path().join("source/file"), b"original").unwrap();
    symlink("file", scratch.path().join("source/link")).unwrap();
    symlink("source", scratch.path().join("alias")).unwrap();
    success(&run(scratch.path(), "cp", &["-RH", "alias", "destination"]));
    assert!(scratch.path().join("destination").is_dir());
    assert_eq!(fs::metadata(scratch.path().join("destination")).unwrap().permissions().mode() & 0o777, fs::metadata(scratch.path().join("source")).unwrap().permissions().mode() & 0o777);
    assert_eq!(fs::read(scratch.path().join("destination/file")).unwrap(), b"original");
    assert_eq!(fs::read_link(scratch.path().join("destination/link")).unwrap(), Path::new("file"));
}

#[test]
fn cp_backup_preserves_original_hardlinks_and_replaces_destination() {
    let scratch = Scratch::new("cp-backup-hardlinks");
    fs::write(scratch.path().join("source"), b"original").unwrap();
    fs::hard_link(scratch.path().join("source"), scratch.path().join("destination")).unwrap();
    success(&run(scratch.path(), "cp", &["-b", "source", "destination"]));
    let source = fs::metadata(scratch.path().join("source")).unwrap();
    assert_eq!(source.ino(), fs::metadata(scratch.path().join("destination~")).unwrap().ino());
    assert_ne!(source.ino(), fs::metadata(scratch.path().join("destination")).unwrap().ino());
    fs::write(scratch.path().join("destination"), b"replacement").unwrap();
    assert_eq!(fs::read(scratch.path().join("source")).unwrap(), b"original");
    assert_eq!(fs::read(scratch.path().join("destination~")).unwrap(), b"original");
}

#[test]
fn cp_no_clobber_skips_same_inode_but_reports_cyclic_lookup() {
    let scratch = Scratch::new("cp-no-clobber");
    fs::create_dir(scratch.path().join("source")).unwrap();
    fs::write(scratch.path().join("source/file"), b"original").unwrap();
    fs::hard_link(scratch.path().join("source/file"), scratch.path().join("hard")).unwrap();
    symlink("source", scratch.path().join("alias")).unwrap();
    for destination in ["source/file", "hard", "alias", "alias/"] {
        success(&run(scratch.path(), "cp", &["-n", "source/file", destination]));
        assert_eq!(fs::read(scratch.path().join("source/file")).unwrap(), b"original");
    }
    symlink("cycle", scratch.path().join("cycle")).unwrap();
    let output = run(scratch.path(), "cp", &["-n", "source/file", "cycle"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read_link(scratch.path().join("cycle")).unwrap(), Path::new("cycle"));
    assert_eq!(fs::read(scratch.path().join("source/file")).unwrap(), b"original");
}

#[test]
fn cp_recursive_l_restores_umask_permissions_after_a_loop_error() {
    let scratch = Scratch::new("cp-loop-mode");
    fs::create_dir(scratch.path().join("source")).unwrap();
    fs::set_permissions(scratch.path().join("source"), fs::Permissions::from_mode(0o777)).unwrap();
    fs::write(scratch.path().join("source/file"), b"original").unwrap();
    symlink(".", scratch.path().join("source/loop")).unwrap();
    symlink("source", scratch.path().join("alias")).unwrap();
    let output = run(scratch.path(), "cp", &["-RL", "alias", "destination"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::metadata(scratch.path().join("destination")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::read(scratch.path().join("source/file")).unwrap(), b"original");
}

#[test]
fn ls_literal_quoting_preserves_tabs_but_explicit_q_hides_them() {
    let scratch = Scratch::new("ls-literal-tab");
    fs::write(scratch.path().join("tab\tname"), b"").unwrap();
    for locale in ["C", "C.UTF-8"] {
        for option in ["-1", "-N", "-F", "-X", "-S"] {
            let output = command(scratch.path(), "ls")
                .env("LC_ALL", locale)
                .env("QUOTING_STYLE", "literal")
                .args(["-1", option])
                .output()
                .unwrap();
            success(&output);
            assert_eq!(output.stdout, b"tab\tname\n");
        }
        let output = command(scratch.path(), "ls")
            .env("LC_ALL", locale)
            .env("QUOTING_STYLE", "literal")
            .args(["-1", "-q"])
            .output()
            .unwrap();
        success(&output);
        assert_eq!(output.stdout, b"tab?name\n");
    }
}
