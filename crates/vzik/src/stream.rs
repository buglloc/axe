use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::cli::MAX_LINE_BYTES;
use crate::protocol::SCHEMA_VERSION;

const OUTCOMES: [&str; 4] = ["complete", "partial", "unavailable", "unsupported"];
const STREAM_OUTCOMES: [&str; 2] = ["complete", "degraded"];

#[derive(Debug)]
pub enum Error {
    Read(io::Error),
    Invalid { line: u64, message: String },
    Write(io::Error),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Read(_) => 2,
            Self::Invalid { .. } => 4,
            Self::Write(_) => 5,
        }
    }

    pub fn event_code(&self) -> &'static str {
        match self {
            Self::Read(_) => "stream_read_failed",
            Self::Invalid { .. } => "stream_invalid",
            Self::Write(_) => "stdout_write_failed",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Read(error) | Self::Write(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                )
        )
    }

    pub fn details(&self) -> Value {
        match self {
            Self::Invalid { line, .. } => json!({"line":line}),
            Self::Read(error) | Self::Write(error) => json!({
                "io_kind":format!("{:?}", error.kind()).to_ascii_lowercase(),
            }),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "read JSONL stream: {error}"),
            Self::Invalid { line, message } => {
                write!(formatter, "invalid JSONL stream at line {line}: {message}")
            }
            Self::Write(error) => write!(formatter, "write result: {error}"),
        }
    }
}

#[derive(Debug)]
pub(crate) struct CapabilitySummary {
    pub id: String,
    pub outcome: String,
    pub coverage: Value,
    pub limits_hit: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Summary {
    pub records: u64,
    pub bytes: u64,
    pub data_records: u64,
    pub command_id: String,
    pub invocation_kind: String,
    pub collector: Value,
    pub global_limits: Value,
    pub stream_outcome: String,
    pub stream_truncated: bool,
    pub planned_capabilities: Vec<String>,
    pub not_started_capabilities: Vec<String>,
    pub capabilities: Vec<CapabilitySummary>,
    pub outcomes: BTreeMap<String, Vec<String>>,
    pub diagnostic_records: u64,
    pub diagnostic_count: u64,
    pub diagnostics_by_status: BTreeMap<String, u64>,
    pub diagnostics_by_code: BTreeMap<String, u64>,
}

pub fn validate(path: &Path, output: &mut impl Write) -> Result<(), Error> {
    let summary = scan_path(path)?;
    write_document(
        output,
        &json!({
            "schema_version": SCHEMA_VERSION,
            "valid": true,
            "records": summary.records,
            "jsonl_bytes": summary.bytes,
        }),
    )
}

pub fn summarize(path: &Path, output: &mut impl Write) -> Result<(), Error> {
    let summary = scan_path(path)?;
    let started = summary
        .capabilities
        .iter()
        .map(|capability| capability.id.as_str())
        .collect::<Vec<_>>();

    let counts = json!({
        "planned":summary.planned_capabilities.len(),
        "started":started.len(),
        "not_started":summary.not_started_capabilities.len(),
    });

    write_document(
        output,
        &json!({
            "schema_version": SCHEMA_VERSION,
            "command_id": summary.command_id,
            "stream": {
                "collection_outcome": summary.stream_outcome,
                "records": summary.records,
                "jsonl_bytes": summary.bytes,
                "truncated": summary.stream_truncated,
            },
            "capabilities": {
                "planned": summary.planned_capabilities,
                "started": started,
                "not_started": summary.not_started_capabilities,
                "counts":counts,
                "by_outcome": summary.outcomes,
            },
            "diagnostics": {
                "records": summary.diagnostic_records,
                "reported_count": summary.diagnostic_count,
                "by_status": summary.diagnostics_by_status,
                "by_code": summary.diagnostics_by_code,
            },
        }),
    )
}

pub(crate) fn inspect(path: &Path) -> Result<Summary, Error> {
    scan_path(path)
}

fn write_document(output: &mut impl Write, value: &Value) -> Result<(), Error> {
    serde_json::to_writer_pretty(&mut *output, value)
        .map_err(|error| Error::Write(io::Error::other(error)))?;
    output.write_all(b"\n").map_err(Error::Write)
}

fn scan_path(path: &Path) -> Result<Summary, Error> {
    if path.as_os_str() == OsStr::new("-") {
        let stdin = io::stdin();
        scan(stdin.lock())
    } else {
        let file = File::open(path).map_err(Error::Read)?;
        scan(BufReader::new(file))
    }
}

fn scan(mut input: impl BufRead) -> Result<Summary, Error> {
    let mut summary = Summary {
        outcomes: OUTCOMES
            .into_iter()
            .map(|outcome| (outcome.to_string(), Vec::new()))
            .collect(),
        ..Summary::default()
    };
    let mut line = Vec::new();
    let mut line_number = 0_u64;
    let mut expected_seq = 0_u64;
    let mut bytes_before = 0_u64;
    let mut open_capability = None::<String>;
    let mut seen_capabilities = BTreeSet::new();
    let mut saw_start = false;
    let mut saw_end = false;
    let mut abort_reason = None::<String>;

    while read_record(&mut input, &mut line, line_number + 1)? {
        line_number += 1;
        if saw_end || abort_reason.is_some() {
            return invalid(line_number, "record follows terminal stream record");
        }
        let value: Value = serde_json::from_slice(&line).map_err(|error| Error::Invalid {
            line: line_number,
            message: format!("invalid JSON: {error}"),
        })?;
        let object = value
            .as_object()
            .ok_or_else(|| invalid_error(line_number, "record must be a JSON object"))?;

        if object.get("schema_version").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
            return invalid(
                line_number,
                format!("schema_version must be {SCHEMA_VERSION}"),
            );
        }
        if object.get("seq").and_then(Value::as_u64) != Some(expected_seq) {
            return invalid(line_number, format!("seq must be {expected_seq}"));
        }
        let record_type = required_string(object.get("type"), line_number, "type")?;

        match record_type {
            "stream_start" => {
                if expected_seq != 0 || saw_start {
                    return invalid(line_number, "stream_start must be the first record");
                }
                summary.command_id =
                    required_string(object.get("command_id"), line_number, "command_id")?
                        .to_string();
                summary.invocation_kind = required_string(
                    object.get("invocation_kind"),
                    line_number,
                    "invocation_kind",
                )?
                .to_string();
                summary.planned_capabilities = required_string_array(
                    object.get("planned_capabilities"),
                    line_number,
                    "planned_capabilities",
                )?;

                if summary.planned_capabilities.is_empty() {
                    return invalid(line_number, "planned_capabilities must not be empty");
                }
                if summary
                    .planned_capabilities
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len()
                    != summary.planned_capabilities.len()
                {
                    return invalid(line_number, "planned_capabilities contains duplicates");
                }

                summary.collector = object
                    .get("collector")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| invalid_error(line_number, "collector must be an object"))?;
                summary.global_limits = object
                    .get("global_limits")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| invalid_error(line_number, "global_limits must be an object"))?;

                saw_start = true;
            }
            "capability_start" => {
                require_started(saw_start, line_number)?;
                if open_capability.is_some() {
                    return invalid(line_number, "nested capability_start");
                }
                let capability =
                    required_string(object.get("capability"), line_number, "capability")?;
                if !seen_capabilities.insert(capability.to_string()) {
                    return invalid(line_number, format!("duplicate capability {capability}"));
                }
                if let Some(expected) = summary.planned_capabilities.get(summary.capabilities.len())
                    && expected != capability
                {
                    return invalid(
                        line_number,
                        format!(
                            "capability {capability} does not match planned capability {expected}"
                        ),
                    );
                }
                open_capability = Some(capability.to_string());
            }
            "data" => {
                require_capability(object, open_capability.as_deref(), line_number)?;
                if object.get("data").is_none() {
                    return invalid(line_number, "data record lacks data");
                }
                summary.data_records = summary.data_records.saturating_add(1);
            }
            "diagnostic" => {
                require_capability(object, open_capability.as_deref(), line_number)?;
                let diagnostic = object
                    .get("diagnostic")
                    .and_then(Value::as_object)
                    .ok_or_else(|| invalid_error(line_number, "diagnostic must be an object"))?;
                let status =
                    required_string(diagnostic.get("status"), line_number, "diagnostic.status")?;
                let code = required_string(diagnostic.get("code"), line_number, "diagnostic.code")?;
                let count = diagnostic
                    .get("count")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        invalid_error(line_number, "diagnostic.count must be an integer")
                    })?;

                summary.diagnostic_records += 1;
                summary.diagnostic_count = summary.diagnostic_count.saturating_add(count);
                *summary
                    .diagnostics_by_status
                    .entry(status.to_string())
                    .or_default() += count;
                *summary
                    .diagnostics_by_code
                    .entry(code.to_string())
                    .or_default() += count;
            }
            "capability_end" => {
                let capability =
                    require_capability(object, open_capability.as_deref(), line_number)?;
                verify_counters(object.get("counters"), bytes_before, line_number)?;
                let outcome = valid_capability_outcome(object.get("outcome"), line_number)?;
                let coverage = object
                    .get("coverage")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| {
                        invalid_error(line_number, "capability_end.coverage must be an object")
                    })?;
                let truncated = coverage
                    .get("truncated")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        invalid_error(
                            line_number,
                            "capability_end.coverage.truncated must be a boolean",
                        )
                    })?;
                let limits_hit = object
                    .get("limits_hit")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        invalid_error(line_number, "capability_end.limits_hit must be an array")
                    })?
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_owned).ok_or_else(|| {
                            invalid_error(
                                line_number,
                                "capability_end.limits_hit entries must be strings",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                summary
                    .outcomes
                    .get_mut(outcome)
                    .expect("all valid outcomes initialized")
                    .push(capability.to_string());
                summary.stream_truncated |= truncated;
                summary.capabilities.push(CapabilitySummary {
                    id: capability.to_string(),
                    outcome: outcome.to_string(),
                    coverage,
                    limits_hit,
                });
                open_capability = None;
            }
            "stream_end" => {
                require_started(saw_start, line_number)?;
                if open_capability.is_some() {
                    return invalid(line_number, "stream_end precedes capability_end");
                }
                verify_counters(object.get("counters"), bytes_before, line_number)?;
                summary.stream_outcome =
                    valid_stream_outcome(object.get("collection_outcome"), line_number)?.into();
                let truncated = object
                    .get("truncated")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        invalid_error(line_number, "stream_end.truncated must be a boolean")
                    })?;
                summary.not_started_capabilities = required_string_array(
                    object.get("not_started_capabilities"),
                    line_number,
                    "not_started_capabilities",
                )?;

                let observed_plan = summary
                    .capabilities
                    .iter()
                    .map(|capability| capability.id.as_str())
                    .chain(summary.not_started_capabilities.iter().map(String::as_str));

                if !observed_plan.eq(summary.planned_capabilities.iter().map(String::as_str)) {
                    return invalid(
                        line_number,
                        "started and not-started capabilities do not match the declared plan",
                    );
                }

                let expected_outcome = if summary.not_started_capabilities.is_empty()
                    && summary
                        .capabilities
                        .iter()
                        .all(|capability| capability.outcome == "complete")
                {
                    "complete"
                } else {
                    "degraded"
                };

                if summary.stream_outcome != expected_outcome {
                    return invalid(
                        line_number,
                        format!("stream_end.collection_outcome must be {expected_outcome}"),
                    );
                }
                summary.stream_truncated |= !summary.not_started_capabilities.is_empty();
                if truncated != summary.stream_truncated {
                    return invalid(
                        line_number,
                        format!("stream_end.truncated must be {}", summary.stream_truncated),
                    );
                }
                saw_end = true;
            }
            "stream_abort" => {
                require_started(saw_start, line_number)?;
                verify_counters(object.get("counters"), bytes_before, line_number)?;
                if object.get("complete").and_then(Value::as_bool) != Some(false) {
                    return invalid(line_number, "stream_abort.complete must be false");
                }
                abort_reason = Some(
                    required_string(object.get("reason"), line_number, "stream_abort.reason")?
                        .to_string(),
                );
            }
            other => return invalid(line_number, format!("unknown record type {other}")),
        }

        expected_seq += 1;
        bytes_before = bytes_before.saturating_add(line.len() as u64);
    }

    if !saw_start {
        return invalid(line_number.saturating_add(1), "missing stream_start");
    }
    if let Some(reason) = abort_reason {
        return invalid(
            line_number.saturating_add(1),
            format!("stream aborted: {reason}"),
        );
    }
    if !saw_end {
        return invalid(line_number.saturating_add(1), "missing stream_end");
    }

    summary.records = expected_seq;
    summary.bytes = bytes_before;
    Ok(summary)
}

fn read_record(
    input: &mut impl BufRead,
    line: &mut Vec<u8>,
    line_number: u64,
) -> Result<bool, Error> {
    line.clear();
    loop {
        let available = input.fill_buf().map_err(Error::Read)?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);

        if line.len().saturating_add(end) > MAX_LINE_BYTES as usize {
            return invalid(
                line_number,
                format!("record exceeds {MAX_LINE_BYTES} bytes"),
            );
        }

        let complete = available[end - 1] == b'\n';
        line.extend_from_slice(&available[..end]);
        input.consume(end);
        if complete {
            return Ok(true);
        }
    }
}

fn require_started(started: bool, line: u64) -> Result<(), Error> {
    if started {
        Ok(())
    } else {
        invalid(line, "stream_start must be the first record")
    }
}

fn require_capability<'a>(
    object: &'a serde_json::Map<String, Value>,
    open: Option<&str>,
    line: u64,
) -> Result<&'a str, Error> {
    let capability = required_string(object.get("capability"), line, "capability")?;
    match open {
        Some(expected) if capability == expected => Ok(capability),
        Some(expected) => invalid(
            line,
            format!("capability {capability} does not match open capability {expected}"),
        ),
        None => invalid(line, format!("{capability} has no open capability")),
    }
}

fn verify_counters(counters: Option<&Value>, bytes: u64, line: u64) -> Result<(), Error> {
    let counters = counters
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_error(line, "counters must be an object"))?;

    if counters.get("jsonl_bytes").and_then(Value::as_u64) != Some(bytes) {
        return invalid(line, format!("counters.jsonl_bytes must be {bytes}"));
    }
    Ok(())
}

fn required_string_array(
    value: Option<&Value>,
    line: u64,
    field: &str,
) -> Result<Vec<String>, Error> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_error(line, format!("{field} must be an array")))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_error(line, format!("{field} entries must be strings")))
        })
        .collect()
}

fn required_string<'a>(value: Option<&'a Value>, line: u64, field: &str) -> Result<&'a str, Error> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_error(line, format!("{field} must be a string")))
}

fn valid_capability_outcome(value: Option<&Value>, line: u64) -> Result<&str, Error> {
    valid_enum(value, line, "outcome", &OUTCOMES)
}

fn valid_stream_outcome(value: Option<&Value>, line: u64) -> Result<&str, Error> {
    valid_enum(value, line, "collection_outcome", &STREAM_OUTCOMES)
}

fn valid_enum<'a>(
    value: Option<&'a Value>,
    line: u64,
    field: &str,
    allowed: &[&str],
) -> Result<&'a str, Error> {
    let outcome = required_string(value, line, field)?;
    if allowed.contains(&outcome) {
        Ok(outcome)
    } else {
        invalid(line, format!("unknown {field} {outcome}"))
    }
}

fn invalid<T>(line: u64, message: impl Into<String>) -> Result<T, Error> {
    Err(invalid_error(line, message))
}

fn invalid_error(line: u64, message: impl Into<String>) -> Error {
    Error::Invalid {
        line,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn stream_fixture() -> Vec<Value> {
        let mut records = vec![json!({
            "type":"stream_start",
            "command_id":"fixture",
            "invocation_kind":"profile",
            "collector":{
                "name":"vzik",
                "version":"test",
                "build_id":"test",
                "target":"test-linux",
            },
            "planned_capabilities":[
                "fixture.complete",
                "fixture.partial",
                "fixture.unavailable",
                "fixture.unsupported"
            ],
            "global_limits":{},
        })];
        for outcome in OUTCOMES {
            let capability = format!("fixture.{outcome}");
            records.push(json!({
                "type":"capability_start",
                "capability":capability,
            }));
            if outcome == "partial" {
                records.push(json!({
                    "type":"diagnostic",
                    "capability":capability,
                    "diagnostic":{
                        "status":"permission_denied",
                        "code":"source_permission_denied",
                        "count":2,
                    },
                }));
            }
            records.push(json!({
                "type":"capability_end",
                "capability":capability,
                "outcome":outcome,
                "coverage":{
                    "observed":1,
                    "skipped":0,
                    "denied":0,
                    "vanished":0,
                    "truncated":false,
                },
                "limits_hit":[],
            }));
        }
        records.push(json!({
            "type":"stream_end",
            "collection_outcome":"degraded",
            "truncated":false,
            "not_started_capabilities":[],
        }));

        records
    }

    fn encode_fixture(records: &mut [Value]) -> Vec<u8> {
        let mut output = Vec::new();
        for (seq, record) in records.iter_mut().enumerate() {
            record["schema_version"] = json!(SCHEMA_VERSION);
            record["seq"] = json!(seq);
            if matches!(
                record["type"].as_str(),
                Some("capability_end" | "stream_end")
            ) {
                record["counters"] = json!({
                    "jsonl_bytes": output.len(),
                });
            }
            serde_json::to_writer(&mut output, &*record).expect("encode fixture record");
            output.push(b'\n');
        }
        output
    }

    #[test]
    fn validates_complete_protocol_structure_and_counters() {
        let fixture = encode_fixture(&mut stream_fixture());
        let summary = scan(Cursor::new(&fixture)).expect("validate fixture");

        assert_eq!(summary.records, 11);
        assert_eq!(summary.bytes, fixture.len() as u64);
        assert_eq!(summary.command_id, "fixture");
        assert_eq!(summary.planned_capabilities.len(), 4);
        assert!(summary.not_started_capabilities.is_empty());
    }

    #[test]
    fn summarizes_every_capability_outcome_and_diagnostics() {
        let summary =
            scan(Cursor::new(encode_fixture(&mut stream_fixture()))).expect("summarize fixture");

        for outcome in OUTCOMES {
            assert_eq!(
                summary.outcomes[outcome],
                [format!("fixture.{outcome}")],
                "{outcome} outcome"
            );
        }
        assert_eq!(summary.stream_outcome, "degraded");
        assert!(!summary.stream_truncated);
        assert_eq!(summary.diagnostic_records, 1);
        assert_eq!(summary.diagnostic_count, 2);
        assert_eq!(summary.diagnostics_by_status["permission_denied"], 2);
        assert_eq!(summary.diagnostics_by_code["source_permission_denied"], 2);
    }

    #[test]
    fn rejects_collection_outcome_inconsistent_with_capabilities() {
        for expected in ["complete", "degraded"] {
            let mut records = stream_fixture();
            if expected == "complete" {
                let terminal = records.pop().expect("stream end");
                records.truncate(3);
                records.push(terminal);
                records[0]["planned_capabilities"] = json!(["fixture.complete"]);
            }
            records.last_mut().expect("stream end")["collection_outcome"] = json!(expected);
            let summary = scan(Cursor::new(encode_fixture(&mut records)))
                .expect("honest collection outcome validates");
            assert_eq!(summary.stream_outcome, expected);

            records.last_mut().expect("stream end")["collection_outcome"] =
                json!(if expected == "complete" {
                    "degraded"
                } else {
                    "complete"
                });
            let error = scan(Cursor::new(encode_fixture(&mut records)))
                .err()
                .expect("reject inconsistent collection outcome");
            assert!(
                error
                    .to_string()
                    .contains(&format!("stream_end.collection_outcome must be {expected}"))
            );
        }
    }

    #[test]
    fn rejects_missing_collection_outcome_even_with_old_outcome_key() {
        for old_outcome in [None, Some("degraded")] {
            let mut records = stream_fixture();
            let terminal = records
                .last_mut()
                .expect("stream end")
                .as_object_mut()
                .expect("terminal object");
            terminal.remove("collection_outcome");
            if let Some(outcome) = old_outcome {
                terminal.insert("outcome".into(), json!(outcome));
            }
            let error = scan(Cursor::new(encode_fixture(&mut records)))
                .err()
                .expect("collection outcome is required without legacy fallback");
            assert!(
                error
                    .to_string()
                    .contains("collection_outcome must be a string")
            );
        }
    }

    #[test]
    fn rejects_terminal_truncation_inconsistent_with_acquisition() {
        for (capability_truncated, not_started) in [(false, false), (true, false), (false, true)] {
            let mut records = stream_fixture();
            let partial = records
                .iter_mut()
                .find(|record| {
                    record["type"] == "capability_end" && record["capability"] == "fixture.partial"
                })
                .expect("partial capability");
            partial["coverage"]["truncated"] = json!(capability_truncated);
            if capability_truncated {
                partial["limits_hit"] = json!(["max_items"]);
            }
            if not_started {
                records[0]["planned_capabilities"]
                    .as_array_mut()
                    .expect("planned capabilities")
                    .push(json!("fixture.not_started"));
                records.last_mut().expect("stream end")["not_started_capabilities"] =
                    json!(["fixture.not_started"]);
            }
            let expected_truncated = capability_truncated || not_started;
            records.last_mut().expect("stream end")["truncated"] = json!(expected_truncated);
            let summary = scan(Cursor::new(encode_fixture(&mut records)))
                .expect("honest acquisition state validates");
            assert_eq!(summary.stream_truncated, expected_truncated);

            records.last_mut().expect("stream end")["truncated"] = json!(!expected_truncated);
            let error = scan(Cursor::new(encode_fixture(&mut records)))
                .err()
                .expect("reject inconsistent terminal truncation");
            assert!(error.to_string().contains(&format!(
                "stream_end.truncated must be {expected_truncated}"
            )));
        }
    }

    #[test]
    fn rejects_schema_mismatches_and_sequence_gaps() {
        for (needle, replacement, message) in [
            (
                &b"\"schema_version\":4"[..],
                &b"\"schema_version\":3"[..],
                "schema_version must be 4",
            ),
            (&b"\"seq\":1"[..], &b"\"seq\":9"[..], "seq must be 1"),
        ] {
            let mut fixture = encode_fixture(&mut stream_fixture());
            let start = fixture
                .windows(needle.len())
                .position(|window| window == needle)
                .expect("find record field");
            fixture[start..start + needle.len()].copy_from_slice(replacement);

            let error = scan(Cursor::new(fixture))
                .err()
                .expect("reject invalid record");
            assert!(error.to_string().contains(message));
        }
    }
}
