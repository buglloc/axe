#![cfg(unix)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "axe-layout-repair-{}-{}",
                std::process::id(),
                NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create layout scratch directory: {error}"),
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

fn run(root: &Path, applet: &str, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", applet, "--"])
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/nonexistent")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn layout applet");
    let mut stdin = child.stdin.take().expect("piped stdin");
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let output = child.wait_with_output().expect("wait for layout applet");
        if let Err(error) = writer.join().expect("stdin writer panicked") {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        output
    })
}

fn assert_success(output: Output, expected: &[u8]) {
    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, expected);
    assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
}

#[test]
fn sort_general_numeric_orders_signed_infinities() {
    let scratch = Scratch::new();
    assert_success(
        run(
            scratch.path(),
            "sort",
            &["-g"],
            b"NaN\n-nan\n-inf\n+inf\n1e2\n10\n-0\n0\n1.01\n",
        ),
        b"-nan\nNaN\n-inf\n-0\n0\n1.01\n10\n1e2\n+inf\n",
    );
}

#[test]
fn fmt_preserves_carriage_returns_in_formatted_words() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "fmt", &[], b"a\r\nb\r\nlast\r"),
        b"a\r b\r last\r\n",
    );
}

#[test]
fn fmt_nonmatching_prefix_records_keep_their_terminators() {
    let scratch = Scratch::new();
    for input in [&b"a"[..], &b"a\r\nb\xff\r"[..], &b"a\n"[..]] {
        assert_success(
            run(scratch.path(), "fmt", &["-p", "> ", "-w12"], input),
            input,
        );
    }
}

#[test]
fn fmt_missing_operand_does_not_suppress_later_files() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("a"), b"abc\tdef").unwrap();
    let output = run(scratch.path(), "fmt", &["missing", "a"], b"");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"abc\tdef\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing"));
}
