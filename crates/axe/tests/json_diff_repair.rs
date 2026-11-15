use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-json-diff-repair-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, applet: &str, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--applet", applet, "--"])
            .args(args)
            .current_dir(&self.0)
            .env_clear()
            .env("HOME", &self.0)
            .env("LC_ALL", "C")
            .env("PATH", "/nonexistent")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Err(error) = child.stdin.take().unwrap().write_all(input) {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        child.wait_with_output().unwrap()
    }

    fn diff(&self, args: &[&str], left: &[u8], right: &[u8]) -> Output {
        fs::write(self.0.join("left"), left).unwrap();
        fs::write(self.0.join("right"), right).unwrap();
        let mut args = args.to_vec();
        args.extend(["left", "right"]);
        self.run("diff", &args, b"")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn expect_output(output: Output, status: i32, stdout: &[u8]) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert_eq!(output.stdout, stdout, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn diff_blank_lines_are_empty_unless_trailing_whitespace_is_ignored() {
    let scratch = Scratch::new();
    expect_output(
        scratch.diff(&["-B"], b"\n", b" \n"),
        1,
        b"1c1\n< \n---\n>  \n",
    );
    expect_output(scratch.diff(&["-B"], b"", b"\n"), 0, b"");
    for option in ["-w", "-b", "-Z"] {
        expect_output(scratch.diff(&["-B", option], b"", b" \t\n"), 0, b"");
    }
    expect_output(
        scratch.diff(&["-B"], b"\n", b"\r\n"),
        1,
        b"1c1\n< \n---\n> \r\n",
    );
    expect_output(
        scratch.diff(&["-B", "--strip-trailing-cr"], b"\n", b"\r\n"),
        0,
        b"",
    );
}
