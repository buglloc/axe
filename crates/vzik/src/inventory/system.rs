use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{
    emit, finish, item_limit, output_limit, parse_u64, path_value, read_bounded, sorted_entries,
    source_failure, systemd_unit_type, text, trim_ascii, unavailable,
};
use crate::cli::{CapabilityId, Invocation};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, Outcome, ProtocolError, RecordSink,
    check_deadline,
};

const SMALL_SOURCE: u64 = 1 << 20;
const PACKAGE_SOURCE: u64 = 32 << 20;

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match invocation.capability {
        CapabilityId::CgroupInspect => cgroup(sink, deadline),
        CapabilityId::PackageList => packages(invocation, sink, deadline),
        CapabilityId::ServiceList => services(invocation, sink, deadline),
        CapabilityId::ScheduleList => schedules(invocation, sink, deadline),
        _ => unreachable!("system dispatcher received unrelated capability"),
    }
}

fn cgroup<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::CgroupInspect;
    let mut coverage = Coverage::default();
    let path = Path::new("/proc/self/cgroup");
    let (bytes, truncated) = match read_bounded(path, SMALL_SOURCE) {
        Ok(value) => value,
        Err(error) => return unavailable(sink, capability, path, error, coverage),
    };

    coverage.scanned = bytes.len() as u64;
    let mut partial = truncated;
    let mut unified_path = None;

    for line in bytes.split(|byte| *byte == b'\n') {
        check_deadline(deadline)?;
        if line.is_empty() {
            continue;
        }

        let fields = line.splitn(3, |byte| *byte == b':').collect::<Vec<_>>();
        if fields.len() != 3 {
            partial = true;
            coverage.skipped += 1;
            continue;
        }

        if fields[0] == b"0" {
            let relative = fields[2].strip_prefix(b"/").unwrap_or(fields[2]);
            unified_path = Some(PathBuf::from(std::ffi::OsStr::from_bytes(relative)));
        }

        let data = json!({
            "hierarchy_id":parse_u64(fields[0]).unwrap_or(0),
            "controllers":fields[1].split(|byte| *byte == b',').filter(|value| !value.is_empty()).map(text).collect::<Vec<_>>(),
            "path":text(fields[2]),
            "version":if fields[0] == b"0" {2} else {1},
            "scope":"self",
            "source":path_value(path),
        });

        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    if let Some(unified_path) = unified_path {
        let current_root = Path::new("/sys/fs/cgroup").join(unified_path);
        for name in [
            "cgroup.controllers",
            "cgroup.subtree_control",
            "cpu.max",
            "memory.current",
            "memory.max",
            "pids.current",
            "pids.max",
            "cpuset.cpus.effective",
        ] {
            check_deadline(deadline)?;
            let source = current_root.join(name);
            if let Ok((value, value_truncated)) = read_bounded(&source, 64 << 10) {
                coverage.scanned += value.len() as u64;
                partial |= value_truncated;
                let data = json!({
                    "version":2,
                    "scope":"self",
                    "property":name,
                    "value":text(trim_ascii(&value)),
                    "source":path_value(&source),
                });
                if !emit(sink, capability, data, &mut coverage)? {
                    return Ok(output_limit(coverage));
                }
            }
        }
    }

    Ok(finish(coverage, partial, None))
}

fn packages<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::PackageList;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut any_source = false;

    for (path, manager) in [
        (Path::new("/var/lib/dpkg/status"), "dpkg"),
        (Path::new("/lib/apk/db/installed"), "apk"),
    ] {
        check_deadline(deadline)?;
        let (bytes, truncated) = match read_bounded(path, PACKAGE_SOURCE) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, path, &error, &mut coverage)?;
                continue;
            }
        };

        any_source = true;
        coverage.scanned += bytes.len() as u64;
        partial |= truncated;

        let records = if manager == "dpkg" {
            parse_dpkg(&bytes)
        } else {
            parse_apk(&bytes)
        };

        for data in records {
            check_deadline(deadline)?;
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            if !emit(sink, capability, data, &mut coverage)? {
                return Ok(output_limit(coverage));
            }
        }
    }

    let pacman_root = Path::new("/var/lib/pacman/local");
    if let Ok(entries) = sorted_entries(pacman_root) {
        any_source = true;
        for entry in entries {
            check_deadline(deadline)?;
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            let desc = entry.join("desc");
            let Ok((bytes, truncated)) = read_bounded(&desc, SMALL_SOURCE) else {
                partial = true;
                coverage.skipped += 1;
                continue;
            };
            coverage.scanned += bytes.len() as u64;
            partial |= truncated;
            if let Some(data) = parse_pacman(&bytes, &desc)
                && !emit(sink, capability, data, &mut coverage)?
            {
                return Ok(output_limit(coverage));
            }
        }
    }

    for path in nix_manifest_paths() {
        check_deadline(deadline)?;
        let (bytes, truncated) = match read_bounded(&path, PACKAGE_SOURCE) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &path, &error, &mut coverage)?;
                continue;
            }
        };
        any_source = true;
        coverage.scanned += bytes.len() as u64;
        partial |= truncated;
        let records = match parse_nix_manifest(&bytes, &path) {
            Ok(records) => records,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &path, &error, &mut coverage)?;
                continue;
            }
        };
        for data in records {
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            if !emit(sink, capability, data, &mut coverage)? {
                return Ok(output_limit(coverage));
            }
        }
    }

    if !any_source {
        return Ok(CapabilityReport {
            outcome: Outcome::Unavailable,
            coverage,
            limits_hit: Vec::new(),
        });
    }

    Ok(finish(coverage, partial, None))
}

fn parse_dpkg(bytes: &[u8]) -> Vec<Value> {
    paragraphs(bytes)
        .into_iter()
        .filter_map(|paragraph| {
            let fields = colon_fields(paragraph);
            Some(json!({
                "name":text(fields.get(b"Package".as_slice())?),
                "version":text(fields.get(b"Version".as_slice())?),
                "architecture":fields.get(b"Architecture".as_slice()).map(|value| text(value)),
                "status":fields.get(b"Status".as_slice()).map(|value| text(value)),
                "description":fields.get(b"Description".as_slice()).map(|value| text(value)),
                "manager":"dpkg",
                "source":path_value(Path::new("/var/lib/dpkg/status")),
            }))
        })
        .collect()
}

fn parse_apk(bytes: &[u8]) -> Vec<Value> {
    paragraphs(bytes)
        .into_iter()
        .filter_map(|paragraph| {
            let fields = apk_fields(paragraph);
            Some(json!({
                "name":text(fields.get(&b'P')?),
                "version":text(fields.get(&b'V')?),
                "architecture":fields.get(&b'A').map(|value| text(value)),
                "size":fields.get(&b'S').and_then(|value| parse_u64(value).ok()),
                "description":fields.get(&b'T').map(|value| text(value)),
                "manager":"apk",
                "source":path_value(Path::new("/lib/apk/db/installed")),
            }))
        })
        .collect()
}

fn parse_pacman(bytes: &[u8], source: &Path) -> Option<Value> {
    let mut fields = BTreeMap::<Vec<u8>, Vec<u8>>::new();
    let mut lines = bytes.split(|byte| *byte == b'\n');
    while let Some(line) = lines.next() {
        if line.starts_with(b"%")
            && line.ends_with(b"%")
            && let Some(value) = lines.next()
        {
            fields.insert(line.to_vec(), value.to_vec());
        }
    }

    Some(json!({
        "name":text(fields.get(b"%NAME%".as_slice())?),
        "version":text(fields.get(b"%VERSION%".as_slice())?),
        "architecture":fields.get(b"%ARCH%".as_slice()).map(|value| text(value)),
        "description":fields.get(b"%DESC%".as_slice()).map(|value| text(value)),
        "manager":"pacman",
        "source":path_value(source),
    }))
}

fn nix_manifest_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/nix/var/nix/profiles/system/manifest.json"),
        PathBuf::from("/nix/var/nix/profiles/default/manifest.json"),
    ];
    if let Ok(users) = sorted_entries(Path::new("/nix/var/nix/profiles/per-user")) {
        'users: for user in users {
            if let Ok(profiles) = sorted_entries(&user) {
                for profile in profiles {
                    if paths.len() >= 4_096 {
                        break 'users;
                    }
                    paths.push(profile.join("manifest.json"));
                }
            }
        }
    }

    paths
}

fn parse_nix_manifest(bytes: &[u8], source: &Path) -> io::Result<Vec<Value>> {
    let manifest: Value = serde_json::from_slice(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let mut records = Vec::new();
    if let Some(elements) = manifest.get("elements").and_then(Value::as_object) {
        for (element_name, element) in elements {
            let name = element
                .get("pname")
                .and_then(Value::as_str)
                .unwrap_or(element_name);
            let version = element
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let store_paths = element
                .get("storePaths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|path| text(path.as_bytes()))
                .collect::<Vec<_>>();
            let status = if element
                .get("active")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                b"active".as_slice()
            } else {
                b"inactive".as_slice()
            };
            records.push(json!({
                "name":text(name.as_bytes()),
                "version":text(version.as_bytes()),
                "status":text(status),
                "store_paths":store_paths,
                "manager":"nix",
                "source":path_value(source),
            }));
        }
    } else if let Some(elements) = manifest.as_array() {
        for element in elements {
            let Some(name) = element.get("name").and_then(Value::as_str) else {
                continue;
            };
            let version = element
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut store_paths = element
                .get("outputs")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|outputs| outputs.values())
                .filter_map(Value::as_str)
                .map(|path| text(path.as_bytes()))
                .collect::<Vec<_>>();
            if let Some(path) = element.get("outPath").and_then(Value::as_str) {
                store_paths.push(text(path.as_bytes()));
            }
            records.push(json!({
                "name":text(name.as_bytes()),
                "version":text(version.as_bytes()),
                "status":text(b"active"),
                "store_paths":store_paths,
                "manager":"nix",
                "source":path_value(source),
            }));
        }
    }

    Ok(records)
}

#[derive(Clone)]
struct SystemdManagerRoots {
    scope: &'static str,
    manager_user: Option<Vec<u8>>,
    manager_uid: Option<u32>,
    roots: Vec<PathBuf>,
}

fn systemd_manager_roots() -> (Vec<SystemdManagerRoots>, bool, Option<io::Error>) {
    let mut managers = vec![
        SystemdManagerRoots {
            scope: "system",
            manager_user: None,
            manager_uid: None,
            roots: [
                "/etc/systemd/system.control",
                "/run/systemd/system.control",
                "/run/systemd/transient",
                "/run/systemd/generator.early",
                "/etc/systemd/system",
                "/etc/systemd/system.attached",
                "/run/systemd/system",
                "/run/systemd/system.attached",
                "/run/systemd/generator",
                "/usr/local/lib/systemd/system",
                "/usr/lib/systemd/system",
                "/lib/systemd/system",
                "/run/systemd/generator.late",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        },
        SystemdManagerRoots {
            scope: "user",
            manager_user: None,
            manager_uid: None,
            roots: [
                "/etc/systemd/user",
                "/run/systemd/user",
                "/usr/local/lib/systemd/user",
                "/usr/local/share/systemd/user",
                "/usr/lib/systemd/user",
                "/usr/share/systemd/user",
                "/lib/systemd/user",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        },
    ];

    let (truncated, error) = match read_bounded(Path::new("/etc/passwd"), SMALL_SOURCE) {
        Ok((passwd, truncated)) => {
            add_user_manager_roots(&mut managers, &passwd);
            (truncated, None)
        }
        Err(error) => (false, Some(error)),
    };

    managers.sort_by(|left, right| {
        (left.scope, left.manager_uid, left.manager_user.as_deref()).cmp(&(
            right.scope,
            right.manager_uid,
            right.manager_user.as_deref(),
        ))
    });
    (managers, truncated, error)
}

fn add_user_manager_roots(managers: &mut Vec<SystemdManagerRoots>, passwd: &[u8]) {
    for line in passwd.split(|byte| *byte == b'\n') {
        let fields = line.split(|byte| *byte == b':').collect::<Vec<_>>();
        if fields.len() < 7 {
            continue;
        }
        let Ok(uid) = parse_u64(fields[2]).and_then(|uid| {
            u32::try_from(uid)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "UID exceeds u32"))
        }) else {
            continue;
        };
        if fields[5].is_empty() {
            continue;
        }

        let home = PathBuf::from(std::ffi::OsStr::from_bytes(fields[5]));
        let runtime = PathBuf::from(format!("/run/user/{uid}"));
        managers.push(SystemdManagerRoots {
            scope: "user",
            manager_user: Some(fields[0].to_vec()),
            manager_uid: Some(uid),
            roots: vec![
                home.join(".config/systemd/user.control"),
                runtime.join("systemd/user.control"),
                runtime.join("systemd/transient"),
                runtime.join("systemd/generator.early"),
                home.join(".config/systemd/user"),
                runtime.join("systemd/user"),
                runtime.join("systemd/generator"),
                home.join(".local/share/systemd/user"),
                runtime.join("systemd/generator.late"),
            ],
        });
    }
}

fn systemd_enablement(roots: &[PathBuf]) -> BTreeMap<Vec<u8>, Vec<PathBuf>> {
    let mut enabled = BTreeMap::<Vec<u8>, Vec<PathBuf>>::new();
    for root in roots {
        let Ok(entries) = sorted_entries(root) else {
            continue;
        };
        for directory in entries {
            let Some(name) = directory.file_name() else {
                continue;
            };
            if !name.as_bytes().ends_with(b".wants") && !name.as_bytes().ends_with(b".requires") {
                continue;
            }
            if let Ok(links) = sorted_entries(&directory) {
                for link in links {
                    if let Some(unit) = link.file_name() {
                        enabled
                            .entry(unit.as_bytes().to_vec())
                            .or_default()
                            .push(link);
                    }
                }
            }
        }
    }

    enabled
}

fn systemd_drop_ins(roots: &[PathBuf], name: &[u8]) -> Vec<PathBuf> {
    let mut directory = name.to_vec();
    directory.extend_from_slice(b".d");
    let directory = std::ffi::OsStr::from_bytes(&directory);
    let mut paths = BTreeMap::<Vec<u8>, PathBuf>::new();
    for root in roots {
        if let Ok(entries) = sorted_entries(&root.join(directory)) {
            for path in entries {
                let Some(file_name) = path.file_name() else {
                    continue;
                };
                if path
                    .extension()
                    .is_some_and(|extension| extension == "conf")
                {
                    paths.entry(file_name.as_bytes().to_vec()).or_insert(path);
                }
            }
        }
    }

    paths.into_values().collect()
}

fn parse_systemd_unit(
    bytes: &[u8],
    source: &Path,
    data: &mut Map<String, Value>,
    evidence: &mut Vec<Value>,
) {
    for raw in bytes.split(|byte| *byte == b'\n') {
        let line = trim_ascii(raw);
        if line.is_empty() || line.starts_with(b"#") || line.starts_with(b";") {
            continue;
        }
        for (key, field) in [
            (b"Description=".as_slice(), "description"),
            (b"Type=".as_slice(), "service_type"),
            (b"User=".as_slice(), "user"),
            (b"Group=".as_slice(), "group"),
            (b"ExecStart=".as_slice(), "exec_start"),
        ] {
            if let Some(value) = line.strip_prefix(key) {
                data.insert(field.into(), text(value));
            }
        }

        let Some(equal) = line.iter().position(|byte| *byte == b'=') else {
            continue;
        };

        let key = &line[..equal];
        if [
            b"AmbientCapabilities".as_slice(),
            b"CapabilityBoundingSet".as_slice(),
            b"EnvironmentFile".as_slice(),
            b"NoNewPrivileges".as_slice(),
            b"PrivateDevices".as_slice(),
            b"PrivateTmp".as_slice(),
            b"ProtectHome".as_slice(),
            b"ProtectKernelModules".as_slice(),
            b"ProtectSystem".as_slice(),
            b"ReadWritePaths".as_slice(),
            b"RestrictNamespaces".as_slice(),
            b"SystemCallFilter".as_slice(),
        ]
        .contains(&key)
            || key.starts_with(b"Listen")
        {
            evidence.push(json!({
                "source":path_value(source),
                "key":text(key),
                "value":text(&line[equal + 1..]),
            }));
        }
    }
}

enum ManagerCollection {
    Complete { readable_source: bool },
    ItemLimit,
    OutputLimit,
}

fn collect_systemd_manager<W: Write>(
    manager: &SystemdManagerRoots,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    max_items: usize,
    coverage: &mut Coverage,
    partial: &mut bool,
) -> Result<ManagerCollection, ProtocolError> {
    let capability = CapabilityId::ServiceList;
    let mut units = BTreeMap::<Vec<u8>, PathBuf>::new();
    let enabled = systemd_enablement(&manager.roots);
    let mut readable_source = false;

    for root in &manager.roots {
        let Ok(entries) = sorted_entries(root) else {
            continue;
        };
        readable_source = true;
        for path in entries {
            let Some(name) = path.file_name() else {
                continue;
            };
            if systemd_unit_type(name.as_bytes()).is_some() {
                units.entry(name.as_bytes().to_vec()).or_insert(path);
            }
        }
    }

    for (name, path) in units {
        check_deadline(deadline)?;
        if coverage.observed as usize >= max_items {
            return Ok(ManagerCollection::ItemLimit);
        }
        coverage.scanned += 1;
        let metadata = fs::symlink_metadata(&path).ok();
        let linked = metadata
            .as_ref()
            .is_some_and(|item| item.file_type().is_symlink());
        let masked =
            linked && fs::read_link(&path).is_ok_and(|target| target == Path::new("/dev/null"));
        let enabled_by = enabled.get(&name).cloned().unwrap_or_default();

        let mut data = Map::new();
        data.insert("name".into(), text(&name));
        data.insert("manager".into(), json!("systemd"));
        data.insert("scope".into(), json!(manager.scope));
        data.insert(
            "unit_type".into(),
            json!(systemd_unit_type(&name).expect("filtered systemd unit has a type")),
        );
        data.insert("source".into(), path_value(&path));
        if let Some(user) = manager.manager_user.as_deref() {
            data.insert("manager_user".into(), text(user));
        }
        if let Some(uid) = manager.manager_uid {
            data.insert("manager_uid".into(), json!(uid));
        }
        data.insert(
            "static_state".into(),
            json!(if masked {
                "masked"
            } else if !enabled_by.is_empty() {
                "enabled"
            } else if linked {
                "linked"
            } else {
                "present"
            }),
        );
        data.insert(
            "enabled_by".into(),
            Value::Array(enabled_by.iter().map(|path| path_value(path)).collect()),
        );

        let drop_ins = systemd_drop_ins(&manager.roots, &name);
        data.insert(
            "drop_ins".into(),
            Value::Array(drop_ins.iter().map(|path| path_value(path)).collect()),
        );

        let mut evidence = Vec::new();
        for source in std::iter::once(&path).chain(drop_ins.iter()) {
            match read_bounded(source, SMALL_SOURCE) {
                Ok((bytes, truncated)) => {
                    coverage.scanned += bytes.len() as u64;
                    *partial |= truncated;
                    parse_systemd_unit(&bytes, source, &mut data, &mut evidence);
                }
                Err(error) => {
                    *partial = true;
                    source_failure(sink, capability, source, &error, coverage)?;
                }
            }
        }

        data.insert("security_directives".into(), Value::Array(evidence));

        if !emit(sink, capability, Value::Object(data), coverage)? {
            return Ok(ManagerCollection::OutputLimit);
        }
    }

    Ok(ManagerCollection::Complete { readable_source })
}

fn services<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::ServiceList;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let (managers, passwd_truncated, passwd_error) = systemd_manager_roots();
    if let Some(error) = passwd_error {
        partial = true;
        source_failure(
            sink,
            capability,
            Path::new("/etc/passwd"),
            &error,
            &mut coverage,
        )?;
    }
    partial |= passwd_truncated;
    let mut any_source = false;

    for manager in &managers {
        match collect_systemd_manager(
            manager,
            sink,
            deadline,
            max_items,
            &mut coverage,
            &mut partial,
        )? {
            ManagerCollection::Complete { readable_source } => any_source |= readable_source,
            ManagerCollection::ItemLimit => {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            ManagerCollection::OutputLimit => return Ok(output_limit(coverage)),
        }
    }

    if let Ok(entries) = sorted_entries(Path::new("/etc/init.d")) {
        any_source = true;
        for path in entries {
            check_deadline(deadline)?;
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            let Some(name) = path.file_name() else {
                continue;
            };
            let data = json!({
                "name":text(name.as_bytes()),
                "manager":"sysv",
                "scope":"system",
                "unit_type":"service",
                "source":path_value(&path),
                "static_state":"present",
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
            Path::new("/etc/systemd/system"),
            io::Error::new(io::ErrorKind::NotFound, "no service inventory source found"),
            coverage,
        );
    }

    Ok(finish(coverage, partial, None))
}

fn schedules<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::ScheduleList;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut any_source = false;

    let mut files = vec![
        PathBuf::from("/etc/crontab"),
        PathBuf::from("/etc/anacrontab"),
    ];
    for root in ["/etc/cron.d", "/var/spool/cron", "/var/spool/cron/crontabs"] {
        if let Ok(mut entries) = sorted_entries(Path::new(root)) {
            entries.retain(|path| path.is_file());
            files.append(&mut entries);
            any_source = true;
        }
    }

    for path in files {
        check_deadline(deadline)?;
        let (bytes, truncated) = match read_bounded(&path, SMALL_SOURCE) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &path, &error, &mut coverage)?;
                continue;
            }
        };
        any_source = true;
        coverage.scanned += bytes.len() as u64;
        partial |= truncated;

        let entry_type = if path == Path::new("/etc/anacrontab") {
            "anacron"
        } else {
            "cron"
        };

        for (line_number, raw) in bytes.split(|byte| *byte == b'\n').enumerate() {
            let line = trim_ascii(raw);
            if line.is_empty() || line.starts_with(b"#") || is_cron_environment(line) {
                continue;
            }

            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }

            let data = json!({
                "type":entry_type,
                "source":path_value(&path),
                "line":line_number + 1,
                "entry":text(line),
            });
            if !emit(sink, capability, data, &mut coverage)? {
                return Ok(output_limit(coverage));
            }
        }
    }

    for (root, period) in [
        ("/etc/cron.hourly", "hourly"),
        ("/etc/cron.daily", "daily"),
        ("/etc/cron.weekly", "weekly"),
        ("/etc/cron.monthly", "monthly"),
    ] {
        let Ok(entries) = sorted_entries(Path::new(root)) else {
            continue;
        };
        any_source = true;
        for path in entries {
            check_deadline(deadline)?;
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            if !emit(
                sink,
                capability,
                json!({"type":"periodic","period":period,"source":path_value(&path)}),
                &mut coverage,
            )? {
                return Ok(output_limit(coverage));
            }
        }
    }

    for root in ["/var/spool/at", "/var/spool/cron/atjobs"] {
        let Ok(entries) = sorted_entries(Path::new(root)) else {
            continue;
        };
        any_source = true;
        for path in entries {
            check_deadline(deadline)?;
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            if !emit(
                sink,
                capability,
                json!({"type":"at_job","source":path_value(&path)}),
                &mut coverage,
            )? {
                return Ok(output_limit(coverage));
            }
        }
    }

    let (managers, passwd_truncated, passwd_error) = systemd_manager_roots();
    if let Some(error) = passwd_error {
        partial = true;
        source_failure(
            sink,
            capability,
            Path::new("/etc/passwd"),
            &error,
            &mut coverage,
        )?;
    }
    partial |= passwd_truncated;
    let timer_roots = managers
        .into_iter()
        .flat_map(|manager| manager.roots)
        .map(|root| (root.as_os_str().as_bytes().to_vec(), root))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    let enabled = systemd_enablement(&timer_roots);

    for root in &timer_roots {
        let Ok(entries) = sorted_entries(root) else {
            continue;
        };
        any_source = true;
        for path in entries.into_iter().filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "timer")
        }) {
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

            let mut triggers = Vec::new();
            let mut timer_directives = Vec::new();
            for raw in bytes.split(|byte| *byte == b'\n') {
                let line = trim_ascii(raw);
                let Some(equal) = line.iter().position(|byte| *byte == b'=') else {
                    continue;
                };
                let key = &line[..equal];
                let value = &line[equal + 1..];
                if [
                    b"AccuracySec".as_slice(),
                    b"OnActiveSec".as_slice(),
                    b"OnBootSec".as_slice(),
                    b"OnCalendar".as_slice(),
                    b"OnStartupSec".as_slice(),
                    b"OnUnitActiveSec".as_slice(),
                    b"OnUnitInactiveSec".as_slice(),
                    b"Persistent".as_slice(),
                    b"RandomizedDelaySec".as_slice(),
                    b"Unit".as_slice(),
                ]
                .contains(&key)
                {
                    timer_directives.push(json!({"key":text(key),"value":text(value)}));
                    if key.starts_with(b"On") {
                        triggers.push(text(value));
                    }
                }
            }

            let unit_name = path.file_name().map(|name| name.as_bytes().to_vec());
            let enabled_by = unit_name
                .as_ref()
                .and_then(|name| enabled.get(name))
                .cloned()
                .unwrap_or_default();

            let data = json!({
                "type":"systemd_timer",
                "unit":unit_name.as_deref().map(text),
                "triggers":triggers,
                "timer_directives":timer_directives,
                "enabled_by":enabled_by.iter().map(|path| path_value(path)).collect::<Vec<_>>(),
                "source":path_value(&path),
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
            Path::new("/etc/crontab"),
            io::Error::new(
                io::ErrorKind::NotFound,
                "no schedule inventory source found",
            ),
            coverage,
        );
    }

    Ok(finish(coverage, partial, None))
}

fn paragraphs(bytes: &[u8]) -> Vec<&[u8]> {
    let mut output = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'\n' && bytes[index + 1] == b'\n' {
            if start < index {
                output.push(&bytes[start..index]);
            }
            index += 2;
            start = index;
        } else {
            index += 1;
        }
    }

    if start < bytes.len() {
        output.push(&bytes[start..]);
    }

    output
}

fn colon_fields(paragraph: &[u8]) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut fields = BTreeMap::new();
    for line in paragraph.split(|byte| *byte == b'\n') {
        if let Some(index) = line.iter().position(|byte| *byte == b':') {
            fields.insert(
                line[..index].to_vec(),
                trim_ascii(&line[index + 1..]).to_vec(),
            );
        }
    }
    fields
}

fn apk_fields(paragraph: &[u8]) -> BTreeMap<u8, Vec<u8>> {
    paragraph
        .split(|byte| *byte == b'\n')
        .filter_map(|line| Some((*line.first()?, line.get(2..)?.to_vec())))
        .collect()
}

fn is_cron_environment(line: &[u8]) -> bool {
    let Some(equal) = line.iter().position(|byte| *byte == b'=') else {
        return false;
    };
    !line[..equal].iter().any(|byte| byte.is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nix_v3_manifest_preserves_profile_state_and_store_paths() {
        let records = parse_nix_manifest(
            br#"{
                "version": 3,
                "elements": {
                    "curl": {
                        "active": false,
                        "pname": "curl",
                        "version": "8.12.1",
                        "storePaths": ["/nix/store/example-curl"]
                    }
                }
            }"#,
            Path::new("/nix/var/nix/profiles/default/manifest.json"),
        )
        .expect("parse manifest");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["name"]["display"], "curl");
        assert_eq!(records[0]["version"]["display"], "8.12.1");
        assert_eq!(records[0]["status"]["display"], "inactive");
        assert_eq!(
            records[0]["store_paths"][0]["display"],
            "/nix/store/example-curl"
        );
    }
    #[test]
    fn service_inventory_recognizes_every_systemd_unit_type() {
        for expected in [
            "service",
            "socket",
            "device",
            "mount",
            "automount",
            "swap",
            "target",
            "path",
            "timer",
            "slice",
            "scope",
        ] {
            assert_eq!(
                systemd_unit_type(format!("demo.{expected}").as_bytes()),
                Some(expected)
            );
        }
        assert_eq!(systemd_unit_type(b"demo.invalid"), None);
    }

    #[test]
    fn service_inventory_discovers_per_user_config_and_runtime_roots() {
        let mut managers = Vec::new();
        add_user_manager_roots(
            &mut managers,
            b"alice:x:1000:1000:Alice:/srv/alice:/bin/sh\n",
        );

        assert_eq!(managers.len(), 1);
        assert_eq!(
            managers[0].manager_user.as_deref(),
            Some(b"alice".as_slice())
        );
        assert_eq!(managers[0].manager_uid, Some(1000));
        assert!(
            managers[0]
                .roots
                .contains(&PathBuf::from("/srv/alice/.config/systemd/user"))
        );
        assert!(
            managers[0]
                .roots
                .contains(&PathBuf::from("/run/user/1000/systemd/transient"))
        );
    }
}
