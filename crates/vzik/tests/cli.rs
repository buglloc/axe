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
    assert_eq!(diagnostics[0]["schema_version"], 3);
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
    assert_eq!(index["schema_version"], 3);
    assert_eq!(index["protocol"]["schema_version"], 3);
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
    assert!(records.iter().all(|record| record["schema_version"] == 3));
    let terminal = records.last().expect("terminal record");
    assert_eq!(terminal["type"], "stream_end");
    assert_eq!(terminal["outcome"], "degraded");
    assert!(terminal["not_started_capabilities"].is_array());

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("vzik-protocol-v3-{nonce}.jsonl"));
    fs::write(&path, &output.stdout).expect("write captured stream");
    let summary = vzik()
        .arg("summarize")
        .arg(&path)
        .output()
        .expect("summarize degraded stream");
    fs::remove_file(&path).expect("remove captured stream");
    assert!(summary.status.success());
    let summary: Value = serde_json::from_slice(&summary.stdout).expect("summary JSON");
    assert_eq!(summary["schema_version"], 3);
    assert_eq!(summary["stream"]["outcome"], "degraded");
    assert_eq!(
        summary["capabilities"]["counts"]["planned"],
        summary["capabilities"]["planned"]
            .as_array()
            .expect("planned IDs")
            .len()
    );

    let capture_path = std::env::temp_dir().join(format!("vzik-capture-v3-{nonce}.jsonl"));
    let receipt_path = std::env::temp_dir().join(format!("vzik-capture-v3-{nonce}.receipt.json"));
    let stderr_path = std::env::temp_dir().join(format!("vzik-capture-v3-{nonce}.stderr.jsonl"));
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
    assert_eq!(capture_result["outcome"], "degraded");
    assert_eq!(capture_result["artifact_status"], "sealed");
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).expect("read receipt"))
        .expect("receipt JSON");
    assert_eq!(receipt["schema_version"], 3);
    assert_eq!(receipt["capture"]["stream_outcome"], "degraded");
    for field in [
        "planned_capabilities",
        "started_capabilities",
        "not_started_capabilities",
        "capability_counts",
    ] {
        assert_eq!(
            receipt["capture"][field],
            capture_result["coverage_summary"][field]
        );
    }
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
