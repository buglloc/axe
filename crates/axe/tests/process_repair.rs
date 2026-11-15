#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-process-repair-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn applet(root: &Path, name: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .args(["--applet", name, "--"])
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/nonexistent")
        .env("LC_ALL", "C")
        .env("AXE_STORE_MODE", "off");
    command
}

fn success(output: Output) -> String {
    assert!(output.status.success(), "{:?}", output.stderr);
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn lsof_directory_selection_uses_existing_entries_and_their_inode_aliases() {
    let scratch = Scratch::new();
    let directory = scratch.0.join("selected");
    fs::create_dir(&directory).unwrap();
    fs::create_dir(directory.join("nested")).unwrap();
    let outside = scratch.0.join("outside");
    fs::write(&outside, b"outside").unwrap();
    fs::hard_link(&outside, directory.join("alias")).unwrap();
    let held_alias = File::open(&outside).unwrap();
    let nested = directory.join("nested/held");
    fs::write(&nested, b"nested").unwrap();
    let held_nested = File::open(&nested).unwrap();
    let deleted = directory.join("deleted");
    fs::write(&deleted, b"deleted").unwrap();
    let held_deleted = File::open(&deleted).unwrap();
    fs::remove_file(&deleted).unwrap();
    let pid = std::process::id().to_string();
    for option in ["+d", "+D"] {
        let output = success(
            applet(&scratch.0, "lsof")
                .args(["-nP", "-a", "-p", &pid, "-Ff", option])
                .arg(&directory)
                .output()
                .unwrap(),
        );
        let contains = |file: &File| {
            let marker = format!("f{}", file.as_raw_fd());
            output.lines().any(|line| line == marker)
        };
        assert!(contains(&held_alias), "{option}: {output:?}");
        assert_eq!(contains(&held_nested), option == "+D", "{output:?}");
        assert!(!contains(&held_deleted), "{option}: {output:?}");
    }
}

struct OwnedChild(std::process::Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn pstree_arguments_do_not_repeat_the_executable_path() {
    let scratch = Scratch::new();
    let child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--noprofile", "--no-config", "-c", "read REPLY"])
            .current_dir(&scratch.0)
            .env("AXE_STORE_MODE", "off")
            .env("PATH", "/nonexistent")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = child.0.id().to_string();
    let comm = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap();
    let output = success(
        applet(&scratch.0, "pstree")
            .args(["-a", "-l", &pid])
            .output()
            .unwrap(),
    );
    assert_eq!(
        output.lines().next(),
        Some(format!("{} --noprofile --no-config -c read REPLY", comm.trim()).as_str())
    );
}

#[test]
fn timeout_kill_terminates_its_group_but_foreground_returns_timeout_status() {
    use std::os::unix::process::ExitStatusExt;

    let scratch = Scratch::new();
    for foreground in [false, true] {
        let mut command = applet(&scratch.0, "timeout");
        if foreground {
            command.arg("--foreground");
        }
        let output = command
            .args(["-s", "KILL", "0.02", env!("CARGO_BIN_EXE_axe"), "--applet", "sleep", "--", "0.2"])
            .output()
            .unwrap();
        if foreground {
            assert_eq!(output.status.code(), Some(124), "{output:?}");
        } else {
            assert_eq!(output.status.signal(), Some(libc::SIGKILL), "{output:?}");
        }
    }
}
