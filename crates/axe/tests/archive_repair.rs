#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-archive-repair-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create scratch directory");
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

fn run(directory: &Path, applet: &str, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", applet, "--"])
        .args(args)
        .current_dir(directory)
        .env_clear()
        .env("HOME", directory)
        .env("LC_ALL", "C")
        .env("PATH", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn applet");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write input");
    child.wait_with_output().expect("wait for applet")
}

#[test]
fn base_decoders_reject_carriage_returns_unless_ignoring_garbage() {
    let scratch = Scratch::new("decode-cr");
    for (applet, format) in [
        ("base64", None),
        ("base32", None),
        ("basenc", Some("--base64")),
        ("basenc", Some("--base64url")),
        ("basenc", Some("--base32")),
        ("basenc", Some("--base32hex")),
        ("basenc", Some("--base16")),
        ("basenc", Some("--base2msbf")),
        ("basenc", Some("--base2lsbf")),
    ] {
        let mut args = vec!["-d"];
        args.extend(format);
        let output = run(scratch.path(), applet, &args, b"\r\n");
        assert_eq!(output.status.code(), Some(1), "{applet} {args:?}: {output:?}");
        assert!(output.stdout.is_empty());
        let output = run(scratch.path(), applet, &args, b"\n");
        assert!(output.status.success(), "{applet} {args:?}: {output:?}");
        args.push("-i");
        let output = run(scratch.path(), applet, &args, b"\r\n");
        assert!(output.status.success(), "{applet} {args:?}: {output:?}");
    }
    let output = run(scratch.path(), "base64", &["-d"], b"jQe\r\n3");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output.stdout, b"\x8d\x07");
    let output = run(scratch.path(), "base64", &["-di"], b"jQe\r\n3");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"\x8d\x07\xb7");
}
