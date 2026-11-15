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
