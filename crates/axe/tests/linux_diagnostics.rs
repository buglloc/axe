#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-linux-diagnostics-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
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

fn run(scratch: &Scratch, applet: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .env("AXE_STORE_DIR", scratch.path().join("store"))
        .env("AXE_STORE_URL", "http://127.0.0.1:9")
        .env("AXE_STORE_ADDRESSES", "127.0.0.1")
        .args(["--applet", applet, "--"])
        .args(args)
        .output()
        .expect("run AXE applet")
}

#[test]
fn namei_reports_symlink_and_nosym_link_modes() {
    let scratch = Scratch::new("namei");
    let target = scratch.path().join("target");
    let link = scratch.path().join("link");
    fs::create_dir(&target).expect("create target");
    fs::write(target.join("file"), b"data").expect("write target file");
    std::os::unix::fs::symlink("target", &link).expect("create symlink");

    let followed = run(
        &scratch,
        "namei",
        &["-m", &format!("{}/file", link.display())],
    );
    assert!(followed.status.success());
    let followed = String::from_utf8(followed.stdout).expect("namei output is UTF-8");
    assert!(followed.contains("link -> target"));
    assert!(followed.contains("file"));

    let not_followed = run(
        &scratch,
        "namei",
        &["-n", &format!("{}/file", link.display())],
    );
    assert!(not_followed.status.success());
    let not_followed = String::from_utf8(not_followed.stdout).expect("namei output is UTF-8");
    assert!(not_followed.contains("link -> target"));
    assert!(not_followed.lines().any(|line| line.ends_with(" file")));
    let followed_target_lines = followed
        .lines()
        .filter(|line| line.trim_end().ends_with("target"))
        .count();
    let not_followed_target_lines = not_followed
        .lines()
        .filter(|line| line.trim_end().ends_with("target"))
        .count();
    assert_eq!(followed_target_lines, not_followed_target_lines + 1);
}

#[test]
fn fuser_silent_no_match_has_status_one() {
    let scratch = Scratch::new("fuser");
    let path = scratch.path().join("unused");
    fs::write(&path, b"data").expect("write unused file");

    let output = run(
        &scratch,
        "fuser",
        &["-s", path.to_str().expect("UTF-8 path")],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn lslocks_json_has_typed_root() {
    let scratch = Scratch::new("lslocks");
    let output = run(&scratch, "lslocks", &["-J", "-n", "-o", "PID,TYPE,MODE"]);
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("lslocks JSON");
    assert!(json["locks"].is_array());
}

#[test]
fn pstree_can_render_pid_tree_without_path_lookup() {
    let scratch = Scratch::new("pstree");
    let output = run(&scratch, "pstree", &["-A", "-p", "-T", "1"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).expect("pstree output is UTF-8");
    assert!(text.contains("(1)"));
}

#[test]
fn lsblk_lists_supported_columns_without_store_or_path() {
    let scratch = Scratch::new("lsblk");
    let output = run(&scratch, "lsblk", &["-H"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).expect("lsblk output is UTF-8");
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("NAME "))
    );
}
