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

#[test]
fn missing_mode_preserves_cyclic_components_and_resolves_later_parents() {
    let scratch = Scratch::new();
    let root = scratch.0.canonicalize().unwrap();
    fs::write(root.join("present"), b"present").unwrap();
    for cycle in [&["a", "b"][..], &["x", "y", "z"][..]] {
        for (index, name) in cycle.iter().enumerate() {
            std::os::unix::fs::symlink(cycle[(index + 1) % cycle.len()], root.join(name))
                .unwrap();
        }
        for tool in ["readlink", "realpath"] {
            for (operand, expected) in [
                (cycle[0].to_owned(), root.join(cycle[0])),
                (format!("{}/../present", cycle[0]), root.join("present")),
            ] {
                let argv = args(&["-m", &operand]);
                let output = run(&root, tool, &refs(&argv), b"");
                assert!(output.status.success(), "{tool} {operand}: {:?}", output.stderr);
                let mut bytes = expected.as_os_str().as_bytes().to_vec();
                bytes.push(b'\n');
                assert_eq!(output.stdout, bytes);
            }
            let argv = args(&["-e", cycle[0]]);
            let output = run(&root, tool, &refs(&argv), b"");
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
        }
    }
    fs::create_dir(root.join("dst")).unwrap();
    let argv = args(&["-sr", "a", "dst/link"]);
    let output = run(&root, "ln", &refs(&argv), b"");
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(fs::read_link(root.join("dst/link")).unwrap(), Path::new("../a"));
}

#[test]
fn xargs_runs_completed_arguments_before_a_later_quote_error() {
    let scratch = Scratch::new();
    for options in [&[][..], &["-n1"][..], &["-s512"][..]] {
        let mut argv = args(options);
        argv.extend(args(&["printf", "%s\\0"]));
        let output = run(&scratch.0, "xargs", &refs(&argv), b"unclosed \"quote");
        assert_eq!(output.status.code(), Some(1), "{options:?}");
        assert_eq!(output.stdout, b"unclosed\0", "{options:?}");
    }
    let argv = args(&["-L1", "printf", "%s\\0"]);
    let output = run(&scratch.0, "xargs", &refs(&argv), b"unclosed \"quote");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn xargs_default_mode_truncates_nul_fields_but_zero_mode_keeps_arguments() {
    let scratch = Scratch::new();
    for options in [&[][..], &["-n1"][..], &["-L1"][..], &["-s512"][..]] {
        let mut argv = args(options);
        argv.extend(args(&["printf", "%s\\0"]));
        let output = run(&scratch.0, "xargs", &refs(&argv), b"word\0other\n");
        assert!(output.status.success(), "{options:?}: {:?}", output.stderr);
        assert_eq!(output.stdout, b"word\0");
        assert!(!output.stderr.is_empty());
    }
    let argv = args(&["-0", "printf", "%s\\0"]);
    let output = run(&scratch.0, "xargs", &refs(&argv), b"word\0other\0");
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(output.stdout, b"word\0other\0");
    assert!(output.stderr.is_empty());
}

#[test]
fn forced_recursive_rm_ignores_a_trailing_slash_symlink_without_following_it() {
    let scratch = Scratch::new();
    fs::create_dir(scratch.0.join("target")).unwrap();
    fs::write(scratch.0.join("target/keep"), b"keep").unwrap();
    std::os::unix::fs::symlink("target", scratch.0.join("link")).unwrap();
    let argv = args(&["-rf", "link/"]);
    let output = run(&scratch.0, "rm", &refs(&argv), b"");
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read_link(scratch.0.join("link")).unwrap(), Path::new("target"));
    assert_eq!(fs::read(scratch.0.join("target/keep")).unwrap(), b"keep");
}
