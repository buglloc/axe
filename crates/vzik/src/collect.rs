use std::io::Write;

use base64::Engine as _;
#[cfg(target_os = "linux")]
use serde_json::Map;
use serde_json::{Value, json};

use crate::cli::Invocation;
#[cfg(target_os = "linux")]
use crate::cli::{CapabilityId, Request};
use crate::protocol::{CapabilityReport, ExecutionContext, ProtocolError, RecordSink};
#[cfg(target_os = "linux")]
use crate::protocol::{Coverage, Outcome, check_deadline};

pub fn linux_bytes(bytes: &[u8]) -> Value {
    if let Ok(value) = std::str::from_utf8(bytes) {
        json!({"display":value})
    } else {
        let mut display = String::with_capacity(bytes.len());

        for &byte in bytes {
            if byte.is_ascii_graphic() || byte == b' ' {
                display.push(char::from(byte));
            } else {
                use std::fmt::Write as _;
                write!(display, "\\x{byte:02x}").expect("write to string");
            }
        }

        json!({
            "display":display,
            "raw_base64":base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }
}

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    #[cfg(target_os = "linux")]
    {
        linux::run(invocation, sink, deadline)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (invocation, sink, deadline);
        Ok(CapabilityReport::unsupported())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::{BTreeMap, BinaryHeap};
    use std::fs::{self, File};
    use std::io::{self, BufRead, BufReader, Read};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    use rustix::fs::{FileType, Mode, OFlags, fstat, open};

    use super::*;

    const FIXED_SOURCE_BYTES: u64 = 512 << 10;
    const PROCESS_SOURCE_BYTES: u64 = 64 << 10;
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
            _ => crate::inventory::run(invocation, sink, deadline),
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
                data.insert(key, linux_bytes(&value));
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

        if !emit_data(sink, capability, Value::Object(data))? {
            return Ok(limit_report(coverage));
        }

        coverage.observed = 1;
        Ok(report_from_coverage(coverage, partial))
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

        if !emit_data(sink, capability, Value::Object(data))? {
            return Ok(limit_report(coverage));
        }

        coverage.observed = 1;
        Ok(report_from_coverage(coverage, partial))
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
                    if !emit_data(sink, capability, data)? {
                        return Ok(limit_report(coverage));
                    }
                    coverage.observed += 1;
                }
                Err(error) => {
                    partial = true;
                    let path = PathBuf::from(format!("/proc/{pid}/stat"));
                    source_failure(sink, capability, &path, &error, &mut coverage)?;
                }
            }
        }

        let mut report = report_from_coverage(coverage, partial);
        if report.coverage.truncated {
            report.limits_hit.push("max_items");
        }

        Ok(report)
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

        let mut data = Map::new();
        data.insert("pid".into(), json!(parsed_pid));
        data.insert("ppid".into(), json!(ppid));
        data.insert("name".into(), linux_bytes(name));
        data.insert("state".into(), linux_bytes(state));
        data.insert("is_self".into(), json!(is_self));
        let mut complete = true;

        match read_bounded(&root.join("status"), PROCESS_SOURCE_BYTES) {
            Ok((status, false)) => add_process_status(&mut data, &status),
            _ => complete = false,
        }

        match read_bounded(&root.join("cmdline"), PROCESS_SOURCE_BYTES) {
            Ok((cmdline, false)) => {
                data.insert(
                    "cmdline".into(),
                    Value::Array(
                        cmdline
                            .split(|byte| *byte == 0)
                            .filter(|value| !value.is_empty())
                            .map(linux_bytes)
                            .collect(),
                    ),
                );
            }
            _ => complete = false,
        }

        match read_bounded(&root.join("cgroup"), PROCESS_SOURCE_BYTES) {
            Ok((cgroup, false)) => {
                data.insert("cgroup".into(), linux_bytes(trim_ascii(&cgroup)));
            }
            _ => complete = false,
        }

        for (source, field) in [("exe", "exe"), ("cwd", "cwd"), ("root", "root")] {
            match fs::read_link(root.join(source)) {
                Ok(path) => {
                    data.insert(field.into(), linux_bytes(path.as_os_str().as_bytes()));
                }
                Err(_) => complete = false,
            }
        }

        let mut namespaces = Map::new();
        for name in ["cgroup", "ipc", "mnt", "net", "pid", "user", "uts"] {
            match fs::read_link(root.join("ns").join(name)) {
                Ok(path) => {
                    namespaces.insert(name.into(), linux_bytes(path.as_os_str().as_bytes()));
                }
                Err(_) => complete = false,
            }
        }
        data.insert("namespaces".into(), Value::Object(namespaces));

        data.insert("security_context_complete".into(), json!(complete));

        Ok((Value::Object(data), complete))
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
            .map(|path| linux_bytes(path.as_os_str().as_bytes()));

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

            if !emit_data(sink, capability, data)? {
                return Ok(limit_report(coverage));
            }

            coverage.observed += 1;
        }

        let mut report = report_from_coverage(coverage, partial);
        if report.coverage.truncated {
            report
                .limits_hit
                .push(if report.coverage.scanned > MOUNT_SOURCE_BYTES {
                    "max_source_bytes"
                } else {
                    "max_items"
                });
        }

        Ok(report)
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
                    "source":linux_bytes(path.as_os_str().as_bytes()),
                    "line":line_number + 1,
                    "name":linux_bytes(fields[0]),
                    "values":fields[1..].iter().map(|value| linux_bytes(value)).collect::<Vec<_>>(),
                });

                if !emit_data(sink, capability, data)? {
                    return Ok(limit_report(coverage));
                }
                coverage.observed += 1;
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

        let mut report = report_from_coverage(coverage, partial);
        if report.coverage.truncated {
            report.limits_hit.push("max_source_bytes");
        }
        Ok(report)
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
            "path":linux_bytes(path.as_os_str().as_bytes()),
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
            data["link_target"] = linux_bytes(target.as_os_str().as_bytes());
        }

        if !emit_data(sink, capability, data)? {
            return Ok(limit_report(coverage));
        }

        coverage.observed = 1;
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
                "path":linux_bytes(path.as_os_str().as_bytes()),
                "offset":offset,
                "bytes":read,
                "encoding":encoding,
                "content":content,
                "eof":false,
            });

            if !emit_data(sink, capability, data)? {
                return Ok(limit_report(coverage));
            }

            offset += read as u64;
            coverage.observed += 1;
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
                "path":linux_bytes(path.as_os_str().as_bytes()),
                "offset":offset,
                "bytes":0,
                "encoding":"utf-8",
                "content":"",
                "eof":true,
            });
            if !emit_data(sink, capability, data)? {
                return Ok(limit_report(coverage));
            }
            coverage.observed += 1;
        }

        coverage.truncated = !eof;
        let mut report = report_from_coverage(coverage, !eof);

        if !eof {
            report.limits_hit.push("max_bytes");
        }

        Ok(report)
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
                data.insert(field.into(), linux_bytes(&bytes));
            }
            Err(error) => {
                *partial = true;
                source_failure(sink, capability, path, &error, coverage)?;
            }
        }
        Ok(())
    }

    fn read_bounded(path: &Path, maximum: u64) -> io::Result<(Vec<u8>, bool)> {
        let file = File::open(path)?;
        let mut bytes = Vec::with_capacity(usize::try_from(maximum.min(64 << 10)).unwrap_or(0));

        file.take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)?;

        let truncated = bytes.len() as u64 > maximum;
        if truncated {
            bytes.truncate(usize::try_from(maximum).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "source limit exceeds usize")
            })?);
        }

        Ok((bytes, truncated))
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

        let pid = parse_ascii_u32(trim_ascii(&bytes[..open]))?;
        let fields = bytes[close + 2..]
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();

        if fields.len() < 3 {
            return Err(invalid_data());
        }

        let ppid = parse_ascii_u32(fields[1])?;
        Ok((pid, &bytes[open + 1..close], fields[0], ppid))
    }

    fn add_process_status(data: &mut Map<String, Value>, bytes: &[u8]) {
        for line in bytes.split(|byte| *byte == b'\n') {
            let Some(separator) = line.iter().position(|byte| *byte == b':') else {
                continue;
            };
            let key = &line[..separator];
            let value = trim_ascii(&line[separator + 1..]);
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
                        if key == b"Uid" {
                            data.insert("uid".into(), json!(values[0]));
                        }
                    }
                }
                b"Groups" => {
                    data.insert(
                        "groups".into(),
                        Value::Array(
                            value
                                .split(|byte| byte.is_ascii_whitespace())
                                .filter_map(|value| parse_ascii_u32(value).ok())
                                .map(Value::from)
                                .collect(),
                        ),
                    );
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
                    data.insert(name.into(), linux_bytes(value));
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
                    data.insert("umask".into(), linux_bytes(value));
                }
                _ => {}
            }
        }
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
            .map(linux_bytes)
            .collect::<Vec<_>>();

        Ok(json!({
            "mount_id":mount_id,
            "parent_id":parent_id,
            "device":linux_bytes(fields[2]),
            "root":linux_bytes(&unescape_mount(fields[3])),
            "target":linux_bytes(&unescape_mount(fields[4])),
            "filesystem":linux_bytes(fields[separator + 1]),
            "source":linux_bytes(&unescape_mount(fields[separator + 2])),
            "options":options,
            "optional_fields":fields[6..separator].iter().map(|field| linux_bytes(field)).collect::<Vec<_>>(),
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

    fn trim_ascii(mut value: &[u8]) -> &[u8] {
        while value.first().is_some_and(u8::is_ascii_whitespace) {
            value = &value[1..];
        }
        while value.last().is_some_and(u8::is_ascii_whitespace) {
            value = &value[..value.len() - 1];
        }

        value
    }

    fn invalid_data() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, "malformed source")
    }

    fn emit_data<W: Write>(
        sink: &mut RecordSink<'_, W>,
        capability: CapabilityId,
        data: Value,
    ) -> Result<bool, ProtocolError> {
        match sink.data(capability.id(), capability.data_kind(), "native", data) {
            Ok(()) => Ok(true),
            Err(error) if error.is_acquisition_limit() => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn source_failure<W: Write>(
        sink: &mut RecordSink<'_, W>,
        capability: CapabilityId,
        path: &Path,
        error: &io::Error,
        coverage: &mut Coverage,
    ) -> Result<(), ProtocolError> {
        let (status, code) = match error.kind() {
            io::ErrorKind::NotFound => {
                coverage.vanished += 1;
                ("not_found", "source_not_found")
            }
            io::ErrorKind::PermissionDenied => {
                coverage.denied += 1;
                ("permission_denied", "source_permission_denied")
            }
            io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => {
                coverage.skipped += 1;
                ("malformed", "source_malformed")
            }
            _ => {
                coverage.skipped += 1;
                ("io_error", "source_io_error")
            }
        };

        sink.diagnostic(
            capability.id(),
            "source",
            status,
            code,
            linux_bytes(path.as_os_str().as_bytes()),
            &error.to_string(),
            1,
        )
    }

    fn malformed_source<W: Write>(
        sink: &mut RecordSink<'_, W>,
        capability: CapabilityId,
        path: &Path,
        coverage: &mut Coverage,
    ) -> Result<(), ProtocolError> {
        coverage.skipped += 1;

        sink.diagnostic(
            capability.id(),
            "source",
            "malformed",
            "source_malformed",
            linux_bytes(path.as_os_str().as_bytes()),
            "source did not match its bounded parser",
            1,
        )
    }

    fn unavailable<W: Write>(
        sink: &mut RecordSink<'_, W>,
        capability: CapabilityId,
        path: &Path,
        error: io::Error,
        mut coverage: Coverage,
    ) -> Result<CapabilityReport, ProtocolError> {
        source_failure(sink, capability, path, &error, &mut coverage)?;

        Ok(CapabilityReport {
            outcome: Outcome::Unavailable,
            coverage,
            limits_hit: Vec::new(),
        })
    }

    fn report_from_coverage(coverage: Coverage, partial: bool) -> CapabilityReport {
        CapabilityReport {
            outcome: if partial {
                Outcome::Partial
            } else {
                Outcome::Complete
            },
            coverage,
            limits_hit: Vec::new(),
        }
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

    fn limit_report(mut coverage: Coverage) -> CapabilityReport {
        coverage.truncated = true;

        CapabilityReport {
            outcome: Outcome::Partial,
            coverage,
            limits_hit: vec!["max_output_bytes_or_records"],
        }
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
        fn non_utf8_bytes_round_trip_through_base64() {
            let value = linux_bytes(b"a\xffb");
            assert_eq!(value["raw_base64"], "Yf9i");
            assert_eq!(value["display"], "a\\xffb");
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
}
