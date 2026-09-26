use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value, json};
use zbus::blocking::fdo::DBusProxy;
use zbus::names::BusName;

use super::systemd::{ManagerTarget, connect, manager_targets};
use super::{emit_provider, finish, output_limit, path_value, text};
use crate::cli::{CapabilityId, Invocation, Request};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, Outcome, ProtocolError, RecordSink,
    check_deadline,
};

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match (&invocation.capability, &invocation.request) {
        (
            CapabilityId::DbusList,
            Request::DbusList {
                max_items,
                socket,
                user,
                all_users,
            },
        ) => list(
            sink,
            deadline,
            *max_items,
            socket.as_deref(),
            user.as_deref(),
            *all_users,
        ),
        (CapabilityId::DbusInspect, Request::DbusInspect { name, socket, user }) => {
            inspect(sink, deadline, name, socket.as_deref(), user.as_deref())
        }
        _ => Err(ProtocolError::Internal(
            "dbus capability request mismatch".into(),
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
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::DbusList;
    let mut coverage = Coverage::default();
    let (targets, discovery_error) = match manager_targets(socket, user, all_users) {
        Ok(targets) => targets,
        Err(error) => {
            provider_failure(
                sink,
                capability,
                socket.unwrap_or(Path::new("/run/dbus/system_bus_socket")),
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

    let mut successful_buses = 0_u64;

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

        let proxy = match DBusProxy::new(&connection) {
            Ok(proxy) => proxy,
            Err(error) => {
                partial = true;
                provider_failure(
                    sink,
                    capability,
                    &target.socket,
                    &error.to_string(),
                    &mut coverage,
                )?;
                continue;
            }
        };
        let owned = match proxy.list_names() {
            Ok(names) => names,
            Err(error) => {
                partial = true;
                provider_failure(
                    sink,
                    capability,
                    &target.socket,
                    &error.to_string(),
                    &mut coverage,
                )?;
                continue;
            }
        };

        let activatable = match proxy.list_activatable_names() {
            Ok(names) => names,
            Err(error) => {
                partial = true;
                property_failure(
                    sink,
                    capability,
                    &target.socket,
                    "ListActivatableNames",
                    &error.to_string(),
                    1,
                )?;
                Vec::new()
            }
        };

        let own_name = connection.unique_name().map(|name| name.as_str());
        let owned = owned
            .into_iter()
            .map(|name| name.as_str().to_owned())
            .filter(|name| Some(name.as_str()) != own_name)
            .collect::<BTreeSet<_>>();
        let activatable = activatable
            .into_iter()
            .map(|name| name.as_str().to_owned())
            .collect::<BTreeSet<_>>();

        successful_buses += 1;
        coverage.scanned = coverage
            .scanned
            .saturating_add((owned.len() + activatable.len()) as u64);

        if coverage.observed as usize >= max_items {
            return Ok(finish(coverage, true, Some("max_items")));
        }
        let (summary, summary_errors) = bus_summary(&proxy, &target);
        if summary_errors != 0 {
            partial = true;
            coverage.skipped = coverage.skipped.saturating_add(summary_errors);
            property_failure(
                sink,
                capability,
                &target.socket,
                "GetId/Features/Interfaces",
                "one or more D-Bus metadata queries failed",
                summary_errors,
            )?;
        }

        if !emit_provider(
            sink,
            capability,
            "dbus",
            Value::Object(summary),
            &mut coverage,
        )? {
            return Ok(output_limit(coverage));
        }

        let names = owned.union(&activatable).cloned().collect::<Vec<_>>();
        let mut credentials = BTreeMap::<String, Map<String, Value>>::new();
        let mut property_errors = 0_u64;

        for name in names {
            check_deadline(deadline)?;
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }
            let is_owned = owned.contains(&name);
            let is_activatable = activatable.contains(&name);
            let mut data = manager_data(&target, "name");
            data.insert("name".into(), json!(name));
            data.insert("owned".into(), json!(is_owned));
            data.insert("activatable".into(), json!(is_activatable));

            if is_owned {
                match owner_name(&proxy, &name) {
                    Ok(owner) => {
                        data.insert("owner".into(), json!(owner));
                        if let Some(cached) = credentials.get(&owner) {
                            data.extend(cached.clone());
                        } else {
                            match connection_credentials(&proxy, &owner) {
                                Ok(values) => {
                                    data.extend(values.clone());
                                    credentials.insert(owner, values);
                                }
                                Err(_) => {
                                    partial = true;
                                    property_errors += 1;
                                    coverage.skipped += 1;
                                }
                            }
                        }
                    }
                    Err(_) => {
                        partial = true;
                        property_errors += 1;
                        coverage.skipped += 1;
                    }
                }
            }

            if !emit_provider(sink, capability, "dbus", Value::Object(data), &mut coverage)? {
                return Ok(output_limit(coverage));
            }
        }

        if property_errors != 0 {
            property_failure(
                sink,
                capability,
                &target.socket,
                "GetNameOwner/GetConnectionCredentials",
                "one or more D-Bus owner queries failed",
                property_errors,
            )?;
        }
    }

    if successful_buses == 0 {
        return Ok(unavailable_report(coverage));
    }

    Ok(finish(coverage, partial, None))
}

fn inspect<W: Write>(
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    name: &str,
    socket: Option<&Path>,
    user: Option<&str>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::DbusInspect;
    let mut coverage = Coverage::default();
    let target = match manager_targets(socket, user, false) {
        Ok((mut targets, _)) => targets.remove(0),
        Err(error) => {
            provider_failure(
                sink,
                capability,
                socket.unwrap_or(Path::new("/run/dbus/system_bus_socket")),
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
    let proxy = match DBusProxy::new(&connection) {
        Ok(proxy) => proxy,
        Err(error) => {
            provider_failure(
                sink,
                capability,
                &target.socket,
                &error.to_string(),
                &mut coverage,
            )?;
            return Ok(unavailable_report(coverage));
        }
    };
    let bus_name = match BusName::try_from(name) {
        Ok(name) => name,
        Err(error) => {
            name_failure(
                sink,
                capability,
                &target.socket,
                name,
                &error.to_string(),
                &mut coverage,
            )?;
            return Ok(unavailable_report(coverage));
        }
    };

    let owned = match proxy.name_has_owner(bus_name.clone()) {
        Ok(owned) => owned,
        Err(error) => {
            provider_failure(
                sink,
                capability,
                &target.socket,
                &error.to_string(),
                &mut coverage,
            )?;
            return Ok(unavailable_report(coverage));
        }
    };

    let (activatable, activatable_error) = match proxy.list_activatable_names() {
        Ok(names) => (
            names.iter().any(|candidate| candidate.as_str() == name),
            None,
        ),
        Err(error) => (false, Some(error.to_string())),
    };

    coverage.scanned += 1;

    let mut data = manager_data(&target, "name");
    data.insert("name".into(), json!(name));
    data.insert("owned".into(), json!(owned));
    data.insert("activatable".into(), json!(activatable));
    let mut partial = false;
    if let Some(error) = activatable_error {
        partial = true;
        coverage.skipped += 1;
        property_failure(
            sink,
            capability,
            &target.socket,
            "ListActivatableNames",
            &error,
            1,
        )?;
    }
    if owned {
        match owner_name(&proxy, name) {
            Ok(owner) => {
                data.insert("owner".into(), json!(owner));
                match connection_credentials(&proxy, &owner) {
                    Ok(values) => data.extend(values),
                    Err(error) => {
                        partial = true;
                        coverage.skipped += 1;
                        property_failure(
                            sink,
                            capability,
                            &target.socket,
                            "GetConnectionCredentials",
                            &error,
                            1,
                        )?;
                    }
                }
            }
            Err(error) => {
                partial = true;
                coverage.skipped += 1;
                property_failure(sink, capability, &target.socket, "GetNameOwner", &error, 1)?;
            }
        }
    }

    if !emit_provider(sink, capability, "dbus", Value::Object(data), &mut coverage)? {
        return Ok(output_limit(coverage));
    }

    Ok(finish(coverage, partial, None))
}

fn bus_summary(proxy: &DBusProxy<'_>, target: &ManagerTarget) -> (Map<String, Value>, u64) {
    let mut data = manager_data(target, "bus");
    let mut errors = 0_u64;
    match proxy.get_id() {
        Ok(id) => {
            data.insert("bus_id".into(), json!(id.as_str()));
        }
        Err(_) => errors += 1,
    }
    match proxy.features() {
        Ok(features) => {
            data.insert("features".into(), json!(features));
        }
        Err(_) => errors += 1,
    }
    match proxy.interfaces() {
        Ok(interfaces) => {
            data.insert(
                "interfaces".into(),
                json!(
                    interfaces
                        .iter()
                        .map(|interface| interface.as_str())
                        .collect::<Vec<_>>()
                ),
            );
        }
        Err(_) => errors += 1,
    }
    (data, errors)
}

fn manager_data(target: &ManagerTarget, record_type: &'static str) -> Map<String, Value> {
    let mut data = Map::new();
    data.insert("record_type".into(), json!(record_type));
    data.insert("manager_scope".into(), json!(target.scope));
    data.insert("socket".into(), path_value(&target.socket));
    if let Some(user) = target.user.as_deref() {
        data.insert("manager_user".into(), text(user));
    }
    if let Some(uid) = target.uid {
        data.insert("manager_uid".into(), json!(uid));
    }
    data
}

fn owner_name(proxy: &DBusProxy<'_>, name: &str) -> Result<String, String> {
    if name.starts_with(':') {
        return Ok(name.to_owned());
    }
    let name = BusName::try_from(name).map_err(|error| error.to_string())?;
    proxy
        .get_name_owner(name)
        .map(|owner| owner.as_str().to_owned())
        .map_err(|error| error.to_string())
}

fn connection_credentials(
    proxy: &DBusProxy<'_>,
    owner: &str,
) -> Result<Map<String, Value>, String> {
    let owner = BusName::try_from(owner).map_err(|error| error.to_string())?;
    let credentials = proxy
        .get_connection_credentials(owner)
        .map_err(|error| error.to_string())?;
    let mut data = Map::new();
    if let Some(uid) = credentials.unix_user_id() {
        data.insert("owner_uid".into(), json!(uid));
    }
    if let Some(gids) = credentials.unix_group_ids() {
        data.insert("owner_gids".into(), json!(gids));
    }
    if let Some(pid) = credentials.process_id() {
        data.insert("owner_pid".into(), json!(pid));
    }
    if let Some(label) = credentials.linux_security_label() {
        data.insert("owner_security_label".into(), text(label));
    }
    Ok(data)
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
        "dbus",
        path_value(socket),
        error,
        1,
    )
}

fn property_failure<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    socket: &Path,
    operation: &str,
    error: &str,
    count: u64,
) -> Result<(), ProtocolError> {
    sink.diagnostic_provider(
        capability.id(),
        "property",
        "partial",
        "property_errors",
        "dbus",
        json!({"socket":path_value(socket),"operation":{"display":operation}}),
        error,
        count,
    )
}

fn name_failure<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    socket: &Path,
    name: &str,
    error: &str,
    coverage: &mut Coverage,
) -> Result<(), ProtocolError> {
    coverage.skipped += 1;
    sink.diagnostic_provider(
        capability.id(),
        "name",
        "malformed",
        "source_malformed",
        "dbus",
        json!({"socket":path_value(socket),"name":{"display":name}}),
        error,
        1,
    )
}

fn unavailable_report(coverage: Coverage) -> CapabilityReport {
    CapabilityReport {
        outcome: Outcome::Unavailable,
        coverage,
        limits_hit: Vec::new(),
    }
}
