#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-path-repair-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create fixture directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(root: &Path, tool: &str, args: &[&OsStr], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", tool, "--"])
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/nonexistent")
        .env("LC_ALL", "C.UTF-8")
        .env("AXE_STORE_MODE", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run applet");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().expect("wait for applet")
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn refs(values: &[OsString]) -> Vec<&OsStr> {
    values.iter().map(OsString::as_os_str).collect()
}

#[test]
fn find_utf8_globs_keep_invalid_filename_bytes() {
    let scratch = Scratch::new();
    let raw = OsStr::from_bytes(b"raw-\xff");
    fs::write(scratch.0.join(raw), b"raw").unwrap();
    fs::write(scratch.0.join("raw-\u{fffd}"), b"unicode").unwrap();
    for selector in ["-name", "-iname"] {
        for pattern in ["raw-?", "raw-[!x]"] {
            let argv = args(&[".", selector, pattern, "-print0"]);
            let output = run(&scratch.0, "find", &refs(&argv), b"");
            assert!(output.status.success(), "{:?}", output.stderr);
            let mut names = output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>();
            names.sort_unstable();
            let mut expected = vec![b"./raw-\xff".as_slice(), "./raw-\u{fffd}".as_bytes()];
            expected.sort_unstable();
            assert_eq!(names, expected, "{selector} {pattern}");
        }
    }
}

#[test]
fn find_empty_streamed_roots_fail_without_skipping_valid_neighbors() {
    let scratch = Scratch::new();
    let raw = OsString::from_vec(b"raw-\xff".to_vec());
    fs::write(scratch.0.join(&raw), b"raw").unwrap();
    fs::write(scratch.0.join("other"), b"other").unwrap();
    let mut input = raw.as_bytes().to_vec();
    input.extend_from_slice(b"\0\0other\0");
    let argv = args(&["-files0-from", "-", "-print0"]);
    let output = run(&scratch.0, "find", &refs(&argv), &input);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"raw-\xff\0other\0");
    assert!(!output.stderr.is_empty());
}
