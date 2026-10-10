use std::collections::{BTreeMap, BinaryHeap};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use rustix::fs::{FileType, Mode, OFlags, fstat, open};
use serde_json::{Map, Value, json};

use super::{
    NAMESPACES, emit, finish, output_limit, path_value, read_bounded, source_failure, text,
    unavailable,
};
use crate::cli::{CapabilityId, Invocation, Request};
use crate::procfs::{hex_u64, malformed, parse_u64};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, Outcome, ProtocolError, RecordSink,
    check_deadline,
};

const FIXED_SOURCE_BYTES: u64 = 512 << 10;
const PROCESS_SOURCE_BYTES: u64 = 64 << 10;
const PROCESS_DETAIL_VALUE_BYTES: usize = (crate::cli::MAX_LINE_BYTES / 32) as usize;
const MOUNT_SOURCE_BYTES: u64 = 16 << 20;
const RESOLVER_SOURCE_BYTES: u64 = 256 << 10;
const FILE_CHUNK_BYTES: usize = 48 << 10;

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match invocation.capability {
        CapabilityId::HostInfo => host_info(sink, deadline),
        CapabilityId::KernelInfo => kernel_info(sink, deadline),
        CapabilityId::ProcessList => {
            let Request::ItemLimit { max_items } = &invocation.request else {
                return Err(ProtocolError::Internal(
                    "process.list request mismatch".into(),
                ));
            };
            process_list(sink, deadline, *max_items)
        }
        CapabilityId::ProcessInspect => {
            let Request::ProcessInspect { pid } = &invocation.request else {
                return Err(ProtocolError::Internal(
                    "process.inspect request mismatch".into(),
                ));
            };
            process_inspect(sink, deadline, *pid)
        }
        CapabilityId::MountList => {
            let Request::ItemLimit { max_items } = &invocation.request else {
                return Err(ProtocolError::Internal(
                    "mount.list request mismatch".into(),
                ));
            };
            mount_list(sink, deadline, *max_items)
        }
        CapabilityId::NetworkResolvers => network_resolvers(sink, deadline),
        CapabilityId::FileStat => {
            let Request::FileStat { path } = &invocation.request else {
                return Err(ProtocolError::Internal("file.stat request mismatch".into()));
            };
            file_stat(sink, deadline, path)
        }
        CapabilityId::FileRead => {
            let Request::FileRead { path, max_bytes } = &invocation.request else {
                return Err(ProtocolError::Internal("file.read request mismatch".into()));
            };
            file_read(sink, deadline, path, *max_bytes)
        }
        _ => unreachable!("host dispatcher received unrelated capability"),
    }
}

fn host_info<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::HostInfo;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut data = Map::new();
    data.insert("os".into(), json!(std::env::consts::OS));
    data.insert("architecture".into(), json!(std::env::consts::ARCH));

    read_trimmed_field(
        sink,
        capability,
        deadline,
        Path::new("/proc/sys/kernel/hostname"),
        "hostname",
        &mut data,
        &mut coverage,
        &mut partial,
    )?;

    let mut release = None;
    let mut release_error = None;
    for path in [
        Path::new("/etc/os-release"),
        Path::new("/usr/lib/os-release"),
    ] {
        match read_bounded(path, FIXED_SOURCE_BYTES) {
            Ok(value) => {
                release = Some((path, value));
                break;
            }
            Err(error) => release_error = Some((path, error)),
        }
    }

    if let Some((_path, (bytes, truncated))) = release {
        coverage.scanned += bytes.len() as u64;
        if truncated {
            coverage.truncated = true;
            partial = true;
        }
        for (key, value) in parse_os_release(&bytes) {
            data.insert(key, text(&value));
        }
    } else if let Some((path, error)) = release_error {
        partial = true;
        source_failure(sink, capability, path, &error, &mut coverage)?;
    }

    match read_bounded(Path::new("/proc/uptime"), 4096) {
        Ok((bytes, _)) => {
            coverage.scanned += bytes.len() as u64;
            if let Some(raw) = bytes.split(|byte| byte.is_ascii_whitespace()).next()
                && let Ok(text) = std::str::from_utf8(raw)
                && let Ok(seconds) = text.parse::<f64>()
            {
                data.insert("uptime_seconds".into(), json!(seconds));
            } else {
                partial = true;
                malformed_source(sink, capability, Path::new("/proc/uptime"), &mut coverage)?;
            }
        }
        Err(error) => {
            partial = true;
            source_failure(
                sink,
                capability,
                Path::new("/proc/uptime"),
                &error,
                &mut coverage,
            )?;
        }
    }

    if !emit(sink, capability, Value::Object(data), &mut coverage)? {
        return Ok(output_limit(coverage));
    }
    Ok(finish(coverage, partial, None))
}

fn kernel_info<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::KernelInfo;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut data = Map::new();
    data.insert("os".into(), json!(std::env::consts::OS));
    data.insert("architecture".into(), json!(std::env::consts::ARCH));

    for (path, field) in [
        ("/proc/sys/kernel/ostype", "name"),
        ("/proc/sys/kernel/osrelease", "release"),
        ("/proc/version", "version"),
        ("/proc/cmdline", "command_line"),
    ] {
        read_trimmed_field(
            sink,
            capability,
            deadline,
            Path::new(path),
            field,
            &mut data,
            &mut coverage,
            &mut partial,
        )?;
    }

    if !emit(sink, capability, Value::Object(data), &mut coverage)? {
        return Ok(output_limit(coverage));
    }
    Ok(finish(coverage, partial, None))
}

fn process_list<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    max_items: usize,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::ProcessList;
    let mut coverage = Coverage::default();
    let self_pid = std::process::id();
    let capacity = max_items.saturating_sub(1);
    let mut lowest = BinaryHeap::<u32>::with_capacity(capacity.saturating_add(1));

    let directory = match fs::read_dir("/proc") {
        Ok(directory) => directory,
        Err(error) => {
            return unavailable(sink, capability, Path::new("/proc"), error, coverage);
        }
    };

    for entry in directory {
        check_deadline(deadline)?;
        coverage.scanned += 1;
        let Ok(entry) = entry else {
            coverage.skipped += 1;
            continue;
        };
        let Ok(pid) = entry
            .file_name()
            .as_bytes()
            .iter()
            .try_fold(0_u32, |value, byte| {
                byte.is_ascii_digit()
                    .then(|| {
                        value
                            .saturating_mul(10)
                            .saturating_add(u32::from(byte - b'0'))
                    })
                    .ok_or(())
            })
        else {
            continue;
        };
        if pid == 0 || pid == self_pid || capacity == 0 {
            continue;
        }
        if lowest.len() < capacity {
            lowest.push(pid);
        } else if lowest.peek().is_some_and(|largest| pid < *largest) {
            lowest.pop();
            lowest.push(pid);
            coverage.truncated = true;
        } else {
            coverage.truncated = true;
        }
    }

    let mut pids = lowest.into_vec();
    pids.sort_unstable();
    if max_items > 0 {
        pids.insert(0, self_pid);
    }

    let mut partial = coverage.truncated;
    for pid in pids {
        check_deadline(deadline)?;
        match process_summary(pid, pid == self_pid) {
            Ok((data, security_context_complete)) => {
                partial |= !security_context_complete;
                if !emit(sink, capability, data, &mut coverage)? {
                    return Ok(output_limit(coverage));
                }
            }
            Err(error) => {
                partial = true;
                let path = PathBuf::from(format!("/proc/{pid}/stat"));
                source_failure(sink, capability, &path, &error, &mut coverage)?;
            }
        }
    }

    let limit = coverage.truncated.then_some("max_items");
    Ok(finish(coverage, partial, limit))
}

fn process_summary(pid: u32, is_self: bool) -> io::Result<(Value, bool)> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let (stat, truncated) = read_bounded(&root.join("stat"), PROCESS_SOURCE_BYTES)?;
    if truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proc stat exceeds limit",
        ));
    }
    let (parsed_pid, name, state, ppid) = parse_proc_stat(&stat)?;

    let mut data = process_summary_fields(parsed_pid, text(name), text(state), ppid, is_self);
    let mut complete = true;

    match read_bounded(&root.join("status"), PROCESS_SOURCE_BYTES) {
        Ok((status, false)) => add_process_status(&mut data, &status),
        _ => complete = false,
    }

    match read_bounded(&root.join("cmdline"), PROCESS_SOURCE_BYTES) {
        Ok((cmdline, false)) => {
            data.insert("cmdline".into(), process_cmdline(&cmdline));
        }
        _ => complete = false,
    }

    match read_bounded(&root.join("cgroup"), PROCESS_SOURCE_BYTES) {
        Ok((cgroup, false)) => match process_cgroup_membership(&cgroup, usize::MAX) {
            Ok((value, _)) => {
                data.insert("cgroup_membership".into(), value);
            }
            Err(_) => complete = false,
        },
        _ => complete = false,
    }

    for (source, field) in [("exe", "exe"), ("cwd", "cwd"), ("root", "root")] {
        match fs::read_link(root.join(source)) {
            Ok(path) => {
                data.insert(field.into(), path_value(&path));
            }
            Err(_) => complete = false,
        }
    }

    let mut namespaces = Map::new();
    for name in NAMESPACES {
        match fs::read_link(root.join("ns").join(name)) {
            Ok(path) => {
                namespaces.insert(name.into(), path_value(&path));
            }
            Err(_) => complete = false,
        }
    }
    data.insert("namespaces".into(), Value::Object(namespaces));

    data.insert("security_context_complete".into(), json!(complete));

    Ok((Value::Object(data), complete))
}

fn process_summary_fields(
    pid: u32,
    name: Value,
    state: Value,
    ppid: u32,
    is_self: bool,
) -> Map<String, Value> {
    let mut data = Map::new();
    data.insert("pid".into(), json!(pid));
    data.insert("ppid".into(), json!(ppid));
    data.insert("name".into(), name);
    data.insert("state".into(), state);
    data.insert("is_self".into(), json!(is_self));
    data
}

fn process_cmdline(bytes: &[u8]) -> Value {
    Value::Array(
        bytes
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
            .map(text)
            .collect(),
    )
}

#[derive(Default)]
struct ProcessDetailCoverage {
    coverage: Coverage,
    sources: Map<String, Value>,
    limits_hit: Vec<&'static str>,
}

impl ProcessDetailCoverage {
    fn report(self, outcome: Outcome) -> CapabilityReport {
        CapabilityReport {
            outcome,
            coverage: self.coverage,
            limits_hit: self.limits_hit,
        }
    }

    fn partial(&self) -> bool {
        self.sources
            .values()
            .any(|source| source["availability"] != "available")
    }
}

fn process_inspect<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    pid: u32,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::ProcessInspect;
    let root = PathBuf::from(format!("/proc/{pid}"));
    let stat_path = root.join("stat");
    let mut detail = ProcessDetailCoverage::default();
    let Some(stat_bytes) = read_process_source(sink, deadline, "stat", &stat_path, &mut detail)?
    else {
        return Ok(detail.report(Outcome::Unavailable));
    };
    let Some(stat) = process_source_result(
        sink,
        "stat",
        &stat_path,
        parse_process_detail_stat(&stat_bytes),
        &mut detail,
    )?
    else {
        return Ok(detail.report(Outcome::Unavailable));
    };
    if stat.pid != pid {
        return process_raced(sink, &stat_path, detail);
    }
    let Some(resources) = process_source_result(
        sink,
        "stat",
        &stat_path,
        process_resources(
            &stat,
            rustix::param::page_size() as u64,
            rustix::param::clock_ticks_per_second(),
        ),
        &mut detail,
    )?
    else {
        return Ok(detail.report(Outcome::Unavailable));
    };

    let (name, name_truncated) = process_detail_text(stat.name);
    let mut data = process_summary_fields(
        pid,
        name,
        text(stat.state),
        stat.ppid,
        pid == std::process::id(),
    );
    if name_truncated {
        process_detail_limit(sink, "stat", &stat_path, "max_line_bytes", &mut detail)?;
    }
    data.insert("scope".into(), json!("collector_visible"));
    data.insert("resources".into(), resources);
    let mut security_context_complete = !name_truncated;

    let status_path = root.join("status");
    if let Some(bytes) = read_process_source(sink, deadline, "status", &status_path, &mut detail)? {
        if process_source_result(
            sink,
            "status",
            &status_path,
            validate_process_status(&bytes),
            &mut detail,
        )?
        .is_some()
        {
            if add_process_status_limited(&mut data, &bytes, PROCESS_DETAIL_VALUE_BYTES / 16) {
                security_context_complete = false;
                process_detail_limit(sink, "status", &status_path, "max_line_bytes", &mut detail)?;
            }
        } else {
            security_context_complete = false;
        }
    } else {
        security_context_complete = false;
    }

    let cmdline_path = root.join("cmdline");
    if let Some(bytes) = read_process_source(sink, deadline, "cmdline", &cmdline_path, &mut detail)?
    {
        if let Some((cmdline, truncated)) = process_source_result(
            sink,
            "cmdline",
            &cmdline_path,
            process_detail_cmdline(&bytes),
            &mut detail,
        )? {
            data.insert("cmdline".into(), cmdline);
            if truncated {
                security_context_complete = false;
                process_detail_limit(
                    sink,
                    "cmdline",
                    &cmdline_path,
                    "max_line_bytes",
                    &mut detail,
                )?;
            }
        } else {
            security_context_complete = false;
        }
    } else {
        security_context_complete = false;
    }

    let cgroup_path = root.join("cgroup");
    if let Some(bytes) = read_process_source(sink, deadline, "cgroup", &cgroup_path, &mut detail)? {
        if let Some((value, truncated)) = process_source_result(
            sink,
            "cgroup",
            &cgroup_path,
            process_cgroup_membership(&bytes, PROCESS_DETAIL_VALUE_BYTES),
            &mut detail,
        )? {
            data.insert("cgroup_membership".into(), value);
            if truncated {
                security_context_complete = false;
                process_detail_limit(sink, "cgroup", &cgroup_path, "max_line_bytes", &mut detail)?;
            }
        } else {
            security_context_complete = false;
        }
    } else {
        security_context_complete = false;
    }

    for source in ["exe", "cwd", "root"] {
        let path = root.join(source);
        check_deadline(deadline)?;
        if let Some(target) =
            process_source_result(sink, source, &path, fs::read_link(&path), &mut detail)?
        {
            let (value, truncated) = process_detail_text(target.as_os_str().as_bytes());
            data.insert(source.into(), value);
            if truncated {
                security_context_complete = false;
                process_detail_limit(sink, source, &path, "max_line_bytes", &mut detail)?;
            }
        } else {
            security_context_complete = false;
        }
    }

    let mut namespaces = Map::new();
    for name in NAMESPACES {
        let source = format!("ns.{name}");
        let path = root.join("ns").join(name);
        check_deadline(deadline)?;
        if let Some(target) =
            process_source_result(sink, &source, &path, fs::read_link(&path), &mut detail)?
        {
            let (value, truncated) = process_detail_text(target.as_os_str().as_bytes());
            namespaces.insert(name.into(), value);
            if truncated {
                security_context_complete = false;
                process_detail_limit(sink, &source, &path, "max_line_bytes", &mut detail)?;
            }
        } else {
            security_context_complete = false;
        }
    }
    data.insert("namespaces".into(), Value::Object(namespaces));
    data.insert(
        "security_context_complete".into(),
        json!(security_context_complete),
    );

    let limits_path = root.join("limits");
    if let Some(bytes) = read_process_source(sink, deadline, "limits", &limits_path, &mut detail)?
        && let Some((limits, truncated)) = process_source_result(
            sink,
            "limits",
            &limits_path,
            parse_process_limits(&bytes),
            &mut detail,
        )?
    {
        data.insert("rlimits".into(), limits);
        if truncated {
            process_detail_limit(sink, "limits", &limits_path, "max_line_bytes", &mut detail)?;
        }
    }

    let io_path = root.join("io");
    if let Some(bytes) = read_process_source(sink, deadline, "io", &io_path, &mut detail)?
        && let Some(value) =
            process_source_result(sink, "io", &io_path, parse_process_io(&bytes), &mut detail)?
    {
        data.insert("io".into(), value);
    }

    let Some(final_stat) = read_process_source(sink, deadline, "stat", &stat_path, &mut detail)?
    else {
        return Ok(detail.report(Outcome::Unavailable));
    };
    let Some(matches) = process_source_result(
        sink,
        "stat",
        &stat_path,
        process_stat_matches(pid, stat.start_ticks, &final_stat),
        &mut detail,
    )?
    else {
        return Ok(detail.report(Outcome::Unavailable));
    };
    if !matches {
        return process_raced(sink, &stat_path, detail);
    }
    if name_truncated {
        detail.sources["stat"]["availability"] = json!("truncated");
    }

    let partial = detail.partial();
    data.insert(
        "sources".into(),
        Value::Object(std::mem::take(&mut detail.sources)),
    );
    check_deadline(deadline)?;
    if !emit(sink, capability, Value::Object(data), &mut detail.coverage)? {
        return Ok(output_limit(detail.coverage));
    }
    Ok(detail.report(if partial {
        Outcome::Partial
    } else {
        Outcome::Complete
    }))
}

fn read_process_source<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    name: &str,
    path: &Path,
    detail: &mut ProcessDetailCoverage,
) -> Result<Option<Vec<u8>>, ProtocolError> {
    check_deadline(deadline)?;
    match read_bounded(path, PROCESS_SOURCE_BYTES) {
        Ok((bytes, truncated)) => {
            detail.coverage.scanned += bytes.len() as u64;
            if truncated {
                process_detail_limit(sink, name, path, "max_source_bytes", detail)?;
                Ok(None)
            } else {
                process_source_result(sink, name, path, Ok(bytes), detail)
            }
        }
        Err(error) => process_source_result(sink, name, path, Err(error), detail),
    }
}

fn process_source_result<W: Write, T>(
    sink: &mut RecordSink<'_, W>,
    name: &str,
    path: &Path,
    result: io::Result<T>,
    detail: &mut ProcessDetailCoverage,
) -> Result<Option<T>, ProtocolError> {
    let availability = match &result {
        Ok(_) => "available",
        Err(error) => match error.kind() {
            io::ErrorKind::NotFound => "not_found",
            io::ErrorKind::PermissionDenied => "permission_denied",
            io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => "malformed",
            _ => "io_error",
        },
    };
    detail.sources.insert(
        name.into(),
        json!({"source":path_value(path), "availability":availability}),
    );
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            source_failure(
                sink,
                CapabilityId::ProcessInspect,
                path,
                &error,
                &mut detail.coverage,
            )?;
            Ok(None)
        }
    }
}

fn process_detail_limit<W: Write>(
    sink: &mut RecordSink<'_, W>,
    name: &str,
    path: &Path,
    limit: &'static str,
    detail: &mut ProcessDetailCoverage,
) -> Result<(), ProtocolError> {
    detail.coverage.truncated = true;
    if !detail.limits_hit.contains(&limit) {
        detail.limits_hit.push(limit);
    }
    detail.sources.insert(
        name.into(),
        json!({"source":path_value(path), "availability":"truncated"}),
    );
    sink.diagnostic(
        CapabilityId::ProcessInspect.id(),
        "limit",
        "limit_reached",
        "limit_reached",
        path_value(path),
        if limit == "max_source_bytes" {
            "process source exceeds its bounded read limit"
        } else {
            "process source was shortened to fit its JSONL line budget"
        },
        1,
    )
}

fn process_raced<W: Write>(
    sink: &mut RecordSink<'_, W>,
    path: &Path,
    mut detail: ProcessDetailCoverage,
) -> Result<CapabilityReport, ProtocolError> {
    detail.coverage.skipped += 1;
    sink.diagnostic(
        CapabilityId::ProcessInspect.id(),
        "source",
        "raced",
        "source_raced",
        path_value(path),
        "process changed while its sources were being read; snapshot discarded",
        1,
    )?;
    Ok(detail.report(Outcome::Unavailable))
}

fn process_detail_text(bytes: &[u8]) -> (Value, bool) {
    // Non-UTF-8 display escaping plus base64 needs less than seven bytes per input byte.
    let maximum = PROCESS_DETAIL_VALUE_BYTES.saturating_sub(128) / 7;
    let length = bytes.len().min(maximum);
    (text(&bytes[..length]), length < bytes.len())
}

#[derive(Default)]
struct JsonBytes(usize);

impl Write for JsonBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json_bytes(value: &Value) -> io::Result<usize> {
    let mut writer = JsonBytes::default();
    serde_json::to_writer(&mut writer, value).map_err(io::Error::other)?;
    Ok(writer.0)
}

fn process_detail_cmdline(bytes: &[u8]) -> io::Result<(Value, bool)> {
    let mut values = Vec::new();
    let mut size = 2;
    let mut truncated = false;
    for argument in bytes
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
    {
        let (value, shortened) = process_detail_text(argument);
        let added = json_bytes(&value)? + usize::from(!values.is_empty());
        if size + added > PROCESS_DETAIL_VALUE_BYTES {
            truncated = true;
            break;
        }
        size += added;
        values.push(value);
        truncated |= shortened;
        if shortened {
            break;
        }
    }
    Ok((Value::Array(values), truncated))
}

fn mount_list<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    max_items: usize,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::MountList;
    let mut coverage = Coverage::default();
    let file = match File::open("/proc/self/mountinfo") {
        Ok(file) => file,
        Err(error) => {
            return unavailable(
                sink,
                capability,
                Path::new("/proc/self/mountinfo"),
                error,
                coverage,
            );
        }
    };

    let namespace = fs::read_link("/proc/self/ns/mnt")
        .ok()
        .map(|path| path_value(&path));

    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut partial = false;

    loop {
        check_deadline(deadline)?;
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(ProtocolError::Write)?;
        if read == 0 {
            break;
        }

        coverage.scanned += read as u64;
        if coverage.scanned > MOUNT_SOURCE_BYTES {
            coverage.truncated = true;
            partial = true;
            break;
        }

        if line.len() > 128 << 10 {
            coverage.skipped += 1;
            partial = true;
            malformed_source(
                sink,
                capability,
                Path::new("/proc/self/mountinfo"),
                &mut coverage,
            )?;
            continue;
        }

        while line
            .last()
            .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
        {
            line.pop();
        }

        let data = match parse_mountinfo(&line, namespace.clone()) {
            Ok(data) => data,
            Err(()) => {
                coverage.skipped += 1;
                partial = true;
                malformed_source(
                    sink,
                    capability,
                    Path::new("/proc/self/mountinfo"),
                    &mut coverage,
                )?;
                continue;
            }
        };

        if coverage.observed as usize >= max_items {
            coverage.truncated = true;
            partial = true;
            break;
        }

        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    let limit = coverage
        .truncated
        .then_some(if coverage.scanned > MOUNT_SOURCE_BYTES {
            "max_source_bytes"
        } else {
            "max_items"
        });
    Ok(finish(coverage, partial, limit))
}

fn network_resolvers<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::NetworkResolvers;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut any_source = false;
    for path in [
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/systemd/resolved.conf",
        "/run/systemd/resolve/resolv.conf",
        "/run/systemd/resolve/stub-resolv.conf",
    ] {
        check_deadline(deadline)?;
        let path = Path::new(path);
        let (bytes, truncated) = match read_bounded(path, RESOLVER_SOURCE_BYTES) {
            Ok(result) => result,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                coverage.vanished += 1;
                continue;
            }
            Err(error) => {
                partial = true;
                source_failure(sink, capability, path, &error, &mut coverage)?;
                continue;
            }
        };

        any_source = true;
        coverage.scanned += bytes.len() as u64;
        coverage.truncated |= truncated;
        partial |= truncated;

        for (line_number, raw) in bytes.split(|byte| *byte == b'\n').enumerate() {
            check_deadline(deadline)?;
            let line = raw
                .split(|byte| *byte == b'#' || *byte == b';')
                .next()
                .unwrap_or_default();
            let fields = line
                .split(|byte| byte.is_ascii_whitespace())
                .filter(|field| !field.is_empty())
                .collect::<Vec<_>>();

            if fields.is_empty() {
                continue;
            }
            let data = json!({
                "source":path_value(path),
                "line":line_number + 1,
                "name":text(fields[0]),
                "values":fields[1..].iter().map(|value| text(value)).collect::<Vec<_>>(),
            });

            if !emit(sink, capability, data, &mut coverage)? {
                return Ok(output_limit(coverage));
            }
        }
    }

    if !any_source {
        return unavailable(
            sink,
            capability,
            Path::new("/etc/resolv.conf"),
            io::Error::new(io::ErrorKind::NotFound, "no name-resolution source found"),
            coverage,
        );
    }

    let limit = coverage.truncated.then_some("max_source_bytes");
    Ok(finish(coverage, partial, limit))
}

fn file_stat<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    path: &Path,
) -> Result<CapabilityReport, ProtocolError> {
    check_deadline(deadline)?;
    let capability = CapabilityId::FileStat;
    let mut coverage = Coverage::default();
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return unavailable(sink, capability, path, error, coverage),
    };

    coverage.scanned = 1;
    let kind = if metadata.file_type().is_symlink() {
        "symlink"
    } else if metadata.is_file() {
        "regular"
    } else if metadata.is_dir() {
        "directory"
    } else {
        "special"
    };

    let mut data = json!({
        "path":path_value(path),
        "type":kind,
        "size":metadata.len(),
        "mode":metadata.mode(),
        "uid":metadata.uid(),
        "gid":metadata.gid(),
        "device":metadata.dev(),
        "inode":metadata.ino(),
        "modified_seconds":metadata.mtime(),
        "modified_nanoseconds":metadata.mtime_nsec(),
    });

    if metadata.file_type().is_symlink()
        && let Ok(target) = fs::read_link(path)
    {
        data["link_target"] = path_value(&target);
    }

    if !emit(sink, capability, data, &mut coverage)? {
        return Ok(output_limit(coverage));
    }
    Ok(CapabilityReport::complete(coverage))
}

fn file_read<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    path: &Path,
    max_bytes: u64,
) -> Result<CapabilityReport, ProtocolError> {
    check_deadline(deadline)?;
    let capability = CapabilityId::FileRead;
    let mut coverage = Coverage::default();
    let mut file = match open_regular(path) {
        Ok(file) => file,
        Err(error) => return unavailable(sink, capability, path, error, coverage),
    };

    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; FILE_CHUNK_BYTES];
    let mut eof = false;

    while offset < max_bytes {
        check_deadline(deadline)?;
        let remaining = usize::try_from((max_bytes - offset).min(FILE_CHUNK_BYTES as u64))
            .expect("chunk bound fits usize");
        let read = match file.read(&mut buffer[..remaining]) {
            Ok(read) => read,
            Err(error) => {
                return source_read_failure(sink, capability, path, error, coverage);
            }
        };

        if read == 0 {
            eof = true;
            break;
        }

        let bytes = &buffer[..read];
        let (encoding, content) = match std::str::from_utf8(bytes) {
            Ok(text) => ("utf-8", text.to_owned()),
            Err(_) => (
                "base64",
                base64::engine::general_purpose::STANDARD.encode(bytes),
            ),
        };

        let data = json!({
            "path":path_value(path),
            "offset":offset,
            "bytes":read,
            "encoding":encoding,
            "content":content,
            "eof":false,
        });

        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }

        offset += read as u64;
        coverage.scanned += read as u64;
    }

    if !eof {
        let mut probe = [0_u8; 1];
        eof = match file.read(&mut probe) {
            Ok(0) => true,
            Ok(_) => false,
            Err(error) => {
                return source_read_failure(sink, capability, path, error, coverage);
            }
        };
    }

    if eof {
        let data = json!({
            "path":path_value(path),
            "offset":offset,
            "bytes":0,
            "encoding":"utf-8",
            "content":"",
            "eof":true,
        });
        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    Ok(finish(coverage, !eof, (!eof).then_some("max_bytes")))
}

fn open_regular(path: &Path) -> io::Result<File> {
    let path_fd = open(
        path,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let stat = fstat(&path_fd)?;

    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file.read accepts only regular files",
        ));
    }

    let fd_path = format!("/proc/self/fd/{}", path_fd.as_raw_fd());
    let read_fd = open(
        fd_path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;

    Ok(File::from(read_fd))
}

#[allow(clippy::too_many_arguments)]
fn read_trimmed_field<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    deadline: ExecutionContext<'_>,
    path: &Path,
    field: &str,
    data: &mut Map<String, Value>,
    coverage: &mut Coverage,
    partial: &mut bool,
) -> Result<(), ProtocolError> {
    check_deadline(deadline)?;
    match read_bounded(path, FIXED_SOURCE_BYTES) {
        Ok((mut bytes, truncated)) => {
            while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
                bytes.pop();
            }
            coverage.scanned += bytes.len() as u64;
            coverage.truncated |= truncated;
            *partial |= truncated;
            data.insert(field.into(), text(&bytes));
        }
        Err(error) => {
            *partial = true;
            source_failure(sink, capability, path, &error, coverage)?;
        }
    }
    Ok(())
}

fn parse_os_release(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut output = BTreeMap::new();
    for raw in bytes.split(|byte| *byte == b'\n') {
        let Some(separator) = raw.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let Ok(key) = std::str::from_utf8(&raw[..separator]) else {
            continue;
        };
        if !matches!(key, "PRETTY_NAME" | "ID" | "VERSION_ID") {
            continue;
        }

        let mut value = raw[separator + 1..].to_vec();
        if value.len() >= 2
            && ((value[0] == b'"' && value.last() == Some(&b'"'))
                || (value[0] == b'\'' && value.last() == Some(&b'\'')))
        {
            value.remove(0);
            value.pop();
        }

        let field = match key {
            "PRETTY_NAME" => "distribution",
            "ID" => "distribution_id",
            "VERSION_ID" => "distribution_version",
            _ => unreachable!(),
        };
        output.insert(field.into(), value);
    }

    output
}

fn parse_proc_stat(bytes: &[u8]) -> io::Result<(u32, &[u8], &[u8], u32)> {
    let (pid, name, fields) = proc_stat_parts(bytes)?;
    let mut fields = fields
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let state = fields.next().ok_or_else(invalid_data)?;
    let ppid = parse_ascii_u32(fields.next().ok_or_else(invalid_data)?)?;
    fields.next().ok_or_else(invalid_data)?;
    Ok((pid, name, state, ppid))
}

fn proc_stat_parts(bytes: &[u8]) -> io::Result<(u32, &[u8], &[u8])> {
    let open = bytes
        .iter()
        .position(|byte| *byte == b'(')
        .ok_or_else(invalid_data)?;
    let close = bytes
        .windows(2)
        .rposition(|window| window == b") ")
        .ok_or_else(invalid_data)?;

    if open == 0 || close <= open {
        return Err(invalid_data());
    }

    let pid = parse_ascii_u32(bytes[..open].trim_ascii())?;
    Ok((pid, &bytes[open + 1..close], &bytes[close + 2..]))
}

struct ProcessStat<'a> {
    pid: u32,
    name: &'a [u8],
    state: &'a [u8],
    ppid: u32,
    start_ticks: u64,
    cpu_user_ticks: u64,
    cpu_system_ticks: u64,
    threads: u64,
    virtual_size_bytes: u64,
    rss_pages: u64,
}

fn parse_process_detail_stat(bytes: &[u8]) -> io::Result<ProcessStat<'_>> {
    let (pid, name, fields) = proc_stat_parts(bytes)?;
    let mut input = fields
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let mut fields = [b"".as_slice(); 22];
    for field in &mut fields {
        *field = input.next().ok_or_else(invalid_data)?;
    }
    if pid == 0 || fields[0].len() != 1 || !fields[0][0].is_ascii_alphabetic() {
        return Err(invalid_data());
    }
    Ok(ProcessStat {
        pid,
        name,
        state: fields[0],
        ppid: parse_ascii_u32(fields[1])?,
        start_ticks: parse_u64(fields[19])?,
        cpu_user_ticks: parse_u64(fields[11])?,
        cpu_system_ticks: parse_u64(fields[12])?,
        threads: parse_u64(fields[17])?,
        virtual_size_bytes: parse_u64(fields[20])?,
        rss_pages: parse_u64(fields[21])?,
    })
}

fn process_resources(
    stat: &ProcessStat<'_>,
    page_size_bytes: u64,
    clock_ticks_per_second: u64,
) -> io::Result<Value> {
    if page_size_bytes == 0 || clock_ticks_per_second == 0 {
        return Err(invalid_data());
    }
    let rss_bytes = stat
        .rss_pages
        .checked_mul(page_size_bytes)
        .ok_or_else(invalid_data)?;
    Ok(json!({
        "rss_bytes":rss_bytes,
        "virtual_size_bytes":stat.virtual_size_bytes,
        "threads":stat.threads,
        "cpu_user_ticks":stat.cpu_user_ticks,
        "cpu_system_ticks":stat.cpu_system_ticks,
        "clock_ticks_per_second":clock_ticks_per_second,
    }))
}

fn process_cgroup_membership(bytes: &[u8], budget: usize) -> io::Result<(Value, bool)> {
    let entries = crate::environment::parse_cgroup_membership(bytes)?;
    let mut values = Vec::new();
    let mut size = 2;
    for entry in entries {
        let mut fields = Map::new();
        match entry.hierarchy {
            crate::environment::CgroupHierarchy::Unified => {
                fields.insert("hierarchy_kind".into(), Value::String("unified".into()));
            }
            crate::environment::CgroupHierarchy::Legacy {
                hierarchy_id,
                controllers,
            } => {
                fields.insert("hierarchy_kind".into(), Value::String("legacy".into()));
                fields.insert("hierarchy_id".into(), Value::from(hierarchy_id));
                fields.insert(
                    "controllers".into(),
                    Value::Array(controllers.into_iter().map(Value::String).collect()),
                );
            }
        }
        fields.insert(
            "namespace_relative_path".into(),
            entry.namespace_relative_path,
        );
        let value = Value::Object(fields);
        if budget != usize::MAX {
            let added = json_bytes(&value)? + usize::from(!values.is_empty());
            if size + added > budget {
                return Ok((Value::Array(values), true));
            }
            size += added;
        }
        values.push(value);
    }
    Ok((Value::Array(values), false))
}

fn process_stat_matches(pid: u32, start_ticks: u64, bytes: &[u8]) -> io::Result<bool> {
    let current = parse_process_detail_stat(bytes)?;
    Ok(current.pid == pid && current.start_ticks == start_ticks)
}

fn validate_process_status(bytes: &[u8]) -> io::Result<()> {
    let mut uid = false;
    let mut gid = false;
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let separator = line
            .iter()
            .position(|byte| *byte == b':')
            .ok_or_else(invalid_data)?;
        let key = &line[..separator];
        let value = line[separator + 1..].trim_ascii();
        match key {
            b"Uid" | b"Gid" => {
                let mut count = 0;
                for field in value
                    .split(|byte| byte.is_ascii_whitespace())
                    .filter(|field| !field.is_empty())
                {
                    parse_ascii_u32(field)?;
                    count += 1;
                }
                if count != 4 {
                    return Err(invalid_data());
                }
                uid |= key == b"Uid";
                gid |= key == b"Gid";
            }
            b"Groups" => {
                for field in value
                    .split(|byte| byte.is_ascii_whitespace())
                    .filter(|field| !field.is_empty())
                {
                    parse_ascii_u32(field)?;
                }
            }
            b"CapInh" | b"CapPrm" | b"CapEff" | b"CapBnd" | b"CapAmb" => {
                if value.len() > 16 {
                    return Err(invalid_data());
                }
                hex_u64(value)?;
            }
            b"NoNewPrivs" if !matches!(value, b"0" | b"1") => {
                return Err(invalid_data());
            }
            b"Seccomp" | b"Seccomp_filters" => {
                parse_ascii_u32(value)?;
            }
            b"Umask"
                if value.is_empty()
                    || value.len() > 4
                    || value.iter().any(|byte| !(b'0'..=b'7').contains(byte)) =>
            {
                return Err(invalid_data());
            }
            _ => {}
        }
    }
    if !uid || !gid {
        return Err(invalid_data());
    }
    Ok(())
}

fn parse_process_limits(bytes: &[u8]) -> io::Result<(Value, bool)> {
    let mut lines = bytes
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|line| !line.is_empty());
    let header = lines.next().ok_or_else(invalid_data)?;
    let expected: &[&[u8]] = &[b"Limit", b"Soft", b"Limit", b"Hard", b"Limit", b"Units"];
    if !header
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .eq(expected.iter().copied())
    {
        return Err(invalid_data());
    }
    let mut values = Vec::new();
    let mut size = 2;
    let mut truncated = false;
    let mut output_full = false;
    let mut count = 0;
    for line in lines {
        let (before, last) = last_proc_field(line)?;
        let (before, hard, units) = if last == b"unlimited" || last.iter().all(u8::is_ascii_digit) {
            (before, last, None)
        } else {
            let (before, hard) = last_proc_field(before)?;
            (before, hard, Some(last))
        };
        let (name, soft) = last_proc_field(before)?;
        let soft = parse_process_limit_value(soft)?;
        let hard = parse_process_limit_value(hard)?;
        count += 1;
        if output_full {
            continue;
        }
        let (name, name_truncated) = process_detail_text(name);
        let mut value = Map::new();
        value.insert("name".into(), name);
        value.insert("soft".into(), soft);
        value.insert("hard".into(), hard);
        truncated |= name_truncated;
        if let Some(units) = units {
            let (units, units_truncated) = process_detail_text(units);
            value.insert("units".into(), units);
            truncated |= units_truncated;
        }
        let value = Value::Object(value);
        let added = json_bytes(&value)? + usize::from(!values.is_empty());
        if size + added > PROCESS_DETAIL_VALUE_BYTES {
            output_full = true;
            truncated = true;
            continue;
        }
        size += added;
        values.push(value);
    }
    if count == 0 {
        return Err(invalid_data());
    }
    Ok((Value::Array(values), truncated))
}

fn last_proc_field(bytes: &[u8]) -> io::Result<(&[u8], &[u8])> {
    let separator = bytes
        .iter()
        .rposition(|byte| byte.is_ascii_whitespace())
        .ok_or_else(invalid_data)?;
    let before = bytes[..separator].trim_ascii_end();
    let last = &bytes[separator + 1..];
    if before.is_empty() || last.is_empty() {
        return Err(invalid_data());
    }
    Ok((before, last))
}

fn parse_process_limit_value(bytes: &[u8]) -> io::Result<Value> {
    if bytes == b"unlimited" {
        Ok(json!("unlimited"))
    } else {
        parse_u64(bytes).map(Value::from)
    }
}

fn parse_process_io(bytes: &[u8]) -> io::Result<Value> {
    let mut data = Map::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let separator = line
            .iter()
            .position(|byte| *byte == b':')
            .ok_or_else(invalid_data)?;
        let key = match &line[..separator] {
            b"rchar" => "read_interface_bytes",
            b"wchar" => "write_interface_bytes",
            b"syscr" => "read_calls",
            b"syscw" => "write_calls",
            b"read_bytes" => "accounted_storage_read_bytes",
            b"write_bytes" => "accounted_storage_write_bytes",
            b"cancelled_write_bytes" => "accounted_cancelled_storage_write_bytes",
            _ => continue,
        };
        if data.contains_key(key) {
            return Err(invalid_data());
        }
        data.insert(key.into(), json!(parse_u64(&line[separator + 1..])?));
    }
    if data.len() != 7 {
        return Err(invalid_data());
    }
    Ok(Value::Object(data))
}

fn add_process_status(data: &mut Map<String, Value>, bytes: &[u8]) {
    add_process_status_limited(data, bytes, usize::MAX);
}

fn add_process_status_limited(
    data: &mut Map<String, Value>,
    bytes: &[u8],
    max_groups: usize,
) -> bool {
    let mut truncated = false;
    for line in bytes.split(|byte| *byte == b'\n') {
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        let key = &line[..separator];
        let value = line[separator + 1..].trim_ascii();
        match key {
            b"Uid" | b"Gid" => {
                let values = value
                    .split(|byte| byte.is_ascii_whitespace())
                    .filter_map(|value| parse_ascii_u32(value).ok())
                    .collect::<Vec<_>>();
                if values.len() == 4 {
                    let name = if key == b"Uid" { "uids" } else { "gids" };
                    data.insert(
                        name.into(),
                        json!({
                            "real":values[0],
                            "effective":values[1],
                            "saved":values[2],
                            "filesystem":values[3],
                        }),
                    );
                }
            }
            b"Groups" => {
                let mut groups = value
                    .split(|byte| byte.is_ascii_whitespace())
                    .filter_map(|value| parse_ascii_u32(value).ok())
                    .map(Value::from);
                let values = groups.by_ref().take(max_groups).collect();
                truncated |= groups.next().is_some();
                data.insert("groups".into(), Value::Array(values));
            }
            b"CapInh" | b"CapPrm" | b"CapEff" | b"CapBnd" | b"CapAmb" => {
                let name = match key {
                    b"CapInh" => "cap_inheritable",
                    b"CapPrm" => "cap_permitted",
                    b"CapEff" => "cap_effective",
                    b"CapBnd" => "cap_bounding",
                    b"CapAmb" => "cap_ambient",
                    _ => unreachable!(),
                };
                data.insert(name.into(), text(value));
            }
            b"NoNewPrivs" => {
                data.insert("no_new_privs".into(), json!(value == b"1"));
            }
            b"Seccomp" => {
                data.insert(
                    "seccomp".into(),
                    parse_ascii_u32(value).map_or(Value::Null, Value::from),
                );
            }
            b"Seccomp_filters" => {
                data.insert(
                    "seccomp_filters".into(),
                    parse_ascii_u32(value).map_or(Value::Null, Value::from),
                );
            }
            b"Umask" => {
                data.insert("umask".into(), text(value));
            }
            _ => {}
        }
    }
    truncated
}

fn parse_mountinfo(line: &[u8], namespace: Option<Value>) -> Result<Value, ()> {
    let fields = line
        .split(|byte| *byte == b' ')
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let separator = fields.iter().position(|field| *field == b"-").ok_or(())?;

    if separator < 6 || fields.len().saturating_sub(separator) < 4 {
        return Err(());
    }

    let mount_id = parse_ascii_u32(fields[0]).map_err(|_| ())?;
    let parent_id = parse_ascii_u32(fields[1]).map_err(|_| ())?;
    let options = fields[5]
        .split(|byte| *byte == b',')
        .chain(fields[separator + 3].split(|byte| *byte == b','))
        .map(text)
        .collect::<Vec<_>>();

    Ok(json!({
        "mount_id":mount_id,
        "parent_id":parent_id,
        "device":text(fields[2]),
        "root":text(&unescape_mount(fields[3])),
        "target":text(&unescape_mount(fields[4])),
        "filesystem":text(fields[separator + 1]),
        "source":text(&unescape_mount(fields[separator + 2])),
        "options":options,
        "optional_fields":fields[6..separator].iter().map(|field| text(field)).collect::<Vec<_>>(),
        "read_only":fields[5].split(|byte| *byte == b',').any(|value| value == b"ro"),
        "namespace":namespace,
    }))
}

fn unescape_mount(value: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] == b'\\'
            && index + 3 < value.len()
            && value[index + 1..index + 4].iter().all(u8::is_ascii_digit)
        {
            let octal = (value[index + 1] - b'0') * 64
                + (value[index + 2] - b'0') * 8
                + (value[index + 3] - b'0');
            output.push(octal);
            index += 4;
        } else {
            output.push(value[index]);
            index += 1;
        }
    }

    output
}

fn parse_ascii_u32(value: &[u8]) -> io::Result<u32> {
    if value.is_empty() || value.iter().any(|byte| !byte.is_ascii_digit()) {
        return Err(invalid_data());
    }

    value.iter().try_fold(0_u32, |current, byte| {
        current
            .checked_mul(10)
            .and_then(|number| number.checked_add(u32::from(byte - b'0')))
            .ok_or_else(invalid_data)
    })
}

fn invalid_data() -> io::Error {
    malformed("malformed source")
}

fn malformed_source<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    path: &Path,
    coverage: &mut Coverage,
) -> Result<(), ProtocolError> {
    let error = malformed("source did not match its bounded parser");
    source_failure(sink, capability, path, &error, coverage)
}

fn source_read_failure<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    path: &Path,
    error: io::Error,
    mut coverage: Coverage,
) -> Result<CapabilityReport, ProtocolError> {
    let outcome = if coverage.observed == 0 {
        Outcome::Unavailable
    } else {
        Outcome::Partial
    };

    source_failure(sink, capability, path, &error, &mut coverage)?;

    Ok(CapabilityReport {
        outcome,
        coverage,
        limits_hit: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_parser_preserves_parentheses_in_name() {
        let data = b"42 (worker ) name) S 7 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 99";
        let (pid, name, state, ppid) = parse_proc_stat(data).expect("parse stat");
        assert_eq!(
            (pid, name, state, ppid),
            (42, b"worker ) name".as_slice(), b"S".as_slice(), 7)
        );
    }

    fn process_detail_stat(pid: u32, start_ticks: u64, rss_pages: &str) -> Vec<u8> {
        format!(
            "{pid} (worker ) name) S 7 0 0 0 0 0 0 0 0 0 31 17 0 0 20 0 3 0 {start_ticks} 65536 {rss_pages}"
        )
        .into_bytes()
    }

    #[test]
    fn process_detail_parses_tick_counters_and_checked_memory_units() {
        let bytes = process_detail_stat(42, 99, "8");
        let stat = parse_process_detail_stat(&bytes).expect("parse process detail");
        assert_eq!(
            (stat.pid, stat.name, stat.state, stat.ppid),
            (42, b"worker ) name".as_slice(), b"S".as_slice(), 7),
        );
        assert_eq!(
            process_resources(&stat, 4096, 100).expect("resource units"),
            json!({
                "rss_bytes":32768,
                "virtual_size_bytes":65536,
                "threads":3,
                "cpu_user_ticks":31,
                "cpu_system_ticks":17,
                "clock_ticks_per_second":100,
            }),
        );
        assert!(parse_process_detail_stat(&process_detail_stat(42, 99, "-1")).is_err());
        assert!(parse_process_detail_stat(b"42 (gone) S 7").is_err());
        let bytes = process_detail_stat(42, 99, &u64::MAX.to_string());
        let stat = parse_process_detail_stat(&bytes).expect("parse large RSS");
        assert!(process_resources(&stat, 4096, 100).is_err());
    }

    #[test]
    fn process_detail_rejects_reused_pid_but_accepts_changed_counters() {
        assert!(
            process_stat_matches(42, 99, &process_detail_stat(42, 99, "12"))
                .expect("same process with changed RSS"),
        );
        assert!(
            !process_stat_matches(42, 99, &process_detail_stat(42, 100, "8")).expect("reused PID"),
        );
        assert!(
            !process_stat_matches(42, 99, &process_detail_stat(43, 99, "8"))
                .expect("different PID"),
        );
        assert!(process_stat_matches(42, 99, b"").is_err());
    }

    #[test]
    fn process_limits_preserve_unlimited_zero_and_unitless_priorities() {
        let (value, truncated) = parse_process_limits(
            b"Limit Soft Limit Hard Limit Units\n\
              Max cpu time unlimited unlimited seconds\n\
              Max open files 0 18446744073709551615 files\n\
              Max nice priority 0 0\n",
        )
        .expect("parse limits");
        assert!(!truncated);
        assert_eq!(
            value,
            json!([
                {"name":{"display":"Max cpu time"}, "soft":"unlimited", "hard":"unlimited", "units":{"display":"seconds"}},
                {"name":{"display":"Max open files"}, "soft":0, "hard":u64::MAX, "units":{"display":"files"}},
                {"name":{"display":"Max nice priority"}, "soft":0, "hard":0},
            ]),
        );
        for bytes in [
            b"Limit Soft Limit Hard Limit Units\n".as_slice(),
            b"wrong header\nMax open files 1 2 files\n",
            b"Limit Soft Limit Hard Limit Units\nMax open files -1 2 files\n",
            b"Limit Soft Limit Hard Limit Units\nMax open files 1 18446744073709551616 files\n",
            b"Limit Soft Limit Hard Limit Units\nMax open files 1 unknown files\n",
        ] {
            assert!(parse_process_limits(bytes).is_err(), "{bytes:?}");
        }
    }

    #[test]
    fn process_membership_budget_retains_only_complete_entries() {
        let first = json!([{
            "hierarchy_kind":"unified",
            "namespace_relative_path":{"display":"/visible:name"},
        }]);
        let budget = serde_json::to_vec(&first)
            .expect("serialize expected membership")
            .len();
        let source = b"0::/visible:name\n2:cpu,cpuacct:/other\n";
        assert_eq!(
            process_cgroup_membership(source, budget).expect("bounded membership"),
            (first, true),
        );
        assert_eq!(
            process_cgroup_membership(source, budget - 1).expect("omit oversized entry"),
            (json!([]), true),
        );
        assert!(process_cgroup_membership(b"0::/visible:name\ninvalid\n", budget).is_err());
    }

    #[test]
    fn process_io_rejects_missing_duplicate_negative_and_overflow_counters() {
        let bytes = b"rchar: 0\nwchar: 2\nsyscr: 3\nsyscw: 4\nread_bytes: 5\nwrite_bytes: 6\ncancelled_write_bytes: 18446744073709551615\n";
        assert_eq!(
            parse_process_io(bytes).expect("parse IO"),
            json!({
                "read_interface_bytes":0,
                "write_interface_bytes":2,
                "read_calls":3,
                "write_calls":4,
                "accounted_storage_read_bytes":5,
                "accounted_storage_write_bytes":6,
                "accounted_cancelled_storage_write_bytes":u64::MAX,
            }),
        );
        assert!(parse_process_io(b"rchar: 0\n").is_err());
        let valid = std::str::from_utf8(bytes).expect("ASCII IO fixture");
        for malformed in [
            format!("{valid}rchar: 1\n"),
            valid.replace("read_bytes: 5", "read_bytes: -1"),
            valid.replace("write_bytes: 6", "write_bytes: 18446744073709551616"),
        ] {
            assert!(
                parse_process_io(malformed.as_bytes()).is_err(),
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn process_status_does_not_silently_drop_malformed_security_values() {
        let valid = b"Uid: 1 2 3 4\nGid: 5 6 7 8\nGroups: 5 9\nCapEff: 00000000000000ff\nNoNewPrivs: 1\nSeccomp: 2\nUmask: 0022\n";
        validate_process_status(valid).expect("valid process status");
        for suffix in [
            b"Uid: 1 invalid 3 4\n".as_slice(),
            b"Groups: 5 invalid\n",
            b"CapEff: not-hex\n",
            b"NoNewPrivs: 2\n",
            b"Seccomp: unknown\n",
            b"Umask: 0088\n",
        ] {
            let mut malformed = valid.to_vec();
            malformed.extend_from_slice(suffix);
            assert!(validate_process_status(&malformed).is_err(), "{suffix:?}");
        }
        assert!(validate_process_status(b"Name: worker\n").is_err());
    }

    #[test]
    fn process_detail_bounds_groups_without_losing_visible_credentials() {
        let status = format!(
            "Uid: 1 2 3 4\nGid: 5 6 7 8\nGroups: {}\n",
            "4294967295 ".repeat(4096),
        );
        validate_process_status(status.as_bytes()).expect("large but valid status");
        let mut data = Map::new();
        assert!(add_process_status_limited(
            &mut data,
            status.as_bytes(),
            PROCESS_DETAIL_VALUE_BYTES / 16,
        ));
        let value = Value::Object(data);
        assert!(json_bytes(&value).expect("encoded status length") <= PROCESS_DETAIL_VALUE_BYTES);
        assert_eq!(
            value["uids"],
            json!({"real":1, "effective":2, "saved":3, "filesystem":4}),
        );
        let groups = value["groups"].as_array().expect("bounded groups");
        assert!(!groups.is_empty());
        assert!(groups.len() < 4096);
        assert!(groups.iter().all(|group| group == &Value::from(u32::MAX)));
    }

    #[test]
    fn process_detail_bounds_encoded_arguments_and_binary_prefixes() {
        let bytes = vec![0xff; PROCESS_SOURCE_BYTES as usize];
        let (value, truncated) = process_detail_text(&bytes);
        assert!(truncated);
        assert!(json_bytes(&value).expect("encoded binary length") <= PROCESS_DETAIL_VALUE_BYTES);
        let raw = base64::engine::general_purpose::STANDARD
            .decode(value["raw_base64"].as_str().expect("raw binary prefix"))
            .expect("decode binary prefix");
        assert!(!raw.is_empty());
        assert_eq!(raw, bytes[..raw.len()]);

        let bytes = b"x\0".repeat(PROCESS_SOURCE_BYTES as usize / 2);
        let (arguments, truncated) = process_detail_cmdline(&bytes).expect("bounded arguments");
        assert!(truncated);
        assert!(
            json_bytes(&arguments).expect("encoded argument length") <= PROCESS_DETAIL_VALUE_BYTES
        );
        let arguments = arguments.as_array().expect("argument array");
        assert!(!arguments.is_empty());
        assert!(arguments.len() < bytes.len() / 2);
        assert!(
            arguments
                .iter()
                .all(|argument| argument == &json!({"display":"x"}))
        );

        let mut limits = b"Limit Soft Limit Hard Limit Units\n".to_vec();
        limits.extend_from_slice(&b"Max open files 1 2 files\n".repeat(1024));
        let (value, truncated) = parse_process_limits(&limits).expect("bounded limits");
        assert!(truncated);
        assert!(json_bytes(&value).expect("encoded limits length") <= PROCESS_DETAIL_VALUE_BYTES);
        assert!(!value.as_array().expect("limit array").is_empty());
    }

    #[test]
    fn mountinfo_parser_decodes_kernel_escapes() {
        let value = parse_mountinfo(
            b"36 29 0:32 / /path\\040with\\040spaces rw,nosuid shared:7 - tmpfs tmpfs ro,size=1",
            None,
        )
        .expect("parse mountinfo");
        assert_eq!(value["target"]["display"], "/path with spaces");
        assert_eq!(value["read_only"], false);
    }

    #[test]
    fn regular_open_rejects_fifo_without_blocking() {
        use rustix::fs::{CWD, Mode, mkfifoat};
        let path = std::env::temp_dir().join(format!("vzik-fifo-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        mkfifoat(CWD, &path, Mode::RUSR | Mode::WUSR).expect("create fifo");
        let error = open_regular(&path).expect_err("fifo must be rejected");
        fs::remove_file(&path).expect("remove fifo");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
