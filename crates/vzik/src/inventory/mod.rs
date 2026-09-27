mod accounts;
mod container;
mod dbus;
mod host;
mod kernel;
mod network;
mod porto;
mod security;
mod system;
mod systemd;

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::cli::{CapabilityId, Invocation, Request};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, Outcome, ProtocolError, RecordSink, linux_bytes,
};

const NAMESPACES: [&str; 7] = ["cgroup", "ipc", "mnt", "net", "pid", "user", "uts"];

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match invocation.capability {
        CapabilityId::HostInfo
        | CapabilityId::KernelInfo
        | CapabilityId::ProcessList
        | CapabilityId::MountList
        | CapabilityId::NetworkResolvers
        | CapabilityId::FileStat
        | CapabilityId::FileRead => host::run(invocation, sink, deadline),
        CapabilityId::KernelModules | CapabilityId::KernelSysctls => {
            kernel::run(invocation, sink, deadline)
        }
        CapabilityId::NetworkInterfaces
        | CapabilityId::NetworkAddresses
        | CapabilityId::NetworkRoutes
        | CapabilityId::NetworkNeighbors
        | CapabilityId::NetworkSockets
        | CapabilityId::NetworkListeners
        | CapabilityId::NetworkFirewall => network::run(invocation, sink, deadline),
        CapabilityId::UserList
        | CapabilityId::GroupList
        | CapabilityId::SudoRules
        | CapabilityId::SshServerConfig
        | CapabilityId::SshAuthorizedKeys => accounts::run(invocation, sink, deadline),
        CapabilityId::CgroupInspect
        | CapabilityId::PackageList
        | CapabilityId::ServiceList
        | CapabilityId::ScheduleList => system::run(invocation, sink, deadline),
        CapabilityId::SystemctlList | CapabilityId::SystemctlInspect => {
            systemd::run(invocation, sink, deadline)
        }
        CapabilityId::DbusList | CapabilityId::DbusInspect => dbus::run(invocation, sink, deadline),
        CapabilityId::ContainerList
        | CapabilityId::ContainerInspect
        | CapabilityId::PortoList
        | CapabilityId::PortoInspect => container::run(invocation, sink, deadline),
        CapabilityId::SecurityPosture
        | CapabilityId::AuthPosture
        | CapabilityId::FilesystemPrivilegeSurfaces
        | CapabilityId::FilesystemUnixSockets => security::run(invocation, sink, deadline),
    }
}

pub(super) fn systemd_unit_type(name: &[u8]) -> Option<&'static str> {
    let separator = name.iter().rposition(|byte| *byte == b'.')?;
    match name.get(separator + 1..)? {
        b"service" => Some("service"),
        b"socket" => Some("socket"),
        b"device" => Some("device"),
        b"mount" => Some("mount"),
        b"automount" => Some("automount"),
        b"swap" => Some("swap"),
        b"target" => Some("target"),
        b"path" => Some("path"),
        b"timer" => Some("timer"),
        b"slice" => Some("slice"),
        b"scope" => Some("scope"),
        _ => None,
    }
}

pub(super) fn item_limit(invocation: &Invocation) -> Result<usize, ProtocolError> {
    match invocation.request {
        Request::ItemLimit { max_items } => Ok(max_items),
        _ => Err(ProtocolError::Internal(format!(
            "{} item limit request mismatch",
            invocation.capability.id()
        ))),
    }
}

pub(super) fn emit<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    data: Value,
    coverage: &mut Coverage,
) -> Result<bool, ProtocolError> {
    emit_provider(sink, capability, "native", data, coverage)
}

pub(super) fn emit_provider<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    provider: &'static str,
    data: Value,
    coverage: &mut Coverage,
) -> Result<bool, ProtocolError> {
    match sink.data(capability.id(), capability.data_kind(), provider, data) {
        Ok(()) => {
            coverage.observed += 1;
            Ok(true)
        }
        Err(error) if error.is_acquisition_limit() => {
            coverage.truncated = true;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn source_failure<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    source: &Path,
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
        linux_bytes(source.as_os_str().as_bytes()),
        &error.to_string(),
        1,
    )
}

pub(super) fn coverage_diagnostic<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    source: &Path,
    message: &str,
) -> Result<(), ProtocolError> {
    sink.diagnostic(
        capability.id(),
        "coverage",
        "unsupported",
        "coverage_incomplete",
        linux_bytes(source.as_os_str().as_bytes()),
        message,
        1,
    )
}

pub(super) fn read_bounded(path: &Path, maximum: u64) -> io::Result<(Vec<u8>, bool)> {
    let file = File::open(path)?;
    let mut bytes = Vec::with_capacity(usize::try_from(maximum.min(64 << 10)).unwrap_or(0));

    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;

    let truncated = bytes.len() as u64 > maximum;
    if truncated {
        bytes.truncate(
            usize::try_from(maximum)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "limit exceeds usize"))?,
        );
    }

    Ok((bytes, truncated))
}

pub(super) fn sorted_entries(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut entries = std::fs::read_dir(path)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });

    Ok(entries)
}

pub(super) fn finish(
    mut coverage: Coverage,
    partial: bool,
    limit: Option<&'static str>,
) -> CapabilityReport {
    let mut limits_hit = Vec::new();

    if let Some(limit) = limit {
        coverage.truncated = true;
        limits_hit.push(limit);
    }

    CapabilityReport {
        outcome: if partial || coverage.truncated {
            Outcome::Partial
        } else {
            Outcome::Complete
        },
        coverage,
        limits_hit,
    }
}

pub(super) fn output_limit(coverage: Coverage) -> CapabilityReport {
    finish(coverage, true, Some("max_output_bytes_or_records"))
}

pub(super) fn unavailable<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    source: &Path,
    error: io::Error,
    mut coverage: Coverage,
) -> Result<CapabilityReport, ProtocolError> {
    source_failure(sink, capability, source, &error, &mut coverage)?;

    Ok(CapabilityReport {
        outcome: Outcome::Unavailable,
        coverage,
        limits_hit: Vec::new(),
    })
}

pub(super) fn text(bytes: &[u8]) -> Value {
    linux_bytes(bytes)
}

pub(super) fn path_value(path: &Path) -> Value {
    linux_bytes(path.as_os_str().as_bytes())
}
