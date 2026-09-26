use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{Execution, GlobalLimits, Invocation};
use crate::collect;

const TERMINAL_BYTE_RESERVE: u64 = 64 << 10;
const TERMINAL_RECORD_RESERVE: u64 = 16;
const PROTOCOL_VERSION: u64 = 3;
pub(crate) const DEGRADED_EXIT_CODE: i32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Complete,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Partial,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unavailable,
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StreamOutcome {
    Complete,
    Degraded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CollectionCompletion {
    pub outcome: StreamOutcome,
}

impl CollectionCompletion {
    pub(crate) const fn exit_code(self) -> i32 {
        match self.outcome {
            StreamOutcome::Complete => 0,
            StreamOutcome::Degraded => DEGRADED_EXIT_CODE,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Coverage {
    pub observed: u64,
    pub scanned: u64,
    pub skipped: u64,
    pub denied: u64,
    pub vanished: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct CapabilityReport {
    pub outcome: Outcome,
    pub coverage: Coverage,
    pub limits_hit: Vec<&'static str>,
}

impl CapabilityReport {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn complete(coverage: Coverage) -> Self {
        Self {
            outcome: Outcome::Complete,
            coverage,
            limits_hit: Vec::new(),
        }
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn unsupported() -> Self {
        Self {
            outcome: Outcome::Unsupported,
            coverage: Coverage::default(),
            limits_hit: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum ProtocolError {
    Deadline,
    Interrupted(usize),
    OutputLimit,
    RecordLimit,
    LineTooLong(usize),
    Encode(serde_json::Error),
    Write(std::io::Error),
    Internal(String),
}

impl ProtocolError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Deadline => 124,
            Self::Interrupted(signal) => 128 + i32::try_from(*signal).unwrap_or(1),
            Self::OutputLimit | Self::RecordLimit | Self::LineTooLong(_) | Self::Internal(_) => 4,
            Self::Encode(_) => 4,
            Self::Write(_) => 5,
        }
    }

    pub fn event_code(&self) -> &'static str {
        match self {
            Self::Deadline => "deadline_reached",
            Self::Interrupted(_) => "collection_interrupted",
            Self::OutputLimit => "output_limit_reached",
            Self::RecordLimit => "record_limit_reached",
            Self::LineTooLong(_) => "line_limit_reached",
            Self::Encode(_) => "record_encode_failed",
            Self::Write(_) => "stdout_write_failed",
            Self::Internal(_) => "collector_internal",
        }
    }

    pub fn is_acquisition_limit(&self) -> bool {
        matches!(self, Self::OutputLimit | Self::RecordLimit)
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::Deadline | Self::Interrupted(_))
    }

    pub fn details(&self) -> Value {
        match self {
            Self::Interrupted(signal) => json!({"signal":signal}),
            Self::LineTooLong(bytes) => json!({"record_bytes":bytes}),
            _ => json!({}),
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deadline => formatter.write_str("collector deadline reached"),
            Self::Interrupted(signal) => {
                write!(formatter, "collector interrupted by signal {signal}")
            }
            Self::OutputLimit => formatter.write_str("JSONL byte limit reached"),
            Self::RecordLimit => formatter.write_str("JSONL record limit reached"),
            Self::LineTooLong(bytes) => write!(formatter, "JSONL record is {bytes} bytes"),
            Self::Encode(error) => write!(formatter, "encode JSONL record: {error}"),
            Self::Write(error) => write!(formatter, "write JSONL record: {error}"),
            Self::Internal(error) => formatter.write_str(error),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ExecutionContext<'a> {
    deadline: Instant,
    interruption: Option<&'a AtomicUsize>,
}

impl ExecutionContext<'_> {
    #[cfg(target_os = "linux")]
    pub(crate) fn remaining(self) -> Result<Duration, ProtocolError> {
        check_deadline(self)?;
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(ProtocolError::Deadline)
    }
}

pub(crate) fn execute_controlled(
    writer: &mut impl Write,
    execution: Execution,
    interruption: Option<&AtomicUsize>,
) -> Result<CollectionCompletion, ProtocolError> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(execution.limits.deadline_seconds))
        .ok_or_else(|| ProtocolError::Internal("deadline overflow".into()))?;
    let context = ExecutionContext {
        deadline,
        interruption,
    };

    let planned_capabilities = execution
        .invocations
        .iter()
        .map(|invocation| invocation.capability.id())
        .collect::<Vec<_>>();
    let mut sink = RecordSink::new(writer, execution.limits);

    sink.emit(
        json!({
            "type":"stream_start",
            "collector":{
                "name":"vzik",
                "version":env!("CARGO_PKG_VERSION"),
                "build_id":concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION")),
                "target":format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            },
            "invocation_kind":execution.invocation_kind,
            "command_id":execution.command_id,
            "planned_capabilities":planned_capabilities,
            "global_limits":execution.limits,
        }),
        Priority::Data,
    )?;

    let mut degraded = false;
    let mut total = Coverage::default();
    let mut next_unstarted = 0;
    for (index, invocation) in execution.invocations.iter().enumerate() {
        if let Err(error) = check_deadline(context) {
            abort_stream(&mut sink, &error, None)?;
            return Err(error);
        }
        match run_capability(&mut sink, invocation, context, &mut degraded, &mut total) {
            Ok(CapabilityRun::Continue) => next_unstarted = index + 1,
            Ok(CapabilityRun::StopAfter) => {
                next_unstarted = index + 1;
                break;
            }
            Ok(CapabilityRun::StopBefore) => {
                next_unstarted = index;
                break;
            }
            Err(error @ (ProtocolError::Deadline | ProtocolError::Interrupted(_))) => {
                abort_stream(&mut sink, &error, Some(invocation.capability.id()))?;
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }

    if let Err(error) = check_deadline(context) {
        abort_stream(&mut sink, &error, None)?;
        return Err(error);
    }

    let not_started_capabilities = &planned_capabilities[next_unstarted..];
    degraded |= !not_started_capabilities.is_empty();
    let outcome = if degraded {
        StreamOutcome::Degraded
    } else {
        StreamOutcome::Complete
    };

    let before_end = sink.counters();
    sink.emit(
        json!({
            "type":"stream_end",
            "outcome":outcome,
            "counters":before_end,
            "coverage":total,
            "not_started_capabilities":not_started_capabilities,
        }),
        Priority::Terminal,
    )?;
    sink.flush()?;

    Ok(CollectionCompletion { outcome })
}

#[derive(Clone, Copy)]
enum CapabilityRun {
    Continue,
    StopAfter,
    StopBefore,
}

fn run_capability<W: Write>(
    sink: &mut RecordSink<'_, W>,
    invocation: &Invocation,
    context: ExecutionContext<'_>,
    degraded: &mut bool,
    total: &mut Coverage,
) -> Result<CapabilityRun, ProtocolError> {
    let start = json!({
        "type":"capability_start",
        "capability":invocation.capability.id(),
        "request":invocation.request.normalized(),
    });

    match sink.emit(start, Priority::Data) {
        Ok(()) => {}
        Err(error) if error.is_acquisition_limit() => {
            *degraded = true;
            total.truncated = true;
            return Ok(CapabilityRun::StopBefore);
        }
        Err(error) => return Err(error),
    }

    let report = collect::run(invocation, sink, context)?;
    check_deadline(context)?;
    if report.outcome != Outcome::Complete {
        *degraded = true;
    }

    total.observed = total.observed.saturating_add(report.coverage.observed);
    total.scanned = total.scanned.saturating_add(report.coverage.scanned);
    total.skipped = total.skipped.saturating_add(report.coverage.skipped);
    total.denied = total.denied.saturating_add(report.coverage.denied);
    total.vanished = total.vanished.saturating_add(report.coverage.vanished);
    total.truncated |= report.coverage.truncated;

    let global_limit_reached = report.limits_hit.contains(&"max_output_bytes_or_records");
    for limit in &report.limits_hit {
        sink.diagnostic_reserved(
            invocation.capability.id(),
            "limit",
            "limit_reached",
            "limit_reached",
            json!({"display":limit}),
            "acquisition stopped at the effective limit",
            1,
        )?;
    }

    let counters = sink.counters();
    sink.emit(
        json!({
            "type":"capability_end",
            "capability":invocation.capability.id(),
            "outcome":report.outcome,
            "counters":counters,
            "coverage":report.coverage,
            "truncated":report.coverage.truncated,
            "limits_hit":report.limits_hit,
        }),
        Priority::Terminal,
    )?;

    Ok(if global_limit_reached {
        CapabilityRun::StopAfter
    } else {
        CapabilityRun::Continue
    })
}

fn abort_stream(
    sink: &mut RecordSink<'_, impl Write>,
    error: &ProtocolError,
    active_capability: Option<&str>,
) -> Result<(), ProtocolError> {
    let reason = match error {
        ProtocolError::Deadline => "deadline",
        ProtocolError::Interrupted(_) => "signal",
        _ => return Ok(()),
    };
    let counters = sink.counters();

    sink.emit(
        json!({
            "type":"stream_abort",
            "complete":false,
            "reason":reason,
            "active_capability":active_capability,
            "details":error.details(),
            "counters":counters,
        }),
        Priority::Terminal,
    )?;
    sink.flush()
}

pub struct RecordSink<'a, W> {
    writer: &'a mut W,
    limits: GlobalLimits,
    seq: u64,
    bytes: u64,
    records: u64,
}

impl<'a, W: Write> RecordSink<'a, W> {
    fn new(writer: &'a mut W, limits: GlobalLimits) -> Self {
        Self {
            writer,
            limits,
            seq: 0,
            bytes: 0,
            records: 0,
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn data(
        &mut self,
        capability: &str,
        data_kind: &str,
        provider: &str,
        data: Value,
    ) -> Result<(), ProtocolError> {
        self.emit(
            json!({
                "type":"data",
                "capability":capability,
                "data_kind":data_kind,
                "provider":provider,
                "data":data,
            }),
            Priority::Data,
        )
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[allow(clippy::too_many_arguments)]
    pub fn diagnostic(
        &mut self,
        capability: &str,
        kind: &str,
        status: &str,
        code: &str,
        source: Value,
        message: &str,
        count: u64,
    ) -> Result<(), ProtocolError> {
        self.diagnostic_provider(
            capability, kind, status, code, "native", source, message, count,
        )
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[allow(clippy::too_many_arguments)]
    pub fn diagnostic_provider(
        &mut self,
        capability: &str,
        kind: &str,
        status: &str,
        code: &str,
        provider: &str,
        source: Value,
        message: &str,
        count: u64,
    ) -> Result<(), ProtocolError> {
        self.emit(
            diagnostic_record(
                capability, kind, status, code, provider, source, message, count,
            ),
            Priority::Data,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn diagnostic_reserved(
        &mut self,
        capability: &str,
        kind: &str,
        status: &str,
        code: &str,
        source: Value,
        message: &str,
        count: u64,
    ) -> Result<(), ProtocolError> {
        self.emit(
            diagnostic_record(
                capability, kind, status, code, "native", source, message, count,
            ),
            Priority::Terminal,
        )
    }

    fn emit(&mut self, mut value: Value, priority: Priority) -> Result<(), ProtocolError> {
        value["schema_version"] = json!(PROTOCOL_VERSION);
        value["seq"] = json!(self.seq);
        let mut line = serde_json::to_vec(&value).map_err(ProtocolError::Encode)?;
        line.push(b'\n');

        if line.len() as u64 > self.limits.max_line_bytes {
            return Err(ProtocolError::LineTooLong(line.len()));
        }

        let (byte_limit, record_limit) = match priority {
            Priority::Data => (
                self.limits
                    .max_output_bytes
                    .saturating_sub(TERMINAL_BYTE_RESERVE),
                self.limits
                    .max_records
                    .saturating_sub(TERMINAL_RECORD_RESERVE),
            ),
            Priority::Terminal => (self.limits.max_output_bytes, self.limits.max_records),
        };

        if self.bytes.saturating_add(line.len() as u64) > byte_limit {
            return Err(ProtocolError::OutputLimit);
        }

        if self.records.saturating_add(1) > record_limit {
            return Err(ProtocolError::RecordLimit);
        }

        self.writer.write_all(&line).map_err(ProtocolError::Write)?;
        self.bytes += line.len() as u64;
        self.records += 1;
        self.seq += 1;

        Ok(())
    }

    fn counters(&self) -> Value {
        json!({"records":self.records, "jsonl_bytes":self.bytes})
    }

    fn flush(&mut self) -> Result<(), ProtocolError> {
        self.writer.flush().map_err(ProtocolError::Write)
    }
}

#[allow(clippy::too_many_arguments)]
fn diagnostic_record(
    capability: &str,
    kind: &str,
    status: &str,
    code: &str,
    provider: &str,
    source: Value,
    message: &str,
    count: u64,
) -> Value {
    json!({
        "type":"diagnostic",
        "capability":capability,
        "diagnostic":{
            "kind":kind,
            "status":status,
            "code":code,
            "provider":provider,
            "source":source,
            "count":count,
            "message":message.chars().take(512).collect::<String>(),
        }
    })
}

#[derive(Clone, Copy)]
enum Priority {
    Data,
    Terminal,
}

pub(crate) fn check_deadline(context: ExecutionContext<'_>) -> Result<(), ProtocolError> {
    if let Some(interruption) = context.interruption {
        let signal = interruption.load(Ordering::Relaxed);
        if signal != 0 {
            return Err(ProtocolError::Interrupted(signal));
        }
    }

    if Instant::now() >= context.deadline {
        Err(ProtocolError::Deadline)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::cli::{self, Action};

    #[test]
    fn profile_record_limit_still_closes_the_stream() {
        let args = ["vzik", "collect", "--max-records", "32"]
            .into_iter()
            .map(OsString::from)
            .collect();
        let Action::Execute(execution) = cli::parse(args).expect("parse collect") else {
            panic!("collect must execute");
        };

        let mut output = Vec::new();
        let completion =
            execute_controlled(&mut output, execution, None).expect("bounded collection");
        let records = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).expect("JSON record"))
            .collect::<Vec<_>>();

        let planned = records[0]["planned_capabilities"]
            .as_array()
            .expect("planned capabilities")
            .iter()
            .map(|value| value.as_str().expect("planned capability"))
            .collect::<Vec<_>>();
        let completed = records
            .iter()
            .filter(|record| record["type"] == "capability_end")
            .map(|record| record["capability"].as_str().expect("completed capability"))
            .collect::<Vec<_>>();
        let not_started = records.last().expect("stream end")["not_started_capabilities"]
            .as_array()
            .expect("not-started capabilities")
            .iter()
            .map(|value| value.as_str().expect("not-started capability"))
            .collect::<Vec<_>>();
        assert_eq!(
            completed
                .iter()
                .chain(not_started.iter())
                .copied()
                .collect::<Vec<_>>(),
            planned
        );
        assert!(!not_started.is_empty());
        assert_eq!(completion.outcome, StreamOutcome::Degraded);

        assert!(records.len() <= 32);
        assert_eq!(
            records.last().and_then(|value| value["type"].as_str()),
            Some("stream_end")
        );
        assert_eq!(
            records.last().and_then(|value| value["outcome"].as_str()),
            Some("degraded")
        );
        assert_eq!(
            records
                .last()
                .and_then(|value| value["coverage"]["truncated"].as_bool()),
            Some(true)
        );
    }
}
