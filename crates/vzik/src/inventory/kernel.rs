use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{
    emit, finish, item_limit, output_limit, path_value, read_bounded, source_failure, text,
    trim_ascii, unavailable,
};
use crate::cli::{CapabilityId, Invocation};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, ProtocolError, RecordSink, check_deadline,
};

const MODULES_SOURCE_LIMIT: u64 = 8 << 20;
const SYSCTL_VALUE_LIMIT: u64 = 64 << 10;

const IMPORTANT_SYSCTLS: &[(&str, &str)] = &[
    ("kernel.kptr_restrict", "/proc/sys/kernel/kptr_restrict"),
    ("kernel.dmesg_restrict", "/proc/sys/kernel/dmesg_restrict"),
    (
        "kernel.yama.ptrace_scope",
        "/proc/sys/kernel/yama/ptrace_scope",
    ),
    (
        "kernel.unprivileged_bpf_disabled",
        "/proc/sys/kernel/unprivileged_bpf_disabled",
    ),
    (
        "kernel.kexec_load_disabled",
        "/proc/sys/kernel/kexec_load_disabled",
    ),
    (
        "kernel.perf_event_paranoid",
        "/proc/sys/kernel/perf_event_paranoid",
    ),
    (
        "kernel.randomize_va_space",
        "/proc/sys/kernel/randomize_va_space",
    ),
    ("kernel.core_pattern", "/proc/sys/kernel/core_pattern"),
    (
        "kernel.modules_disabled",
        "/proc/sys/kernel/modules_disabled",
    ),
    ("kernel.sysrq", "/proc/sys/kernel/sysrq"),
    ("fs.protected_hardlinks", "/proc/sys/fs/protected_hardlinks"),
    ("fs.protected_symlinks", "/proc/sys/fs/protected_symlinks"),
    ("fs.protected_fifos", "/proc/sys/fs/protected_fifos"),
    ("fs.protected_regular", "/proc/sys/fs/protected_regular"),
    ("net.ipv4.ip_forward", "/proc/sys/net/ipv4/ip_forward"),
    (
        "net.ipv4.tcp_syncookies",
        "/proc/sys/net/ipv4/tcp_syncookies",
    ),
    (
        "net.ipv4.conf.all.rp_filter",
        "/proc/sys/net/ipv4/conf/all/rp_filter",
    ),
    (
        "net.ipv4.conf.all.accept_redirects",
        "/proc/sys/net/ipv4/conf/all/accept_redirects",
    ),
    (
        "net.ipv4.conf.all.send_redirects",
        "/proc/sys/net/ipv4/conf/all/send_redirects",
    ),
    (
        "net.ipv6.conf.all.forwarding",
        "/proc/sys/net/ipv6/conf/all/forwarding",
    ),
    (
        "net.ipv6.conf.all.disable_ipv6",
        "/proc/sys/net/ipv6/conf/all/disable_ipv6",
    ),
];

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match invocation.capability {
        CapabilityId::KernelModules => modules(invocation, sink, deadline),
        CapabilityId::KernelSysctls => sysctls(sink, deadline),
        _ => unreachable!("kernel dispatcher received non-kernel capability"),
    }
}

fn modules<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::KernelModules;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let path = Path::new("/proc/modules");
    let (bytes, source_truncated) = match read_bounded(path, MODULES_SOURCE_LIMIT) {
        Ok(value) => value,
        Err(error) => return unavailable(sink, capability, path, error, coverage),
    };

    coverage.scanned = bytes.len() as u64;
    let mut partial = source_truncated;

    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        check_deadline(deadline)?;
        if line.is_empty() {
            continue;
        }

        if coverage.observed as usize >= max_items {
            return Ok(finish(coverage, true, Some("max_items")));
        }

        let fields = line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();

        if fields.len() < 6 {
            partial = true;
            coverage.skipped += 1;
            source_failure(
                sink,
                capability,
                path,
                &io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("malformed module record at line {}", index + 1),
                ),
                &mut coverage,
            )?;
            continue;
        }

        let size = super::parse_u64(fields[1]).unwrap_or(0);
        let use_count = super::parse_u64(fields[2]).unwrap_or(0);
        let dependencies = if fields[3] == b"-" {
            Vec::new()
        } else {
            fields[3]
                .split(|byte| *byte == b',')
                .filter(|dependency| !dependency.is_empty())
                .map(text)
                .collect()
        };

        let data = json!({
            "name":text(fields[0]),
            "size":size,
            "use_count":use_count,
            "dependencies":dependencies,
            "state":text(fields[4]),
            "address":text(fields[5]),
            "source":path_value(path),
        });

        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    Ok(finish(
        coverage,
        partial,
        source_truncated.then_some("max_source_bytes"),
    ))
}

fn sysctls<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::KernelSysctls;
    let mut coverage = Coverage::default();
    let mut partial = false;

    for (name, raw_path) in IMPORTANT_SYSCTLS {
        check_deadline(deadline)?;
        let path = PathBuf::from(raw_path);

        match read_bounded(&path, SYSCTL_VALUE_LIMIT) {
            Ok((bytes, truncated)) => {
                coverage.scanned += bytes.len() as u64;
                partial |= truncated;
                let data = json!({
                    "name":name,
                    "value":text(trim_ascii(&bytes)),
                    "source":path_value(&path),
                });
                if !emit(sink, capability, data, &mut coverage)? {
                    return Ok(output_limit(coverage));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                coverage.vanished += 1;
            }
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &path, &error, &mut coverage)?;
            }
        }
    }

    Ok(finish(coverage, partial, None))
}
