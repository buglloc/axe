use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::CaptureExecution;
use crate::{protocol, stream};

const RECEIPT_SCHEMA: &str = "raskop/vzik-capture-receipt";
const RECEIPT_SCHEMA_VERSION: u64 = 3;

#[derive(Debug)]
pub enum Error {
    Collector(protocol::ProtocolError),
    Stream(stream::Error),
    Io {
        operation: &'static str,
        path: Option<PathBuf>,
        source: io::Error,
    },
    Invalid(String),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Collector(error) => error.exit_code(),
            Self::Stream(error) => error.exit_code(),
            Self::Io { operation, .. } if *operation == "write summary" => 5,
            Self::Io { .. } | Self::Invalid(_) => 4,
        }
    }

    pub fn event_code(&self) -> &'static str {
        match self {
            Self::Collector(error) => error.event_code(),
            Self::Stream(error) => error.event_code(),
            Self::Io { source, .. } if source.kind() == io::ErrorKind::AlreadyExists => {
                "capture_path_exists"
            }
            Self::Io { .. } => "capture_io_failed",
            Self::Invalid(_) => "capture_invalid",
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Collector(error) => error.retryable(),
            Self::Stream(error) => error.retryable(),
            Self::Io { source, .. } => matches!(
                source.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            Self::Invalid(_) => false,
        }
    }

    pub fn details(&self) -> Value {
        match self {
            Self::Collector(error) => error.details(),
            Self::Stream(error) => error.details(),
            Self::Io {
                operation,
                path,
                source,
            } => json!({
                "io_operation":operation,
                "path":path.as_deref().map(crate::cli::linux_path),
                "io_kind":format!("{:?}", source.kind()).to_ascii_lowercase(),
            }),
            Self::Invalid(_) => json!({}),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collector(error) => write!(formatter, "collect evidence: {error}"),
            Self::Stream(error) => write!(formatter, "validate capture: {error}"),
            Self::Io {
                operation,
                path: Some(path),
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::Io {
                operation,
                path: None,
                source,
            } => write!(formatter, "{operation}: {source}"),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

pub fn execute(
    request: CaptureExecution,
    stdout: &mut impl Write,
    interruption: Option<&AtomicUsize>,
) -> Result<protocol::CollectionCompletion, Error> {
    let CaptureExecution {
        execution,
        output: capture_requested,
        receipt: receipt_requested,
        stderr,
    } = request;
    let stderr_requested = stderr.unwrap_or_else(|| append_suffix(&capture_requested, ".stderr"));
    let capture_path = prepare_path(&capture_requested)?;
    let receipt_path = prepare_path(&receipt_requested)?;
    let stderr_path = prepare_path(&stderr_requested)?;
    ensure_distinct(&capture_path, &receipt_path, &stderr_path)?;

    let mut capture_file = create_new(&capture_path, &capture_requested, "create capture")?;
    let mut saved_stderr = create_new(&stderr_path, &stderr_requested, "create saved stderr")?;
    let completion = match protocol::execute_controlled(&mut capture_file, execution, interruption)
    {
        Ok(completion) => completion,
        Err(error) => {
            crate::write_telemetry(
                &mut saved_stderr,
                error.event_code(),
                "collect",
                error.retryable(),
                &error.to_string(),
                error.details(),
            );
            let _ = saved_stderr.flush();
            let _ = saved_stderr.sync_all();
            let _ = capture_file.flush();
            let _ = capture_file.sync_all();
            return Err(Error::Collector(error));
        }
    };

    capture_file
        .flush()
        .map_err(|source| io_error("flush capture", source))?;
    capture_file
        .sync_all()
        .map_err(|source| io_error("sync capture", source))?;
    saved_stderr
        .flush()
        .map_err(|source| io_error("flush saved stderr", source))?;
    saved_stderr
        .sync_all()
        .map_err(|source| io_error("sync saved stderr", source))?;
    drop(capture_file);
    drop(saved_stderr);

    let receipt = build_receipt(&capture_path, &stderr_path).map_err(Error::Stream)?;
    write_receipt(&receipt_path, &receipt_requested, &receipt)?;

    if let Err(error) = verify_receipt(&receipt_path, &capture_path, &stderr_path, &receipt) {
        let _ = fs::remove_file(&receipt_path);
        return Err(error);
    }

    let capture = &receipt["capture"];
    let result = json!({
        "schema_version": 3,
        "outcome": completion.outcome,
        "artifact_status": "sealed",
        "receipt": receipt_requested,
        "capture": capture_requested,
        "stderr": stderr_requested,
        "command_id": capture["command_id"],
        "coverage_summary": {
            "stream_outcome": capture["stream_outcome"],
            "stream_coverage": capture["stream_coverage"],
            "planned_capabilities": capture["planned_capabilities"],
            "started_capabilities": capture["started_capabilities"],
            "not_started_capabilities": capture["not_started_capabilities"],
            "capability_counts": capture["capability_counts"],
            "capabilities": capture["capabilities"],
            "diagnostics": capture["diagnostics"],
        },
    });
    serde_json::to_writer(&mut *stdout, &result)
        .map_err(|source| io_error("write summary", io::Error::other(source)))?;
    stdout
        .write_all(b"\n")
        .map_err(|source| io_error("write summary", source))?;
    Ok(completion)
}

fn build_receipt(capture_path: &Path, stderr_path: &Path) -> Result<Value, stream::Error> {
    let summary = stream::inspect(capture_path)?;
    let (capture_sha256, capture_bytes) = hash_file(capture_path).map_err(stream::Error::Read)?;
    let (stderr_sha256, stderr_bytes) = hash_file(stderr_path).map_err(stream::Error::Read)?;

    let capabilities = summary
        .capabilities
        .iter()
        .map(|capability| {
            json!({
                "id": capability.id,
                "outcome": capability.outcome,
                "coverage": capability.coverage,
                "truncated": capability.truncated,
                "limits_hit": capability.limits_hit,
            })
        })
        .collect::<Vec<_>>();

    let started_capabilities = summary
        .capabilities
        .iter()
        .map(|capability| capability.id.as_str())
        .collect::<Vec<_>>();

    let capability_counts = json!({
        "planned":summary.planned_capabilities.len(),
        "started":started_capabilities.len(),
        "not_started":summary.not_started_capabilities.len(),
    });

    Ok(json!({
        "schema": RECEIPT_SCHEMA,
        "schema_version": RECEIPT_SCHEMA_VERSION,
        "expected_command": summary.command_id,
        "capture": {
            "path": capture_path,
            "sha256": capture_sha256,
            "bytes": capture_bytes,
            "records": summary.records,
            "data_records": summary.data_records,
            "command_id": summary.command_id,
            "invocation_kind": summary.invocation_kind,
            "collector": summary.collector,
            "global_limits": summary.global_limits,
            "stream_outcome": summary.stream_outcome,
            "stream_coverage": summary.coverage,
            "planned_capabilities": summary.planned_capabilities,
            "started_capabilities": started_capabilities,
            "capability_counts": capability_counts,
            "not_started_capabilities": summary.not_started_capabilities,
            "capabilities": capabilities,
            "diagnostics": {
                "records": summary.diagnostic_records,
                "reported_count": summary.diagnostic_count,
                "by_status": summary.diagnostics_by_status,
                "by_code": summary.diagnostics_by_code,
            },
        },
        "stderr": {
            "path": stderr_path,
            "sha256": stderr_sha256,
            "bytes": stderr_bytes,
        },
    }))
}

fn write_receipt(path: &Path, requested_path: &Path, receipt: &Value) -> Result<(), Error> {
    let mut file = create_new(path, requested_path, "create receipt")?;
    let result = (|| {
        serde_json::to_writer(&mut file, receipt)
            .map_err(|source| io_error("encode receipt", io::Error::other(source)))?;
        file.write_all(b"\n")
            .map_err(|source| io_error("write receipt", source))?;
        file.sync_all()
            .map_err(|source| io_error("sync receipt", source))
    })();

    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn verify_receipt(
    receipt_path: &Path,
    capture_path: &Path,
    stderr_path: &Path,
    expected: &Value,
) -> Result<(), Error> {
    let encoded = fs::read(receipt_path).map_err(|source| io_error("read receipt", source))?;
    let observed: Value = serde_json::from_slice(&encoded)
        .map_err(|source| Error::Invalid(format!("decode receipt: {source}")))?;

    if &observed != expected {
        return Err(Error::Invalid("receipt changed after sealing".into()));
    }

    let rebuilt = build_receipt(capture_path, stderr_path).map_err(Error::Stream)?;
    if rebuilt != observed {
        return Err(Error::Invalid(
            "capture or stderr changed after sealing".into(),
        ));
    }

    Ok(())
}

fn hash_file(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 << 10];
    let mut bytes = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        bytes = bytes.saturating_add(count as u64);
    }

    let mut encoded = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok((encoded, bytes))
}

fn prepare_path(path: &Path) -> Result<PathBuf, Error> {
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::Invalid(format!("path must name a file: {}", path.display())))?;

    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let parent = fs::canonicalize(parent)
        .map_err(|source| io_path_error("resolve output directory", parent, source))?;
    Ok(parent.join(file_name))
}

fn ensure_distinct(capture: &Path, receipt: &Path, stderr: &Path) -> Result<(), Error> {
    if capture == receipt || capture == stderr || receipt == stderr {
        return Err(Error::Invalid(
            "capture, receipt, and stderr paths must differ".into(),
        ));
    }
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn create_new(path: &Path, requested_path: &Path, operation: &'static str) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .map_err(|source| io_path_error(operation, requested_path, source))
}

fn io_error(operation: &'static str, source: io::Error) -> Error {
    Error::Io {
        operation,
        path: None,
        source,
    }
}

fn io_path_error(operation: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Io {
        operation,
        path: Some(path.to_path_buf()),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_artifact_has_machine_readable_event() {
        let error = io_path_error(
            "create capture",
            Path::new("/tmp/existing.jsonl"),
            io::Error::from(io::ErrorKind::AlreadyExists),
        );

        assert_eq!(error.event_code(), "capture_path_exists");
        assert!(error.to_string().contains("/tmp/existing.jsonl"));
    }
}
