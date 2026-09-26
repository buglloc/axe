use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::json;

use super::{
    emit, finish, item_limit, output_limit, parse_u64, path_value, read_bounded, sorted_entries,
    source_failure, text, trim_ascii, unavailable,
};
use crate::cli::{CapabilityId, Invocation, Request};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, ProtocolError, RecordSink, check_deadline,
};

const SMALL_SOURCE: u64 = 1 << 20;
const TOTAL_AUTH_SOURCE: u64 = 32 << 20;

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match invocation.capability {
        CapabilityId::SecurityPosture => posture(invocation, sink, deadline),
        CapabilityId::AuthPosture => auth(invocation, sink, deadline),
        CapabilityId::FilesystemPrivilegeSurfaces | CapabilityId::FilesystemUnixSockets => {
            filesystem(invocation, sink, deadline)
        }
        _ => unreachable!("security dispatcher received unrelated capability"),
    }
}

fn posture<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::SecurityPosture;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut any_source = false;

    for (category, name, raw_path) in [
        ("lsm", "active", "/sys/kernel/security/lsm"),
        ("lsm", "selinux_enforce", "/sys/fs/selinux/enforce"),
        (
            "lsm",
            "apparmor_enabled",
            "/sys/module/apparmor/parameters/enabled",
        ),
        ("kernel", "lockdown", "/sys/kernel/security/lockdown"),
        ("kernel", "tainted", "/proc/sys/kernel/tainted"),
        (
            "kernel",
            "module_signature_enforce",
            "/sys/module/module/parameters/sig_enforce",
        ),
        (
            "namespace",
            "max_user_namespaces",
            "/proc/sys/user/max_user_namespaces",
        ),
        (
            "namespace",
            "unprivileged_userns_clone",
            "/proc/sys/kernel/unprivileged_userns_clone",
        ),
        ("platform", "hypervisor_type", "/sys/hypervisor/type"),
        ("platform", "system_vendor", "/sys/class/dmi/id/sys_vendor"),
        ("platform", "product_name", "/sys/class/dmi/id/product_name"),
        (
            "platform",
            "product_version",
            "/sys/class/dmi/id/product_version",
        ),
    ] {
        check_deadline(deadline)?;
        let path = Path::new(raw_path);

        match read_bounded(path, SMALL_SOURCE) {
            Ok((bytes, truncated)) => {
                any_source = true;
                coverage.scanned += bytes.len() as u64;
                partial |= truncated;

                if coverage.observed as usize >= max_items {
                    return Ok(finish(coverage, true, Some("max_items")));
                }

                if !emit(
                    sink,
                    capability,
                    json!({
                        "category":category,
                        "name":name,
                        "value":text(trim_ascii(&bytes)),
                        "source":path_value(path),
                    }),
                    &mut coverage,
                )? {
                    return Ok(output_limit(coverage));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => coverage.vanished += 1,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, path, &error, &mut coverage)?;
            }
        }
    }

    let vulnerabilities = Path::new("/sys/devices/system/cpu/vulnerabilities");
    if let Ok(entries) = sorted_entries(vulnerabilities) {
        any_source = true;
        for path in entries {
            check_deadline(deadline)?;
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            let (bytes, truncated) = match read_bounded(&path, SMALL_SOURCE) {
                Ok(value) => value,
                Err(error) => {
                    partial = true;
                    source_failure(sink, capability, &path, &error, &mut coverage)?;
                    continue;
                }
            };

            coverage.scanned += bytes.len() as u64;
            partial |= truncated;

            if !emit(
                sink,
                capability,
                json!({
                    "category":"cpu_vulnerability",
                    "name":path.file_name().map(|name| text(name.as_bytes())),
                    "value":text(trim_ascii(&bytes)),
                    "source":path_value(&path),
                }),
                &mut coverage,
            )? {
                return Ok(output_limit(coverage));
            }
        }
    }

    let efivars = Path::new("/sys/firmware/efi/efivars");
    if let Ok(entries) = sorted_entries(efivars) {
        for path in entries.into_iter().filter(|path| {
            path.file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b"SecureBoot-"))
        }) {
            any_source = true;
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            let (bytes, truncated) = match read_bounded(&path, 8192) {
                Ok(value) => value,
                Err(error) => {
                    partial = true;
                    source_failure(sink, capability, &path, &error, &mut coverage)?;
                    continue;
                }
            };

            coverage.scanned += bytes.len() as u64;
            partial |= truncated;
            let enabled = bytes.get(4).map(|value| *value != 0);

            if !emit(
                sink,
                capability,
                json!({
                    "category":"boot",
                    "name":"secure_boot",
                    "value":enabled,
                    "source":path_value(&path),
                }),
                &mut coverage,
            )? {
                return Ok(output_limit(coverage));
            }
        }
    }

    if !any_source {
        return unavailable(
            sink,
            capability,
            Path::new("/sys/kernel/security"),
            io::Error::new(io::ErrorKind::NotFound, "no security posture source found"),
            coverage,
        );
    }

    Ok(finish(coverage, partial, None))
}

fn auth<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::AuthPosture;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let mut partial = false;

    collect_shadow(
        sink,
        deadline,
        capability,
        max_items,
        &mut coverage,
        &mut partial,
    )?;

    if coverage.observed as usize >= max_items {
        return Ok(finish(coverage, true, Some("max_items")));
    }

    let mut any_source = coverage.scanned > 0;

    let mut files = vec![
        PathBuf::from("/etc/login.defs"),
        PathBuf::from("/etc/doas.conf"),
        PathBuf::from("/etc/pam.conf"),
    ];
    for root in [
        "/etc/pam.d",
        "/etc/security",
        "/etc/polkit-1/rules.d",
        "/usr/share/polkit-1/rules.d",
    ] {
        if let Ok(mut entries) = sorted_entries(Path::new(root)) {
            entries.retain(|path| path.is_file());
            files.append(&mut entries);
        }
    }

    files.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });
    files.dedup();

    let mut total_bytes = 0_u64;
    for path in files {
        check_deadline(deadline)?;
        let (bytes, truncated) = match read_bounded(&path, SMALL_SOURCE) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                coverage.vanished += 1;
                continue;
            }
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &path, &error, &mut coverage)?;
                continue;
            }
        };

        any_source = true;
        total_bytes += bytes.len() as u64;
        coverage.scanned += bytes.len() as u64;
        partial |= truncated;

        if total_bytes > TOTAL_AUTH_SOURCE {
            return Ok(finish(coverage, true, Some("max_source_bytes")));
        }

        let kind = auth_source_kind(&path);

        for (line_number, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            let line = trim_ascii(line);
            if line.is_empty() || line.starts_with(b"#") {
                continue;
            }

            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }

            if !emit(
                sink,
                capability,
                json!({
                    "kind":kind,
                    "source":path_value(&path),
                    "line":line_number + 1,
                    "directive":text(line),
                }),
                &mut coverage,
            )? {
                return Ok(output_limit(coverage));
            }
        }
    }

    if !any_source {
        return unavailable(
            sink,
            capability,
            Path::new("/etc/shadow"),
            io::Error::new(
                io::ErrorKind::NotFound,
                "no authentication policy source found",
            ),
            coverage,
        );
    }

    Ok(finish(coverage, partial, None))
}

#[allow(clippy::too_many_arguments)]
fn collect_shadow<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    capability: CapabilityId,
    max_items: usize,
    coverage: &mut Coverage,
    partial: &mut bool,
) -> Result<(), ProtocolError> {
    let path = Path::new("/etc/shadow");
    let (bytes, truncated) = match read_bounded(path, 8 << 20) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            coverage.vanished += 1;
            return Ok(());
        }
        Err(error) => {
            *partial = true;
            source_failure(sink, capability, path, &error, coverage)?;
            return Ok(());
        }
    };

    coverage.scanned += bytes.len() as u64;
    *partial |= truncated;

    for line in bytes.split(|byte| *byte == b'\n') {
        check_deadline(deadline)?;
        if line.is_empty() {
            continue;
        }

        let fields = line.split(|byte| *byte == b':').collect::<Vec<_>>();
        if fields.len() < 9 {
            *partial = true;
            coverage.skipped += 1;
            continue;
        }

        if coverage.observed as usize >= max_items {
            coverage.truncated = true;
            return Ok(());
        }

        let (password_state, password_algorithm) = classify_password(fields[1]);

        if !emit(
            sink,
            capability,
            json!({
                "kind":"shadow_account",
                "source":path_value(path),
                "user":text(fields[0]),
                "password_state":password_state,
                "password_algorithm":password_algorithm,
                "last_change_days":optional_decimal(fields[2]),
                "minimum_days":optional_decimal(fields[3]),
                "maximum_days":optional_decimal(fields[4]),
                "warning_days":optional_decimal(fields[5]),
                "inactive_days":optional_decimal(fields[6]),
                "expire_days":optional_decimal(fields[7]),
            }),
            coverage,
        )? {
            coverage.truncated = true;
            return Ok(());
        }
    }
    Ok(())
}

fn classify_password(value: &[u8]) -> (&'static str, Option<&'static str>) {
    if value.is_empty() {
        return ("empty", None);
    }

    if matches!(value.first(), Some(b'!' | b'*')) {
        return ("locked", None);
    }

    let algorithm = if value.starts_with(b"$1$") {
        Some("md5crypt")
    } else if value.starts_with(b"$2") {
        Some("bcrypt")
    } else if value.starts_with(b"$5$") {
        Some("sha256crypt")
    } else if value.starts_with(b"$6$") {
        Some("sha512crypt")
    } else if value.starts_with(b"$y$") {
        Some("yescrypt")
    } else {
        Some("unknown")
    };

    ("usable", algorithm)
}

fn optional_decimal(value: &[u8]) -> Option<u64> {
    (!value.is_empty()).then(|| parse_u64(value).ok()).flatten()
}

fn auth_source_kind(path: &Path) -> &'static str {
    let bytes = path.as_os_str().as_bytes();
    if bytes.starts_with(b"/etc/pam") {
        "pam_directive"
    } else if bytes.windows(6).any(|value| value == b"polkit") {
        "polkit_rule"
    } else if bytes.ends_with(b"login.defs") {
        "login_policy"
    } else if bytes.ends_with(b"doas.conf") {
        "doas_rule"
    } else {
        "security_directive"
    }
}

fn filesystem<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let Request::FilesystemScan {
        path,
        max_items,
        max_entries,
        max_depth,
        include_visible_mounts,
    } = &invocation.request
    else {
        return Err(ProtocolError::Internal(
            "filesystem scan request mismatch".into(),
        ));
    };

    let capability = invocation.capability;
    let root_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return unavailable(sink, capability, path, error, Coverage::default()),
    };

    let root_device = root_metadata.dev();
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut discovered = 1_usize;
    let mut visited_directories = BTreeSet::new();
    let mut queue = VecDeque::with_capacity((*max_entries).min(4096));
    queue.push_back((path.clone(), 0_usize));

    'walk: while let Some((current, depth)) = queue.pop_front() {
        check_deadline(deadline)?;
        coverage.scanned += 1;
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &current, &error, &mut coverage)?;
                continue;
            }
        };

        if metadata.dev() != root_device && !include_visible_mounts {
            partial = true;
            coverage.skipped += 1;
            sink.diagnostic(
            capability.id(),
            "coverage",
            "skipped",
            "mount_boundary_skipped",
            path_value(&current),
            "visible mount boundary was not crossed; pass --include-visible-mounts to include it",
            1,
        )?;
            continue;
        }

        if metadata.is_dir() && !visited_directories.insert((metadata.dev(), metadata.ino())) {
            partial = true;
            coverage.skipped += 1;
            sink.diagnostic(
                capability.id(),
                "coverage",
                "skipped",
                "filesystem_cycle_skipped",
                path_value(&current),
                "directory inode was already visited",
                1,
            )?;
            continue;
        }

        let selected = match capability {
            CapabilityId::FilesystemUnixSockets if metadata.file_type().is_socket() => {
                Some(json!({
                    "path":path_value(&current),
                    "type":"unix_socket",
                    "mode":metadata.mode() & 0o7777,
                    "uid":metadata.uid(),
                    "gid":metadata.gid(),
                    "device":metadata.dev(),
                    "inode":metadata.ino(),
                }))
            }
            CapabilityId::FilesystemPrivilegeSurfaces => {
                let mode = metadata.mode();
                let mut reasons = Vec::new();
                if metadata.is_file() && mode & 0o4000 != 0 {
                    reasons.push("setuid");
                }
                if metadata.is_file() && mode & 0o2000 != 0 {
                    reasons.push("setgid");
                }
                if mode & 0o0002 != 0 {
                    reasons.push("world_writable");
                }
                if mode & 0o0020 != 0 && (metadata.is_dir() || mode & 0o0111 != 0) {
                    reasons.push("group_writable_execution_path");
                }

                let mut capability_buffer = Vec::<u8>::with_capacity(256);
                let file_capability = match rustix::fs::lgetxattr(
                    &current,
                    "security.capability",
                    &mut capability_buffer,
                ) {
                    Ok(length) if length > 0 => Some(capability_buffer),
                    _ => None,
                };
                if file_capability.is_some() {
                    reasons.push("file_capability");
                }
                if reasons.is_empty() {
                    None
                } else {
                    let kind = if metadata.is_dir() {
                        "directory"
                    } else if metadata.is_file() {
                        "regular"
                    } else if metadata.file_type().is_symlink() {
                        "symlink"
                    } else {
                        "special"
                    };
                    Some(json!({
                        "path":path_value(&current),
                        "type":kind,
                        "mode":mode & 0o7777,
                        "uid":metadata.uid(),
                        "gid":metadata.gid(),
                        "reasons":reasons,
                        "file_capability_base64":file_capability.map(|value| {
                            base64::engine::general_purpose::STANDARD.encode(value)
                        }),
                    }))
                }
            }
            _ => None,
        };

        if let Some(data) = selected {
            if coverage.observed as usize >= *max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            if !emit(sink, capability, data, &mut coverage)? {
                return Ok(output_limit(coverage));
            }
        }

        if !metadata.is_dir() || depth >= *max_depth {
            continue;
        }

        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &current, &error, &mut coverage)?;
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    partial = true;
                    coverage.skipped += 1;
                    continue;
                }
            };

            if discovered >= *max_entries {
                coverage.truncated = true;
                partial = true;
                break 'walk;
            }

            discovered += 1;
            queue.push_back((entry.path(), depth + 1));
        }
    }

    let entry_limit_hit = coverage.truncated;
    Ok(finish(
        coverage,
        partial,
        entry_limit_hit.then_some("max_entries"),
    ))
}
