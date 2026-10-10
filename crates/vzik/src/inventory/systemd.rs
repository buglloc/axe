use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::{
    emit_provider, finish, output_limit, path_value, read_bounded, systemd_unit_type, text,
};
use crate::cli::{CapabilityId, Invocation, Request, SystemdUnitSelection};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, Outcome, ProtocolError, RecordSink,
    check_deadline,
};

const SYSTEM_BUS_SOCKET: &str = "/run/dbus/system_bus_socket";
const SYSTEMD_DESTINATION: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER_INTERFACE: &str = "org.freedesktop.systemd1.Manager";
const UNIT_INTERFACE: &str = "org.freedesktop.systemd1.Unit";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const PASSWD_BYTES: u64 = 1 << 20;
const METHOD_TIMEOUT: Duration = Duration::from_secs(5);

type ListedUnit = (
    String,
    String,
    String,
    String,
    String,
    String,
    OwnedObjectPath,
    u32,
    String,
    OwnedObjectPath,
);

#[derive(Clone)]
pub(super) struct ManagerTarget {
    pub(super) selection_scope: &'static str,
    pub(super) user: Option<Vec<u8>>,
    pub(super) uid: Option<u32>,
    pub(super) socket: PathBuf,
}

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match (&invocation.capability, &invocation.request) {
        (
            CapabilityId::SystemctlList,
            Request::SystemctlList {
                max_items,
                socket,
                user,
                all_users,
                unit_selection,
            },
        ) => list(
            sink,
            deadline,
            *max_items,
            socket.as_deref(),
            user.as_deref(),
            *all_users,
            *unit_selection,
        ),
        (CapabilityId::SystemctlInspect, Request::SystemctlInspect { unit, socket, user }) => {
            inspect(sink, deadline, unit, socket.as_deref(), user.as_deref())
        }
        _ => Err(ProtocolError::Internal(
            "systemctl capability request mismatch".into(),
        )),
    }
}

fn list<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    max_items: usize,
    socket: Option<&Path>,
    user: Option<&str>,
    all_users: bool,
    unit_selection: SystemdUnitSelection,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::SystemctlList;
    let mut coverage = Coverage::default();
    let (targets, discovery_error) = match manager_targets(socket, user, all_users) {
        Ok(targets) => targets,
        Err(error) => {
            provider_failure(
                sink,
                capability,
                socket.unwrap_or(Path::new(SYSTEM_BUS_SOCKET)),
                &error.to_string(),
                &mut coverage,
            )?;
            return Ok(unavailable_report(coverage));
        }
    };

    let mut partial = false;
    if let Some(error) = discovery_error {
        partial = true;
        provider_failure(
            sink,
            capability,
            Path::new("/run/user"),
            &error.to_string(),
            &mut coverage,
        )?;
    }

    let mut successful_managers = 0_u64;

    for target in targets {
        check_deadline(deadline)?;
        let connection = match connect(&target.socket, deadline) {
            Ok(connection) => connection,
            Err(error) => {
                partial = true;
                provider_failure(sink, capability, &target.socket, &error, &mut coverage)?;
                continue;
            }
        };
        let mut units = match list_units(&connection) {
            Ok(units) => units,
            Err(error) => {
                partial = true;
                provider_failure(sink, capability, &target.socket, &error, &mut coverage)?;
                continue;
            }
        };

        successful_managers += 1;
        coverage.scanned = coverage.scanned.saturating_add(units.len() as u64);
        units.sort_by(|left, right| left.0.cmp(&right.0));
        let mut property_errors = 0_u64;

        for unit in units {
            check_deadline(deadline)?;
            if !unit_selected(unit_selection, &unit.0) {
                continue;
            }

            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }

            let mut data = listed_unit_data(&target, &unit);
            if enrich_unit(&connection, &unit.0, &unit.6, &mut data).is_err() {
                partial = true;
                property_errors += 1;
                coverage.skipped += 1;
            }

            if !emit_provider(
                sink,
                capability,
                "systemd_dbus",
                Value::Object(data),
                &mut coverage,
            )? {
                return Ok(output_limit(coverage));
            }
        }

        if property_errors != 0 {
            property_diagnostic(sink, capability, &target.socket, property_errors)?;
        }
    }

    if successful_managers == 0 {
        return Ok(unavailable_report(coverage));
    }

    Ok(finish(coverage, partial, None))
}

fn inspect<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    unit: &str,
    socket: Option<&Path>,
    user: Option<&str>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::SystemctlInspect;
    let mut coverage = Coverage::default();
    let target = match manager_targets(socket, user, false) {
        Ok((mut targets, _)) => targets.remove(0),
        Err(error) => {
            provider_failure(
                sink,
                capability,
                socket.unwrap_or(Path::new(SYSTEM_BUS_SOCKET)),
                &error.to_string(),
                &mut coverage,
            )?;
            return Ok(unavailable_report(coverage));
        }
    };

    check_deadline(deadline)?;

    let connection = match connect(&target.socket, deadline) {
        Ok(connection) => connection,
        Err(error) => {
            provider_failure(sink, capability, &target.socket, &error, &mut coverage)?;
            return Ok(unavailable_report(coverage));
        }
    };
    let object_path = match get_unit(&connection, unit) {
        Ok(path) => path,
        Err(error) => {
            unit_failure(
                sink,
                capability,
                &target.socket,
                unit,
                &error,
                &mut coverage,
            )?;
            return Ok(unavailable_report(coverage));
        }
    };

    let unit_properties = match get_all(&connection, &object_path, UNIT_INTERFACE) {
        Ok(properties) => properties,
        Err(error) => {
            provider_failure(sink, capability, &target.socket, &error, &mut coverage)?;
            return Ok(unavailable_report(coverage));
        }
    };
    coverage.scanned += 1;

    let listed = (
        property_string(&unit_properties, "Id").unwrap_or_else(|| unit.to_owned()),
        property_string(&unit_properties, "Description").unwrap_or_default(),
        property_string(&unit_properties, "LoadState").unwrap_or_else(|| "unknown".into()),
        property_string(&unit_properties, "ActiveState").unwrap_or_else(|| "unknown".into()),
        property_string(&unit_properties, "SubState").unwrap_or_else(|| "unknown".into()),
        property_string(&unit_properties, "Following").unwrap_or_default(),
        object_path,
        0,
        String::new(),
        OwnedObjectPath::try_from("/").expect("root is a valid D-Bus object path"),
    );
    let mut data = listed_unit_data(&target, &listed);
    insert_unit_properties(&unit_properties, &mut data);
    let partial = enrich_type_properties(&connection, &listed.0, &listed.6, &mut data).is_err();
    if partial {
        coverage.skipped += 1;
        property_diagnostic(sink, capability, &target.socket, 1)?;
    }

    if !emit_provider(
        sink,
        capability,
        "systemd_dbus",
        Value::Object(data),
        &mut coverage,
    )? {
        return Ok(output_limit(coverage));
    }

    Ok(finish(coverage, partial, None))
}

pub(super) fn manager_targets(
    socket: Option<&Path>,
    user: Option<&str>,
    all_users: bool,
) -> io::Result<(Vec<ManagerTarget>, Option<io::Error>)> {
    if let Some(user) = user {
        let uid = resolve_user(user)?;
        let selection_scope = if socket.is_some() {
            "explicit_socket"
        } else {
            "user"
        };
        let socket = socket
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}/bus")));
        return Ok((
            vec![ManagerTarget {
                selection_scope,
                user: Some(user.as_bytes().to_vec()),
                uid: Some(uid),
                socket,
            }],
            None,
        ));
    }

    if let Some(socket) = socket {
        return Ok((
            vec![ManagerTarget {
                selection_scope: "explicit_socket",
                user: None,
                uid: None,
                socket: socket.to_path_buf(),
            }],
            None,
        ));
    }

    let mut targets = vec![ManagerTarget {
        selection_scope: "system",
        user: None,
        uid: None,
        socket: PathBuf::from(SYSTEM_BUS_SOCKET),
    }];
    let discovery_error = if all_users {
        let (users, error) = discover_user_managers();
        targets.extend(users);
        error
    } else {
        None
    };
    Ok((targets, discovery_error))
}

fn discover_user_managers() -> (Vec<ManagerTarget>, Option<io::Error>) {
    let mut users = passwd_users().unwrap_or_default();
    let mut targets = Vec::new();
    let entries = match std::fs::read_dir("/run/user") {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (targets, None),
        Err(error) => return (targets, Some(error)),
    };

    let mut discovery_error = None;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                discovery_error.get_or_insert(error);
                continue;
            }
        };
        let Some(uid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };

        let socket = entry.path().join("bus");
        match socket.try_exists() {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                discovery_error.get_or_insert(error);
                continue;
            }
        }

        targets.push(ManagerTarget {
            selection_scope: "user",
            user: users.remove(&uid),
            uid: Some(uid),
            socket,
        });
    }
    targets.sort_by_key(|target| target.uid);
    (targets, discovery_error)
}

fn resolve_user(user: &str) -> io::Result<u32> {
    if let Ok(uid) = user.parse::<u32>() {
        return Ok(uid);
    }

    let users = passwd_entries()?;
    users
        .into_iter()
        .find_map(|(name, uid)| (name == user.as_bytes()).then_some(uid))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "local user was not found"))
}

fn passwd_users() -> io::Result<BTreeMap<u32, Vec<u8>>> {
    Ok(passwd_entries()?
        .into_iter()
        .map(|(name, uid)| (uid, name))
        .collect())
}

fn passwd_entries() -> io::Result<Vec<(Vec<u8>, u32)>> {
    let (bytes, _) = read_bounded(Path::new("/etc/passwd"), PASSWD_BYTES)?;

    let mut users = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        let fields = line.split(|byte| *byte == b':').collect::<Vec<_>>();
        if fields.len() < 3 {
            continue;
        }
        let Ok(uid) = std::str::from_utf8(fields[2])
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or(())
        else {
            continue;
        };
        users.push((fields[0].to_vec(), uid));
    }
    Ok(users)
}

pub(super) fn connect(socket: &Path, deadline: ExecutionContext<'_>) -> Result<Connection, String> {
    let address = format!(
        "unix:path={}",
        dbus_address_path(socket.as_os_str().as_bytes())
    );
    let timeout = deadline
        .remaining()
        .map_err(|error| error.to_string())?
        .min(METHOD_TIMEOUT);

    zbus::blocking::connection::Builder::address(address.as_str())
        .map_err(|error| error.to_string())?
        .method_timeout(timeout)
        .build()
        .map_err(|error| error.to_string())
}

fn dbus_address_path(path: &[u8]) -> String {
    let mut encoded = String::with_capacity(path.len());
    for &byte in path {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(encoded, "%{byte:02X}").expect("write to string");
        }
    }
    encoded
}

fn manager_proxy(connection: &Connection) -> Result<Proxy<'_>, String> {
    Proxy::new(
        connection,
        SYSTEMD_DESTINATION,
        MANAGER_PATH,
        MANAGER_INTERFACE,
    )
    .map_err(|error| error.to_string())
}

fn list_units(connection: &Connection) -> Result<Vec<ListedUnit>, String> {
    manager_proxy(connection)?
        .call("ListUnits", &())
        .map_err(|error| error.to_string())
}

fn get_unit(connection: &Connection, unit: &str) -> Result<OwnedObjectPath, String> {
    manager_proxy(connection)?
        .call("GetUnit", &(unit,))
        .map_err(|error| error.to_string())
}

fn get_all(
    connection: &Connection,
    object_path: &OwnedObjectPath,
    interface: &str,
) -> Result<HashMap<String, OwnedValue>, String> {
    Proxy::new(
        connection,
        SYSTEMD_DESTINATION,
        object_path,
        PROPERTIES_INTERFACE,
    )
    .map_err(|error| error.to_string())?
    .call("GetAll", &(interface,))
    .map_err(|error| error.to_string())
}

fn listed_unit_data(target: &ManagerTarget, unit: &ListedUnit) -> Map<String, Value> {
    let mut data = Map::new();
    data.insert("name".into(), json!(unit.0));
    data.insert("description".into(), json!(unit.1));
    data.insert("load_state".into(), json!(unit.2));
    data.insert("active_state".into(), json!(unit.3));
    data.insert("sub_state".into(), json!(unit.4));
    data.insert("following".into(), json!(unit.5));
    data.insert("object_path".into(), json!(unit.6.as_str()));
    data.insert("unit_type".into(), json!(unit_type(&unit.0)));
    data.insert("selection_scope".into(), json!(target.selection_scope));
    data.insert("socket".into(), path_value(&target.socket));
    if let Some(user) = target.user.as_deref() {
        data.insert("manager_user".into(), text(user));
    }
    if let Some(uid) = target.uid {
        data.insert("manager_uid".into(), json!(uid));
    }
    if unit.7 != 0 {
        data.insert("job_id".into(), json!(unit.7));
        data.insert("job_type".into(), json!(unit.8));
        data.insert("job_path".into(), json!(unit.9.as_str()));
    }
    data
}

fn enrich_unit(
    connection: &Connection,
    name: &str,
    object_path: &OwnedObjectPath,
    data: &mut Map<String, Value>,
) -> Result<(), String> {
    let properties = get_all(connection, object_path, UNIT_INTERFACE)?;
    insert_unit_properties(&properties, data);
    enrich_type_properties(connection, name, object_path, data)
}

fn insert_unit_properties(properties: &HashMap<String, OwnedValue>, data: &mut Map<String, Value>) {
    insert_string(properties, "UnitFileState", "unit_file_state", data);
    insert_u64(
        properties,
        "ActiveEnterTimestamp",
        "active_enter_timestamp_usec",
        data,
    );
    insert_u64(
        properties,
        "InactiveEnterTimestamp",
        "inactive_enter_timestamp_usec",
        data,
    );
}

fn enrich_type_properties(
    connection: &Connection,
    name: &str,
    object_path: &OwnedObjectPath,
    data: &mut Map<String, Value>,
) -> Result<(), String> {
    let Some(interface) = type_interface(name) else {
        return Ok(());
    };
    let properties = get_all(connection, object_path, interface)?;

    match unit_type(name) {
        "service" => {
            insert_u32(&properties, "MainPID", "main_pid", data);
            insert_u32(&properties, "ExecMainPID", "exec_main_pid", data);
            insert_i32(&properties, "ExecMainCode", "exec_main_code", data);
            insert_i32(&properties, "ExecMainStatus", "exec_main_status", data);
            insert_string(&properties, "Result", "result", data);
            insert_u32(&properties, "NRestarts", "restart_count", data);
        }
        "socket" => {
            insert_string(&properties, "Result", "result", data);
            insert_u32(&properties, "NAccepted", "accepted_connections", data);
            insert_u32(&properties, "NConnections", "current_connections", data);
        }
        "timer" => {
            insert_string(&properties, "Result", "result", data);
            insert_u64(
                &properties,
                "NextElapseUSecRealtime",
                "next_elapse_realtime_usec",
                data,
            );
            insert_u64(
                &properties,
                "NextElapseUSecMonotonic",
                "next_elapse_monotonic_usec",
                data,
            );
            insert_u64(&properties, "LastTriggerUSec", "last_trigger_usec", data);
        }
        _ => {
            insert_string(&properties, "Result", "result", data);
        }
    }
    Ok(())
}

fn property_string(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    properties.get(name)?.try_clone().ok()?.try_into().ok()
}

fn property_u32(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<u32> {
    properties.get(name)?.try_clone().ok()?.try_into().ok()
}

fn property_i32(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<i32> {
    properties.get(name)?.try_clone().ok()?.try_into().ok()
}

fn property_u64(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<u64> {
    properties.get(name)?.try_clone().ok()?.try_into().ok()
}

fn insert_string(
    properties: &HashMap<String, OwnedValue>,
    property: &str,
    field: &str,
    data: &mut Map<String, Value>,
) {
    if let Some(value) = property_string(properties, property) {
        data.insert(field.into(), json!(value));
    }
}

fn insert_u32(
    properties: &HashMap<String, OwnedValue>,
    property: &str,
    field: &str,
    data: &mut Map<String, Value>,
) {
    if let Some(value) = property_u32(properties, property) {
        data.insert(field.into(), json!(value));
    }
}

fn insert_i32(
    properties: &HashMap<String, OwnedValue>,
    property: &str,
    field: &str,
    data: &mut Map<String, Value>,
) {
    if let Some(value) = property_i32(properties, property) {
        data.insert(field.into(), json!(value));
    }
}

fn insert_u64(
    properties: &HashMap<String, OwnedValue>,
    property: &str,
    field: &str,
    data: &mut Map<String, Value>,
) {
    if let Some(value) = property_u64(properties, property) {
        data.insert(field.into(), json!(value));
    }
}

fn unit_selected(selection: SystemdUnitSelection, name: &str) -> bool {
    selection == SystemdUnitSelection::All || unit_type(name) != "device"
}

fn unit_type(name: &str) -> &'static str {
    systemd_unit_type(name.as_bytes()).unwrap_or("unknown")
}

fn type_interface(name: &str) -> Option<&'static str> {
    Some(match unit_type(name) {
        "service" => "org.freedesktop.systemd1.Service",
        "socket" => "org.freedesktop.systemd1.Socket",
        "device" => "org.freedesktop.systemd1.Device",
        "mount" => "org.freedesktop.systemd1.Mount",
        "automount" => "org.freedesktop.systemd1.Automount",
        "swap" => "org.freedesktop.systemd1.Swap",
        "target" => "org.freedesktop.systemd1.Target",
        "path" => "org.freedesktop.systemd1.Path",
        "timer" => "org.freedesktop.systemd1.Timer",
        "slice" => "org.freedesktop.systemd1.Slice",
        "scope" => "org.freedesktop.systemd1.Scope",
        _ => return None,
    })
}

fn provider_failure<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    socket: &Path,
    error: &str,
    coverage: &mut Coverage,
) -> Result<(), ProtocolError> {
    coverage.skipped += 1;
    sink.diagnostic_provider(
        capability.id(),
        "provider",
        "unavailable",
        "provider_unavailable",
        "systemd_dbus",
        path_value(socket),
        error,
        1,
    )
}

fn unit_failure<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    socket: &Path,
    unit: &str,
    error: &str,
    coverage: &mut Coverage,
) -> Result<(), ProtocolError> {
    coverage.skipped += 1;
    sink.diagnostic_provider(
        capability.id(),
        "unit",
        "not_found",
        "systemd_unit_not_found",
        "systemd_dbus",
        json!({"socket":path_value(socket),"unit":{"display":unit}}),
        error,
        1,
    )
}

fn property_diagnostic<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    socket: &Path,
    errors: u64,
) -> Result<(), ProtocolError> {
    sink.diagnostic_provider(
        capability.id(),
        "property",
        "partial",
        "property_errors",
        "systemd_dbus",
        path_value(socket),
        &format!("{errors} unit property queries failed"),
        errors,
    )
}

fn unavailable_report(coverage: Coverage) -> CapabilityReport {
    CapabilityReport {
        outcome: Outcome::Unavailable,
        coverage,
        limits_hit: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbus_socket_path_preserves_arbitrary_linux_bytes() {
        assert_eq!(dbus_address_path(b"/tmp/a,b=%\xff"), "/tmp/a%2Cb%3D%25%FF");
    }

    #[test]
    fn baseline_excludes_only_runtime_device_units() {
        assert!(unit_selected(SystemdUnitSelection::All, "dev-vda.device"));
        assert!(!unit_selected(
            SystemdUnitSelection::SecurityBaseline,
            "dev-vda.device"
        ));
        assert!(unit_selected(
            SystemdUnitSelection::SecurityBaseline,
            "sshd.service"
        ));
        assert!(unit_selected(
            SystemdUnitSelection::SecurityBaseline,
            "vendor.unknown"
        ));
    }
}
