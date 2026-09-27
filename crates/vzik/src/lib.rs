#![forbid(unsafe_code)]

mod capture;
mod cli;
mod discovery;
pub mod environment;
#[cfg(target_os = "linux")]
mod inventory;
#[cfg(target_os = "linux")]
pub mod procfs;
mod protocol;
mod stream;

use std::ffi::OsString;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use serde_json::{Value, json};

pub use cli::{CapabilityId, GlobalLimits};

/// Runs the collector with explicit output sinks.
///
/// The first argument is treated as `argv[0]`. This keeps the same entry point
/// usable by the standalone binary and by an embedded multicall applet.
pub fn run<I, W, E>(args: I, stdout: &mut W, stderr: &mut E) -> i32
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    run_controlled(args, stdout, stderr, None)
}

fn run_controlled<I, W, E>(
    args: I,
    stdout: &mut W,
    stderr: &mut E,
    interruption: Option<&AtomicUsize>,
) -> i32
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    let args = args.into_iter().collect::<Vec<_>>();

    let action = match cli::parse(args) {
        Ok(action) => action,
        Err(error) => {
            write_telemetry(
                stderr,
                "cli_invalid",
                "parse_request",
                false,
                &error.to_string(),
                error.details(),
            );
            return 2;
        }
    };

    match action {
        cli::Action::Help(help) => match stdout.write_all(help.as_bytes()) {
            Ok(()) => 0,
            Err(error) => {
                write_io_error(stderr, "write_help", &error);
                5
            }
        },
        cli::Action::Validate(path) => {
            finish_stream(stream::validate(&path, stdout), "validate_stream", stderr)
        }
        cli::Action::Summarize(path) => {
            finish_stream(stream::summarize(&path, stdout), "summarize_stream", stderr)
        }
        cli::Action::Capture(request) => match capture::execute(request, stdout, interruption) {
            Ok(completion) => completion.exit_code(),
            Err(error) => {
                write_telemetry(
                    stderr,
                    error.event_code(),
                    "capture",
                    error.retryable(),
                    &error.to_string(),
                    error.details(),
                );
                error.exit_code()
            }
        },
        cli::Action::Capabilities(selected) => finish_output(
            discovery::write_capabilities(stdout, selected),
            "write_capabilities",
            stderr,
        ),
        cli::Action::Execute(execution) => {
            match protocol::execute_controlled(stdout, execution, interruption) {
                Ok(completion) => completion.exit_code(),
                Err(error) => {
                    write_telemetry(
                        stderr,
                        error.event_code(),
                        "collect",
                        error.retryable(),
                        &error.to_string(),
                        error.details(),
                    );
                    error.exit_code()
                }
            }
        }
    }
}

/// Multicall-compatible entry point with scoped cooperative signal handling.
pub fn entry(args: Vec<OsString>) -> i32 {
    entry_with_signals(args)
}

/// Standalone entry point with cooperative SIGINT and SIGTERM handling.
pub fn entry_with_signals(args: Vec<OsString>) -> i32 {
    use signal_hook::consts::signal::{SIGINT, SIGTERM};

    let interruption = Arc::new(AtomicUsize::new(0));
    let mut registrations = Vec::with_capacity(2);
    for signal in [SIGINT, SIGTERM] {
        match signal_hook::flag::register_usize(signal, Arc::clone(&interruption), signal as usize)
        {
            Ok(registration) => registrations.push(registration),
            Err(error) => {
                for registration in registrations {
                    signal_hook::low_level::unregister(registration);
                }
                let stderr = io::stderr();
                write_io_error(&mut stderr.lock(), "install_signal_handler", &error);
                return 4;
            }
        }
    }

    let stdout = io::stdout();
    let stderr = io::stderr();
    let status = run_controlled(
        args,
        &mut stdout.lock(),
        &mut stderr.lock(),
        Some(interruption.as_ref()),
    );

    for registration in registrations {
        signal_hook::low_level::unregister(registration);
    }
    status
}

fn finish_stream(
    result: Result<(), stream::Error>,
    operation: &str,
    stderr: &mut impl Write,
) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            write_telemetry(
                stderr,
                error.event_code(),
                operation,
                error.retryable(),
                &error.to_string(),
                error.details(),
            );
            error.exit_code()
        }
    }
}

fn finish_output(result: io::Result<()>, operation: &str, stderr: &mut impl Write) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            write_io_error(stderr, operation, &error);
            5
        }
    }
}

fn write_io_error(writer: &mut impl Write, operation: &str, error: &io::Error) {
    let retryable = matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    );
    write_telemetry(
        writer,
        "output_write_failed",
        operation,
        retryable,
        &error.to_string(),
        json!({"io_kind":format!("{:?}", error.kind()).to_ascii_lowercase()}),
    );
}

pub(crate) fn write_telemetry(
    writer: &mut impl Write,
    code: &str,
    operation: &str,
    retryable: bool,
    message: &str,
    details: Value,
) {
    let record = json!({
        "schema_version": 3,
        "level": "error",
        "event": code,
        "code": code,
        "operation": operation,
        "retryable": retryable,
        "message": message.chars().take(1024).collect::<String>(),
        "details": details,
    });

    let _ = serde_json::to_writer(&mut *writer, &record);
    let _ = writer.write_all(b"\n");
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn unix_socket_capabilities_report_unavailable_without_failing_collection() {
        for group in ["systemctl", "dbus"] {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let status = run(
                [
                    OsString::from("vzik"),
                    OsString::from(group),
                    OsString::from("list"),
                    OsString::from("--socket"),
                    OsString::from("/definitely/missing/vzik-bus"),
                ],
                &mut stdout,
                &mut stderr,
            );

            assert_eq!(status, 3, "stderr: {}", String::from_utf8_lossy(&stderr));
            let records = String::from_utf8(stdout)
                .expect("protocol output is UTF-8")
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid JSONL"))
                .collect::<Vec<_>>();
            let terminal = records
                .iter()
                .find(|record| record["type"] == "capability_end")
                .expect("capability terminal record");

            assert_eq!(terminal["outcome"], "unavailable");
            assert_eq!(terminal["coverage"]["skipped"], 1);
            let diagnostic = records
                .iter()
                .find(|record| record["type"] == "diagnostic")
                .expect("unavailability diagnostic");
            assert_eq!(
                diagnostic["diagnostic"]["provider"],
                if group == "systemctl" {
                    "systemd_dbus"
                } else {
                    "dbus"
                }
            );
        }
    }

    #[test]
    fn capture_seals_validated_unix_socket_evidence() {
        use std::fs;
        use std::os::unix::net::UnixListener;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("vzik-capture-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).expect("create fixture directory");

        let socket_path = root.join("service.sock");
        let _listener = UnixListener::bind(&socket_path).expect("bind fixture socket");
        let capture_path = root.join("capture.jsonl");
        let receipt_path = root.join("capture.receipt.json");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = run(
            [
                OsString::from("vzik"),
                OsString::from("capture"),
                OsString::from("filesystem"),
                OsString::from("unix-sockets"),
                root.as_os_str().to_owned(),
                OsString::from("--max-items"),
                OsString::from("10"),
                OsString::from("--max-entries"),
                OsString::from("100"),
                OsString::from("--max-depth"),
                OsString::from("4"),
                OsString::from("--output"),
                capture_path.as_os_str().to_owned(),
                OsString::from("--receipt"),
                receipt_path.as_os_str().to_owned(),
            ],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(status, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
        let result: serde_json::Value =
            serde_json::from_slice(&stdout).expect("capture summary JSON");
        assert_eq!(result["schema_version"], 3);
        assert_eq!(result["outcome"], "complete");
        assert_eq!(result["command_id"], "filesystem.unix_sockets");
        assert_eq!(result["capture"], capture_path.to_string_lossy().as_ref());
        assert_eq!(result["receipt"], receipt_path.to_string_lossy().as_ref());
        assert_eq!(
            result["coverage_summary"]["capabilities"][0]["outcome"],
            "complete"
        );

        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt_path).expect("read receipt"))
                .expect("receipt JSON");
        assert_eq!(receipt["schema"], "raskop/vzik-capture-receipt");
        assert_eq!(receipt["schema_version"], 3);
        assert_eq!(
            receipt["capture"]["started_capabilities"],
            serde_json::json!(["filesystem.unix_sockets"])
        );
        assert_eq!(receipt["expected_command"], "filesystem.unix_sockets");
        assert_eq!(receipt["capture"]["data_records"], 1);

        let capture = fs::read_to_string(&capture_path).expect("read capture");
        assert!(capture.contains("\"data_kind\":\"filesystem_unix_socket\""));
        fs::remove_dir_all(&root).expect("remove fixture directory");
    }
}
