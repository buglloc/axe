#![cfg(unix)]

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use nix::unistd::pipe;

const AXE: &str = env!("CARGO_BIN_EXE_axe");

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-sigpipe-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create SIGPIPE scratch directory");
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

#[derive(Clone, Copy)]
enum Disposition {
    Default,
    Ignore,
}

fn configure(command: &mut Command, scratch: &Path, disposition: Disposition) {
    command
        .current_dir(scratch)
        .env_clear()
        .env("AXE_STORE_MODE", "off")
        .env("HOME", scratch)
        .env("PATH", scratch.join("bin"))
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC");

    let signal = match disposition {
        Disposition::Default => libc::SIG_DFL,
        Disposition::Ignore => libc::SIG_IGN,
    };
    // Rust's Command normally resets SIGPIPE to SIG_DFL in a forked child.
    // Set it explicitly so this test exercises both inherited dispositions.
    unsafe {
        command.pre_exec(move || {
            if libc::signal(libc::SIGPIPE, signal) == libc::SIG_ERR {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

fn run_closed_stdout(
    scratch: &Path,
    applet: &str,
    args: &[OsString],
    input: &[u8],
    disposition: Disposition,
) -> Output {
    let (reader, writer) = pipe().expect("create closed stdout pipe");
    drop(reader);
    let mut command = Command::new(AXE);
    command.args(["--applet", applet, "--"]).args(args);
    configure(&mut command, scratch, disposition);
    command.stdout(Stdio::from(writer)).stderr(Stdio::piped());
    if input.is_empty() {
        command.stdin(Stdio::null());
    } else {
        command.stdin(Stdio::piped());
    }

    let mut child = command.spawn().expect("spawn SIGPIPE applet");
    if !input.is_empty()
        && let Err(error) = child
            .stdin
            .take()
            .expect("SIGPIPE applet stdin is piped")
            .write_all(input)
    {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    child.wait_with_output().expect("wait for SIGPIPE applet")
}

fn assert_status(output: &Output, applet: &str, disposition: Disposition) {
    match disposition {
        Disposition::Default => {
            assert_eq!(
                output.status.signal(),
                Some(libc::SIGPIPE),
                "{applet} did not terminate by SIGPIPE: {output:?}"
            );
            assert!(
                output.stderr.is_empty(),
                "{applet} wrote diagnostics for SIGPIPE: {output:?}"
            );
        }
        Disposition::Ignore => {
            assert_eq!(
                output.status.signal(),
                None,
                "{applet} was signaled: {output:?}"
            );
            assert!(
                output.status.code().is_some_and(|code| code != 0),
                "{applet} reported success after EPIPE: {output:?}"
            );
            assert!(
                !output.stderr.is_empty(),
                "{applet} did not report its generic write error: {output:?}"
            );
        }
    }
}

fn os(path: &Path) -> OsString {
    path.as_os_str().to_owned()
}

fn make_archive(path: &Path) {
    let file = File::create(path).expect("create tar fixture");
    let mut builder = tar::Builder::new(file);
    let payload = vec![b'x'; 64];
    for index in 0..128 {
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("entry-{index:03}"), payload.as_slice())
            .expect("append tar fixture entry");
    }
    builder
        .into_inner()
        .expect("finish tar fixture")
        .sync_all()
        .expect("sync tar fixture");
}

fn cases(scratch: &Scratch) -> Vec<(&'static str, Vec<OsString>, Vec<u8>)> {
    let root = scratch.path().join("tree-root");
    fs::create_dir_all(root.join("nested")).expect("create tree fixture");
    for index in 0..128 {
        fs::write(root.join(format!("file-{index:03}")), b"payload\n").expect("write tree fixture");
        fs::write(
            root.join("nested").join(format!("file-{index:03}")),
            b"payload\n",
        )
        .expect("write nested tree fixture");
    }

    let left = scratch.path().join("left");
    let right = scratch.path().join("right");
    fs::write(&left, b"left\n").expect("write cmp left fixture");
    fs::write(&right, b"right\n").expect("write cmp right fixture");

    let strings_file = scratch.path().join("strings-input");
    fs::write(&strings_file, b"a-long-printable-string\n".repeat(4096))
        .expect("write strings fixture");

    let archive = scratch.path().join("archive.tar");
    make_archive(&archive);

    let file_bin = scratch.path().join("file-input");
    fs::write(&file_bin, b"plain text\n").expect("write file fixture");

    let bin = scratch.path().join("bin");
    fs::create_dir(&bin).expect("create which PATH fixture");
    symlink(AXE, bin.join("sigpipe-tool")).expect("create which PATH executable");

    vec![
        ("sed", vec![OsString::from("")], b"line\n".repeat(16 * 1024)),
        ("which", vec![OsString::from("sigpipe-tool")], Vec::new()),
        ("tree", vec![os(&root)], Vec::new()),
        ("cmp", vec![os(&left), os(&right)], Vec::new()),
        (
            "find",
            vec![os(&root), OsString::from("-print")],
            Vec::new(),
        ),
        (
            "jq",
            vec![OsString::from("-n"), OsString::from("\"payload\"")],
            Vec::new(),
        ),
        (
            "gzip",
            vec![OsString::from("-c")],
            b"compressible\n".repeat(64 * 1024),
        ),
        (
            "tar",
            vec![
                OsString::from("-t"),
                OsString::from("-f"),
                OsString::from("archive.tar"),
            ],
            Vec::new(),
        ),
        ("strings", vec![os(&strings_file)], Vec::new()),
        ("file", vec![os(&file_bin)], Vec::new()),
        #[cfg(target_os = "linux")]
        ("ip", vec![OsString::from("link")], Vec::new()),
    ]
}

#[test]
fn applets_match_gnu_sigpipe_dispositions() {
    let scratch = Scratch::new();
    for (applet, args, input) in cases(&scratch) {
        for disposition in [Disposition::Default, Disposition::Ignore] {
            let output = run_closed_stdout(scratch.path(), applet, &args, &input, disposition);
            assert_status(&output, applet, disposition);
        }
    }
}

#[test]
fn axe_shell_pipeline_reports_sigpipe_with_pipefail() {
    let scratch = Scratch::new();
    let script = "set -o pipefail; yes | sed \"\" | head -n1";
    let mut axe = Command::new(AXE);
    axe.args(["-c", script]);
    configure(&mut axe, scratch.path(), Disposition::Default);
    axe.env(
        "PATH",
        std::env::var_os("AXE_REFERENCE_PATH").unwrap_or_default(),
    );
    let output = axe.output().expect("run AXE pipeline");
    assert_eq!(output.status.code(), Some(141), "AXE pipeline: {output:?}");
    assert!(
        output.stderr.is_empty(),
        "AXE pipeline wrote diagnostics: {output:?}"
    );

    let mut env_command = Command::new(AXE);
    env_command.args(["-c", "env"]);
    configure(&mut env_command, scratch.path(), Disposition::Default);
    env_command.env(
        "PATH",
        std::env::var_os("AXE_REFERENCE_PATH").unwrap_or_default(),
    );
    let env_output = env_command.output().expect("run AXE env");
    assert!(
        !String::from_utf8_lossy(&env_output.stdout)
            .lines()
            .any(|line| line.starts_with("RUST_SIGPIPE=")),
        "AXE added a SIGPIPE environment variable: {env_output:?}"
    );

    let bash = which::which("bash").expect("bash is required for SIGPIPE comparison");
    let reference = Command::new(bash)
        .args(["-c", script])
        .output()
        .expect("run bash pipeline");
    assert_eq!(
        reference.status.code(),
        Some(141),
        "bash reference pipeline: {reference:?}"
    );
}
