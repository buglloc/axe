#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-killall-{}-{}",
            std::process::id(),
            unique_name()
        ));
        fs::create_dir(&path).expect("create killall scratch directory");
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

fn unique_name() -> String {
    let mut bytes = [0; 6];
    fs::File::open("/dev/urandom")
        .expect("open process-name entropy source")
        .read_exact(&mut bytes)
        .expect("read process-name entropy");
    // Fourteen bytes leaves room for a prefix-neighbor within Linux TASK_COMM_LEN.
    format!(
        "Ax{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
    )
}

fn offline_axe(scratch: &Scratch) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .env("AXE_STORE_DIR", scratch.path().join("store"))
        .env("AXE_STORE_URL", "http://127.0.0.1:9")
        .env("AXE_STORE_ADDRESSES", "127.0.0.1");
    command
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct ControlledProcess(Child);

impl ControlledProcess {
    fn spawn(scratch: &Scratch, name: &str) -> Self {
        assert!(name.len() <= 15);
        let ready = scratch.path().join(unique_name());
        let child = Command::new(std::env::current_exe().expect("locate killall test binary"))
            .args(["--exact", "killall_process_helper", "--nocapture"])
            .env("AXE_KILLALL_HELPER_NAME", name)
            .env("AXE_KILLALL_HELPER_READY", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn controlled native process");
        // Own the child before any assertions so panic always kills and reaps it.
        let mut process = Self(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                process
                    .0
                    .try_wait()
                    .expect("poll process readiness")
                    .is_none(),
                "controlled process exited before readiness"
            );
            assert!(Instant::now() < deadline, "process readiness timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        process
    }

    fn wait_for_signal(&mut self, signal: i32) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let status: ExitStatus = loop {
            if let Some(status) = self.0.try_wait().expect("poll signalled process") {
                break status;
            }
            assert!(Instant::now() < deadline, "target did not receive signal");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.signal(), Some(signal), "target status: {status}");
    }

    fn assert_alive(&mut self) {
        assert!(
            self.0.try_wait().expect("poll prefix-neighbor").is_none(),
            "killall signalled a prefix-neighbor"
        );
    }
}

impl Drop for ControlledProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Check the complete matching set before signalling, not just our own helpers.
// Random short names avoid collisions, and this check refuses to signal an
// unrelated process even if an existing host process happens to share a name.
fn assert_only_targets(name: &str, ignore_case: bool, targets: &[&ControlledProcess]) {
    let actual: BTreeSet<u32> = fs::read_dir("/proc")
        .expect("read process inventory")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let comm = fs::read_to_string(entry.path().join("comm")).ok()?;
            let comm = comm.trim_end_matches('\n');
            let matches = if ignore_case {
                comm.eq_ignore_ascii_case(name)
            } else {
                comm == name
            };
            matches.then_some(pid)
        })
        .collect();
    let expected: BTreeSet<u32> = targets.iter().map(|process| process.0.id()).collect();
    assert_eq!(actual, expected, "unexpected process matches for {name}");
}

#[test]
fn killall_process_helper() {
    let Some(name) = std::env::var_os("AXE_KILLALL_HELPER_NAME") else {
        return;
    };
    let name = name.to_str().expect("helper name is UTF-8");
    // The test harness runs this function on a worker thread. Writing the
    // thread-group leader's comm, rather than prctl on this thread, makes the
    // native process name visible through /proc/PID/stat to killall.
    fs::write("/proc/self/comm", name).expect("set controlled process name");
    assert_eq!(
        fs::read_to_string("/proc/self/comm")
            .expect("read controlled process name")
            .trim_end_matches('\n'),
        name
    );
    fs::write(
        std::env::var_os("AXE_KILLALL_HELPER_READY").expect("helper readiness path"),
        b"ready",
    )
    .expect("publish helper readiness");
    loop {
        std::thread::park();
    }
}

#[test]
fn direct_killall_signals_all_exact_names_and_preserves_prefix_neighbor() {
    let scratch = Scratch::new();
    let name = unique_name();
    let mut first = ControlledProcess::spawn(&scratch, &name);
    let mut second = ControlledProcess::spawn(&scratch, &name);
    let mut neighbor = ControlledProcess::spawn(&scratch, &format!("{name}x"));
    assert_only_targets(&name, false, &[&first, &second]);

    let output = offline_axe(&scratch)
        .args(["killall", &name])
        .output()
        .expect("run direct bundled killall");
    assert_success(&output);
    first.wait_for_signal(libc::SIGTERM);
    second.wait_for_signal(libc::SIGTERM);
    neighbor.assert_alive();
    assert!(!scratch.path().join("store").exists());
}

#[test]
fn killall_honors_explicit_named_and_numeric_signals() {
    let scratch = Scratch::new();
    for (flags, signal) in [
        (&["-s", "USR1"][..], libc::SIGUSR1),
        (&["-USR2"][..], libc::SIGUSR2),
        (&["-9"][..], libc::SIGKILL),
    ] {
        let name = unique_name();
        let mut target = ControlledProcess::spawn(&scratch, &name);
        assert_only_targets(&name, false, &[&target]);
        let output = offline_axe(&scratch)
            .args(["--applet", "killall", "--"])
            .args(flags)
            .arg(&name)
            .output()
            .expect("run killall with selected signal");
        assert_success(&output);
        target.wait_for_signal(signal);
    }
}

#[test]
fn shell_dispatches_bundled_killall_without_path_or_store() {
    let scratch = Scratch::new();
    let name = unique_name();
    let mut target = ControlledProcess::spawn(&scratch, &name);
    let mut neighbor = ControlledProcess::spawn(&scratch, &format!("{name}x"));
    assert_only_targets(&name, false, &[&target]);
    let output = offline_axe(&scratch)
        .args(["--noprofile", "--norc", "-c"])
        .arg(format!("killall -USR1 {name}"))
        .output()
        .expect("dispatch killall from AXE shell");
    assert_success(&output);
    target.wait_for_signal(libc::SIGUSR1);
    neighbor.assert_alive();
}

#[test]
fn killall_case_insensitive_user_filter_and_verbose_output() {
    let scratch = Scratch::new();
    let name = unique_name();
    let mut target = ControlledProcess::spawn(&scratch, &name);
    let lower = name.to_ascii_lowercase();
    assert_only_targets(&lower, true, &[&target]);
    // SAFETY: getuid takes no arguments and only reads the process's real UID.
    let uid = unsafe { libc::getuid() }.to_string();
    let output = offline_axe(&scratch)
        .args(["killall", "-I", "-u", &uid, "-v", &lower])
        .output()
        .expect("run filtered verbose killall");
    assert_success(&output);
    target.wait_for_signal(libc::SIGTERM);
}

#[test]
fn killall_reports_no_match_and_quiet_preserves_failure_status() {
    let scratch = Scratch::new();
    let name = unique_name();
    assert_only_targets(&name, false, &[]);
    let output = offline_axe(&scratch)
        .args(["killall", &name])
        .output()
        .expect("run killall with no matching process");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no process found"));
    let output = offline_axe(&scratch)
        .args(["killall", "-q", &name])
        .output()
        .expect("run quiet killall with no matching process");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn killall_lists_signals_and_rejects_invalid_arguments() {
    let scratch = Scratch::new();
    let output = offline_axe(&scratch)
        .args(["killall", "-l"])
        .output()
        .expect("list supported killall signals");
    assert_success(&output);
    let signals = String::from_utf8_lossy(&output.stdout);
    assert!(signals.lines().any(|signal| signal == "TERM"));
    assert!(signals.lines().any(|signal| signal == "USR1"));
    for args in [
        &["killall"][..],
        &["killall", "-s", "not-a-signal", "not-a-process"][..],
    ] {
        let output = offline_axe(&scratch)
            .args(args)
            .output()
            .expect("run killall with invalid arguments");
        assert_eq!(output.status.code(), Some(2));
    }
}
