#![cfg(unix)]

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "axe-stream-repair-{}-{}",
                std::process::id(),
                NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create stream scratch directory: {error}"),
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

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(root: &Path, applet: &str, args: &[&str], locale: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .args(["--applet", applet, "--"])
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/nonexistent")
        .env("LANG", locale)
        .env("LC_ALL", locale)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish(child: Child) -> Output {
    let mut child = Running(child);
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    std::thread::scope(|scope| {
        let out = scope.spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let err = scope.spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.0.kill().unwrap();
                child.0.wait().unwrap();
                panic!("stream applet exceeded its deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        }
    })
}

fn run_locale(root: &Path, applet: &str, args: &[&str], input: &[u8], locale: &str) -> Output {
    let mut child = command(root, applet, args, locale)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let output = finish(child);
        if let Err(error) = writer.join().unwrap() {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        output
    })
}

fn run(root: &Path, applet: &str, args: &[&str], input: &[u8]) -> Output {
    run_locale(root, applet, args, input, "C")
}

fn assert_success(output: Output, expected: &[u8]) {
    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, expected);
    assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
}

#[test]
fn comm_repeated_stdin_consumes_records_without_locking_itself() {
    let scratch = Scratch::new();
    assert_success(run(scratch.path(), "comm", &["-", "-"], b"a\nb\n"), b"a\n\tb\n");
    assert_success(
        run(scratch.path(), "comm", &["-z", "-", "-"], b"a\0b\0"),
        b"a\0\tb\0",
    );
}

#[test]
fn comm_default_order_check_depends_on_unpaired_records() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("a"), b"\xff\n\x80").unwrap();
    assert_success(
        run(scratch.path(), "comm", &["-", "a"], b"\xff\n\x80"),
        b"\t\t\xff\n\t\t\x80\n",
    );
    let strict = run(
        scratch.path(),
        "comm",
        &["--check-order", "-", "a"],
        b"\xff\n\x80",
    );
    assert_eq!(strict.status.code(), Some(1));
    assert!(!strict.stderr.is_empty());
    fs::write(scratch.path().join("a"), b"b\na\n").unwrap();
    fs::write(scratch.path().join("empty"), b"").unwrap();
    let unpaired = run(scratch.path(), "comm", &["a", "empty"], b"");
    assert_eq!(unpaired.status.code(), Some(1));
    assert_eq!(unpaired.stdout, b"b\na\n");
    assert!(!unpaired.stderr.is_empty());
}

#[test]
fn tab_converters_preserve_explicit_stdin_among_file_operands() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("a"), b"abc\tdef\n").unwrap();
    for args in [&["-", "a", "-"][..], &["a", "-", "-"][..]] {
        let expected = if args[0] == "-" {
            &b"ab      cd\nabc     def\n"[..]
        } else {
            &b"abc     def\nab      cd\n"[..]
        };
        assert_success(run(scratch.path(), "expand", args, b"ab\tcd\n"), expected);
    }
    fs::write(scratch.path().join("a"), b"        a\n").unwrap();
    assert_success(
        run(scratch.path(), "unexpand", &["-", "a", "-"], b"        i\n"),
        b"\ti\n\ta\n",
    );
    assert_success(
        run(scratch.path(), "unexpand", &["a", "-", "-"], b"        i\n"),
        b"\ta\n\ti\n",
    );
}
