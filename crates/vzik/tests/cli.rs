#![cfg(target_os = "linux")]

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::Value;

fn vzik() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vzik"))
}

fn json_lines(bytes: &[u8]) -> Vec<Value> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("JSON line"))
        .collect()
}

#[test]
fn invalid_request_has_structured_process_error() {
    let output = vzik().arg("not-a-command").output().expect("run vzik");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let diagnostics = json_lines(&output.stderr);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["schema_version"], 4);
    assert_eq!(diagnostics[0]["code"], "cli_invalid");
    assert_eq!(diagnostics[0]["operation"], "parse_request");
    assert_eq!(diagnostics[0]["retryable"], false);
    assert_eq!(diagnostics[0]["details"]["kind"], "invalid_subcommand");
}

#[test]
fn capability_discovery_separates_compact_index_from_detail() {
    let index = vzik().arg("capabilities").output().expect("run index");
    assert!(index.status.success());
    let index: Value = serde_json::from_slice(&index.stdout).expect("capability index JSON");

    assert_eq!(index["schema_version"], 4);
    assert_eq!(index["protocol"]["schema_version"], 4);
    assert_eq!(index["exit_status"]["degraded"], 3);
    assert!(index["capabilities"][0].get("request").is_none());

    let detail = vzik()
        .args(["capabilities", "porto.list"])
        .output()
        .expect("run detail");
    assert!(detail.status.success());
    let detail: Value = serde_json::from_slice(&detail.stdout).expect("capability detail JSON");

    assert_eq!(detail["capability"]["id"], "porto.list");
    assert!(detail["capability"]["request"].is_object());
    assert!(detail["capability"]["outcomes"].is_array());

    let unknown = vzik()
        .args(["capabilities", "porto.unknown"])
        .output()
        .expect("run unknown capability lookup");
    assert_eq!(unknown.status.code(), Some(2));
    let diagnostics = json_lines(&unknown.stderr);
    assert_eq!(diagnostics[0]["details"]["kind"], "unknown_capability");
    assert_eq!(diagnostics[0]["details"]["capability"], "porto.unknown");
}

#[test]
fn degraded_collection_status_agrees_with_terminal_and_summary() {
    let output = vzik()
        .args(["collect", "--max-records", "32"])
        .output()
        .expect("run bounded collection");

    assert_eq!(
        output.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = json_lines(&output.stdout);
    assert!(records.iter().all(|record| record["schema_version"] == 4));
    let terminal = records.last().expect("terminal record");
    assert_eq!(terminal["type"], "stream_end");
    assert_eq!(terminal["collection_outcome"], "degraded");
    assert_eq!(terminal["truncated"], true);
    assert!(terminal["not_started_capabilities"].is_array());

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("vzik-protocol-v4-{nonce}.jsonl"));
    fs::write(&path, &output.stdout).expect("write captured stream");
    let summary = vzik()
        .arg("summarize")
        .arg(&path)
        .output()
        .expect("summarize degraded stream");
    fs::remove_file(&path).expect("remove captured stream");

    assert!(summary.status.success());
    let summary: Value = serde_json::from_slice(&summary.stdout).expect("summary JSON");
    assert_eq!(summary["schema_version"], 4);
    assert_eq!(summary["stream"]["collection_outcome"], "degraded");
    assert_eq!(summary["stream"]["truncated"], true);

    let capture_path = std::env::temp_dir().join(format!("vzik-capture-v4-{nonce}.jsonl"));
    let receipt_path = std::env::temp_dir().join(format!("vzik-capture-v4-{nonce}.receipt.json"));
    let stderr_path = std::env::temp_dir().join(format!("vzik-capture-v4-{nonce}.stderr.jsonl"));
    let captured = vzik()
        .args(["capture", "collect", "--max-records", "32", "--output"])
        .arg(&capture_path)
        .arg("--receipt")
        .arg(&receipt_path)
        .arg("--stderr")
        .arg(&stderr_path)
        .output()
        .expect("capture degraded stream");

    assert_eq!(
        captured.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&captured.stderr)
    );
    let capture_result: Value =
        serde_json::from_slice(&captured.stdout).expect("capture result JSON");
    assert_eq!(capture_result["collection_outcome"], "degraded");
    assert_eq!(capture_result["artifact_status"], "sealed");
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).expect("read receipt"))
        .expect("receipt JSON");

    assert_eq!(receipt["schema_version"], 4);
    assert_eq!(receipt["capture"]["collection_outcome"], "degraded");
    assert_eq!(receipt["capture"]["stream_truncated"], true);

    fs::remove_file(capture_path).expect("remove capture");
    fs::remove_file(receipt_path).expect("remove receipt");
    fs::remove_file(stderr_path).expect("remove saved stderr");
}

#[test]
fn sigterm_emits_abort_record_without_success_terminal() {
    let mut child = vzik()
        .args([
            "filesystem",
            "privilege-surfaces",
            "--max-items",
            "1000000",
            "--max-entries",
            "10000000",
            "--max-depth",
            "128",
            "--deadline-seconds",
            "600",
            "--",
            "/",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vzik");

    let stderr = child.stderr.take().expect("stderr pipe");
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        BufReader::new(stderr)
            .read_to_end(&mut bytes)
            .expect("read stderr");
        bytes
    });

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout pipe"));
    let mut first_line = String::new();
    stdout
        .read_line(&mut first_line)
        .expect("read stream start");
    let first: Value = serde_json::from_str(&first_line).expect("stream-start JSON");
    assert_eq!(first["type"], "stream_start");

    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).expect("send SIGTERM");

    let mut remaining = Vec::new();
    stdout.read_to_end(&mut remaining).expect("read stdout");
    let status = child.wait().expect("wait for vzik");
    let stderr = stderr_reader.join().expect("join stderr reader");

    assert_eq!(status.code(), Some(128 + Signal::SIGTERM as i32));
    let mut stream = serde_json::from_str::<Value>(&first_line)
        .into_iter()
        .chain(json_lines(&remaining))
        .collect::<Vec<_>>();
    assert_eq!(
        stream.pop().expect("terminal record")["type"],
        "stream_abort"
    );
    assert!(!stream.iter().any(|record| record["type"] == "stream_end"));

    let diagnostics = json_lines(&stderr);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["code"], "collection_interrupted");
    assert_eq!(diagnostics[0]["operation"], "collect");
    assert_eq!(diagnostics[0]["retryable"], true);
    assert_eq!(diagnostics[0]["details"]["signal"], Signal::SIGTERM as i32);
}

struct ProcessInspectChild {
    child: std::process::Child,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Drop for ProcessInspectChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn process_inspect_child(skip: Option<&str>) -> ProcessInspectChild {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", "process_inspect_live_child", "--nocapture"])
        .env("VZIK_PROCESS_INSPECT_CHILD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if let Some(skip) = skip {
        command.args(["--skip", skip]);
    }
    let mut child = command.spawn().expect("spawn inspectable child");
    let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let mut child = ProcessInspectChild { child, stdout };
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            child.stdout.read_line(&mut line).expect("read readiness") > 0,
            "child exited before becoming inspectable",
        );
        if line.trim() == "vzik-process-inspect-ready" {
            break;
        }
    }
    child
}

fn process_detail_record(records: &[Value]) -> &Value {
    let mut details = records
        .iter()
        .filter(|record| record["type"] == "data" && record["data_kind"] == "process_detail");
    let detail = details.next().expect("process detail record");
    assert!(details.next().is_none(), "{records:?}");
    &detail["data"]
}

#[test]
fn process_inspect_live_child() {
    if std::env::var_os("VZIK_PROCESS_INSPECT_CHILD").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        println!("vzik-process-inspect-ready");
        let mut byte = [0];
        std::io::stdin()
            .read_exact(&mut byte)
            .expect("release child");
        return;
    }

    let child = process_inspect_child(None);
    let pid = child.child.id();
    let root = std::path::PathBuf::from(format!("/proc/{pid}"));
    let status = fs::read_to_string(root.join("status")).expect("child status");
    let status_field = |key: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .expect("child status field")
            .trim()
    };
    let uid = status_field("Uid:")
        .split_whitespace()
        .next()
        .expect("real UID")
        .parse::<u32>()
        .expect("numeric UID");
    let threads = status_field("Threads:")
        .parse::<u64>()
        .expect("thread count");
    let stat = fs::read_to_string(root.join("stat")).expect("child stat");
    let stat_fields = stat
        .rsplit_once(") ")
        .expect("stat command terminator")
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let user_ticks = stat_fields[11].parse::<u64>().expect("user ticks");
    let system_ticks = stat_fields[12].parse::<u64>().expect("system ticks");

    let output = vzik()
        .args(["process", "inspect"])
        .arg(pid.to_string())
        .output()
        .expect("inspect child");
    assert!(
        matches!(output.status.code(), Some(0 | 3)),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let records = json_lines(&output.stdout);
    let detail = process_detail_record(&records);
    assert_eq!(detail["pid"], pid);
    assert_eq!(detail["ppid"], std::process::id());
    assert_eq!(detail["is_self"], false);
    assert_eq!(detail["scope"], "collector_visible");
    assert_eq!(detail["uids"]["real"], uid);
    assert_eq!(detail["cap_effective"]["display"], status_field("CapEff:"));
    assert_eq!(
        detail["seccomp"],
        status_field("Seccomp:")
            .parse::<u32>()
            .expect("seccomp mode"),
    );
    assert_eq!(detail["resources"]["threads"], threads);
    assert_eq!(
        detail["resources"]["clock_ticks_per_second"],
        rustix::param::clock_ticks_per_second(),
    );
    assert!(
        detail["resources"]["cpu_user_ticks"]
            .as_u64()
            .expect("user ticks")
            >= user_ticks
    );
    assert!(
        detail["resources"]["cpu_system_ticks"]
            .as_u64()
            .expect("system ticks")
            >= system_ticks,
    );
    let executable = std::env::current_exe().expect("test executable");
    assert_eq!(
        detail["cmdline"][0]["display"],
        executable.to_str().expect("UTF-8 test executable"),
    );
    assert_eq!(
        detail["exe"]["display"],
        fs::read_link(root.join("exe"))
            .expect("child executable link")
            .to_str()
            .expect("UTF-8 executable link"),
    );
    let limits = fs::read_to_string(root.join("limits")).expect("child limits");
    let open_files = limits
        .lines()
        .find(|line| line.starts_with("Max open files"))
        .expect("open-files limit")
        .split_whitespace()
        .collect::<Vec<_>>();
    let limit = detail["rlimits"]
        .as_array()
        .expect("rlimits")
        .iter()
        .find(|limit| limit["name"]["display"] == "Max open files")
        .expect("serialized open-files limit");
    for (field, raw) in [("soft", open_files[3]), ("hard", open_files[4])] {
        let expected = if raw == "unlimited" {
            Value::from("unlimited")
        } else {
            Value::from(raw.parse::<u64>().expect("numeric open-files limit"))
        };
        assert_eq!(limit[field], expected);
    }
    assert_eq!(limit["units"]["display"], "files");
    for source in ["stat", "status", "cmdline", "limits", "io"] {
        assert_eq!(detail["sources"][source]["availability"], "available");
        assert_eq!(
            detail["sources"][source]["source"]["display"],
            root.join(source).to_str().expect("proc source path"),
        );
    }
    assert_eq!(records.last().expect("terminal")["type"], "stream_end");
}

#[test]
fn process_inspect_large_command_line_is_explicitly_partial_and_bounded() {
    for (length, limit) in [(32 << 10, "max_line_bytes"), (96 << 10, "max_source_bytes")] {
        let skip = "x".repeat(length);
        let child = process_inspect_child(Some(&skip));
        let output = vzik()
            .args(["process", "inspect"])
            .arg(child.child.id().to_string())
            .output()
            .expect("inspect long command line");
        assert_eq!(
            output.status.code(),
            Some(3),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            output
                .stdout
                .split(|byte| *byte == b'\n')
                .all(|line| (line.len() as u64) < vzik::GlobalLimits::targeted().max_line_bytes),
        );
        let records = json_lines(&output.stdout);
        let detail = process_detail_record(&records);
        assert_eq!(detail["pid"], child.child.id());
        assert_eq!(detail["sources"]["cmdline"]["availability"], "truncated");
        assert_eq!(detail["sources"]["stat"]["availability"], "available");
        assert_eq!(detail["security_context_complete"], false);
        if limit == "max_line_bytes" {
            let cmdline = detail["cmdline"].as_array().expect("bounded command line");
            assert!(!cmdline.is_empty());
            assert!(
                cmdline.last().expect("last argument")["display"]
                    .as_str()
                    .expect("argument")
                    .len()
                    < skip.len(),
            );
        } else {
            assert!(detail.get("cmdline").is_none());
        }
        let capability = records
            .iter()
            .find(|record| record["type"] == "capability_end")
            .expect("capability end");
        assert_eq!(capability["outcome"], "partial");
        assert_eq!(capability["coverage"]["truncated"], true);
        assert!(
            capability["limits_hit"]
                .as_array()
                .expect("limits")
                .iter()
                .any(|value| value == limit),
        );
        assert!(records.iter().any(|record| {
            record["type"] == "diagnostic"
                && record["diagnostic"]["code"] == "limit_reached"
                && record["diagnostic"]["source"]["display"]
                    == format!("/proc/{}/cmdline", child.child.id())
        }));
        assert_eq!(
            records.last().expect("terminal")["collection_outcome"],
            "degraded"
        );
    }
}

#[test]
fn process_inspect_zombie_retains_stat_and_reports_independent_missing_sources() {
    use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid};

    let mut child = process_inspect_child(None);
    let pid = child.child.id();
    std::io::Write::write_all(
        child.child.stdin.as_mut().expect("child control pipe"),
        &[1],
    )
    .expect("let child exit");
    assert!(matches!(
        waitid(
            Id::Pid(Pid::from_raw(pid as i32)),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT,
        )
        .expect("wait without reaping"),
        WaitStatus::Exited(_, 0),
    ));

    let output = vzik()
        .args(["process", "inspect"])
        .arg(pid.to_string())
        .output()
        .expect("inspect zombie");
    assert_eq!(output.status.code(), Some(3));
    let records = json_lines(&output.stdout);
    let detail = process_detail_record(&records);
    assert_eq!(detail["pid"], pid);
    assert_eq!(detail["state"]["display"], "Z");
    assert_eq!(detail["resources"]["rss_bytes"], 0);
    assert_eq!(detail["sources"]["stat"]["availability"], "available");
    assert_eq!(detail["security_context_complete"], false);
    for source in ["exe", "cwd", "root"] {
        assert_eq!(detail["sources"][source]["availability"], "not_found");
        assert!(detail.get(source).is_none());
        assert!(records.iter().any(|record| {
            record["type"] == "diagnostic"
                && record["diagnostic"]["code"] == "source_not_found"
                && record["diagnostic"]["source"]["display"] == format!("/proc/{pid}/{source}")
        }));
    }
    let capability = records
        .iter()
        .find(|record| record["type"] == "capability_end")
        .expect("capability end");
    assert_eq!(capability["outcome"], "partial");
    assert!(
        capability["coverage"]["vanished"]
            .as_u64()
            .expect("vanished")
            >= 3
    );
    assert_eq!(
        records.last().expect("terminal")["collection_outcome"],
        "degraded"
    );
}

#[test]
fn process_inspect_missing_pid_returns_a_valid_unavailable_stream() {
    let pid = i32::MAX;
    let output = vzik()
        .args(["process", "inspect"])
        .arg(pid.to_string())
        .output()
        .expect("inspect missing PID");
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let records = json_lines(&output.stdout);
    assert!(!records.iter().any(|record| record["type"] == "data"));
    assert!(records.iter().any(|record| {
        record["type"] == "diagnostic"
            && record["diagnostic"]["code"] == "source_not_found"
            && record["diagnostic"]["source"]["display"] == format!("/proc/{pid}/stat")
    }));
    let capability = records
        .iter()
        .find(|record| record["type"] == "capability_end")
        .expect("capability end");
    assert_eq!(capability["capability"], "process.inspect");
    assert_eq!(capability["outcome"], "unavailable");
    assert_eq!(capability["coverage"]["observed"], 0);
    assert_eq!(capability["coverage"]["vanished"], 1);
    assert_eq!(
        records.last().expect("terminal")["collection_outcome"],
        "degraded"
    );

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("vzik-missing-pid-{nonce}.jsonl"));
    fs::write(&path, &output.stdout).expect("write unavailable stream");
    let validation = vzik()
        .arg("validate")
        .arg(&path)
        .output()
        .expect("validate unavailable stream");
    fs::remove_file(&path).expect("remove unavailable stream");
    assert!(
        validation.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&validation.stderr),
    );
}

struct CaptureScratch(std::path::PathBuf);

impl CaptureScratch {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt as _;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("vzik-bundle-{}-{nonce}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("private fixture");
        Self(path)
    }
}

impl Drop for CaptureScratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn verify_bundle(directory: &std::path::Path, output: &[u8], outcome: &str) -> Value {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt as _;

    assert_eq!(
        fs::metadata(directory)
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let summary: Value = serde_json::from_slice(output).expect("capture summary");
    assert_eq!(summary["artifact_status"], "sealed");
    assert_eq!(summary["collection_outcome"], outcome);
    let receipt: Value =
        serde_json::from_slice(&fs::read(directory.join("receipt.json")).expect("receipt"))
            .expect("receipt JSON");
    for (filename, section) in [("capture.jsonl", "capture"), ("stderr.jsonl", "stderr")] {
        let path = directory.join(filename);
        assert_eq!(
            fs::metadata(&path)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let bytes = fs::read(&path).expect("artifact");
        assert_eq!(receipt[section]["bytes"], bytes.len() as u64);
        assert_eq!(
            receipt[section]["sha256"],
            format!("{:x}", Sha256::digest(&bytes))
        );
    }
    assert_eq!(
        fs::metadata(directory.join("receipt.json"))
            .expect("receipt metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let capture = fs::read(directory.join("capture.jsonl")).expect("capture");
    let records = json_lines(&capture);
    assert_eq!(
        records.last().expect("terminal")["collection_outcome"],
        outcome
    );
    let validated = vzik()
        .arg("validate")
        .arg(directory.join("capture.jsonl"))
        .output()
        .expect("validate");
    assert!(validated.status.success(), "{:?}", validated.stderr);
    let summarized = vzik()
        .arg("summarize")
        .arg(directory.join("capture.jsonl"))
        .output()
        .expect("summarize");
    assert!(summarized.status.success(), "{:?}", summarized.stderr);
    let summarized: Value = serde_json::from_slice(&summarized.stdout).expect("summary JSON");
    assert_eq!(summarized["stream"]["collection_outcome"], outcome);
    receipt
}

#[test]
fn capture_directory_seals_complete_evidence_and_keeps_literal_option_paths() {
    let scratch = CaptureScratch::new();
    fs::write(scratch.0.join("--output-dir"), b"owned evidence").expect("fixture file");
    let directory = scratch.0.join("evidence");
    let output = vzik()
        .current_dir(&scratch.0)
        .args(["capture", "--output-dir"])
        .arg(&directory)
        .args(["file", "stat", "--", "--output-dir"])
        .output()
        .expect("capture literal option path");
    assert_eq!(output.status.code(), Some(0), "{:?}", output.stderr);
    let receipt = verify_bundle(&directory, &output.stdout, "complete");
    assert_eq!(
        receipt["capture"]["started_capabilities"],
        serde_json::json!(["file.stat"])
    );
}

#[test]
fn capture_directory_preserves_byte_paths_and_seals_degraded_evidence() {
    use base64::Engine as _;
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let scratch = CaptureScratch::new();
    let directory = scratch
        .0
        .join(OsString::from_vec(b"evidence-\xff".to_vec()));
    let output = vzik()
        .args([
            "capture",
            "dbus",
            "list",
            "--socket",
            "/definitely/missing/vzik-bus",
            "--output-dir",
        ])
        .arg(&directory)
        .output()
        .expect("capture byte directory");
    assert_eq!(output.status.code(), Some(3), "{:?}", output.stderr);
    let receipt = verify_bundle(&directory, &output.stdout, "degraded");
    let raw = receipt["capture"]["path"]["raw_base64"]
        .as_str()
        .expect("lossless byte path");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(raw)
            .expect("base64 path"),
        fs::canonicalize(directory.join("capture.jsonl"))
            .expect("canonical artifact path")
            .as_os_str()
            .as_bytes()
    );
    assert_eq!(
        receipt["capture"]["capabilities"][0]["outcome"],
        "unavailable"
    );
}

#[test]
fn capture_directory_refuses_existing_directories_files_and_symlinks() {
    use std::os::unix::fs::symlink;

    let scratch = CaptureScratch::new();
    let existing = scratch.0.join("existing");
    fs::create_dir(&existing).expect("existing directory");
    fs::write(existing.join("marker"), b"keep").expect("marker");
    let file = scratch.0.join("file");
    fs::write(&file, b"keep file").expect("existing file");
    let link = scratch.0.join("link");
    symlink(&existing, &link).expect("symlink");
    let dangling = scratch.0.join("dangling");
    symlink(scratch.0.join("absent"), &dangling).expect("dangling symlink");
    for path in [&existing, &file, &link, &dangling] {
        let output = vzik()
            .args(["capture", "overview", "--output-dir"])
            .arg(path)
            .output()
            .expect("existing destination");
        assert_eq!(output.status.code(), Some(4), "{:?}", output.stderr);
        assert!(output.stdout.is_empty());
        assert_eq!(json_lines(&output.stderr)[0]["code"], "capture_path_exists");
    }
    assert_eq!(fs::read(existing.join("marker")).expect("marker"), b"keep");
    assert_eq!(fs::read(&file).expect("existing file"), b"keep file");
    assert_eq!(fs::read_dir(&existing).expect("directory").count(), 1);
    assert!(!scratch.0.join("absent").exists());
}

#[test]
fn capture_directory_rejects_invalid_requests_before_creating_artifacts() {
    let scratch = CaptureScratch::new();
    let directory = scratch.0.join("must-not-exist");
    for flag in ["--output", "--receipt", "--stderr"] {
        let output = vzik()
            .args(["capture", "overview", "--output-dir"])
            .arg(&directory)
            .arg(flag)
            .arg(scratch.0.join("unused"))
            .output()
            .expect("conflicting destination");
        assert_eq!(output.status.code(), Some(2), "{:?}", output.stderr);
        assert!(!directory.exists());
        assert!(!scratch.0.join("unused").exists());
    }
    for args in [&["capabilities"][..], &["process", "inspect", "0"][..]] {
        let output = vzik()
            .args(["capture", "--output-dir"])
            .arg(&directory)
            .args(args)
            .output()
            .expect("invalid nested command");
        assert_eq!(output.status.code(), Some(2), "{:?}", output.stderr);
        assert!(!directory.exists());
    }
    let output = vzik()
        .args(["capture", "overview", "--output-dir"])
        .arg(scratch.0.join("missing-parent/evidence"))
        .output()
        .expect("missing parent");
    assert_eq!(output.status.code(), Some(4), "{:?}", output.stderr);
    assert!(!scratch.0.join("missing-parent").exists());
}
