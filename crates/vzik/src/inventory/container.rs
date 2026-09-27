use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::porto::{self, Client, ContainerSnapshot, Error as PortoError, ResponseError};
use super::{finish, output_limit, path_value, read_bounded, sorted_entries, text};
use crate::cli::{CapabilityId, Invocation, Request};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, Outcome, ProtocolError, RecordSink,
    check_deadline,
};

const PORTO_CALL_TIMEOUT: Duration = Duration::from_secs(5);
const PORTO_BATCH_SIZE: usize = 64;
const PROPERTY_CHUNK_BYTES: usize = 16 << 10;
const SUMMARY_VALUE_BYTES: usize = 16 << 10;
const MAX_CONTAINER_NAME_BYTES: usize = 4096;
const MAX_PROPERTY_NAME_BYTES: usize = 1024;

#[derive(Clone, Copy)]
struct Options<'a> {
    name: Option<&'a str>,
    socket: &'a Path,
    max_items: usize,
    show_sensitive: bool,
    include_streams: bool,
    max_stream_bytes: usize,
}

struct PortoObservation {
    catalog: Vec<PropertyMetadata>,
    containers: Vec<ObservedContainer>,
    property_errors: usize,
    truncated: bool,
    item_limit_hit: bool,
    stream_truncated: bool,
    catalog_truncated: bool,
    skipped: usize,
}

struct PropertyMetadata {
    name: String,
    description: String,
    read_only: bool,
    dynamic: bool,
}

struct ObservedContainer {
    summary: Map<String, Value>,
    properties: Vec<ObservedProperty>,
}

struct ObservedProperty {
    name: String,
    value: Option<String>,
    error: Option<PropertyError>,
    redacted: bool,
    truncated: bool,
}

struct PropertyError {
    code: i32,
    message: String,
}

struct NativeContainer {
    id: String,
    runtime: &'static str,
    init_pid: u32,
    cgroup: Vec<u8>,
    source: PathBuf,
    root: Option<PathBuf>,
    namespaces: BTreeMap<String, PathBuf>,
}

enum PortoCollectError {
    Control(ProtocolError),
    Api(PortoError),
}

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match invocation.capability {
        CapabilityId::ContainerList => container_list(invocation, sink, deadline),
        CapabilityId::ContainerInspect => container_inspect(invocation, sink, deadline),
        CapabilityId::PortoList => porto_list(invocation, sink, deadline),
        CapabilityId::PortoInspect => porto_inspect(invocation, sink, deadline),
        _ => unreachable!("container dispatcher received unrelated capability"),
    }
}

fn container_list<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let max_items = super::item_limit(invocation)?;
    let capability = CapabilityId::ContainerList;
    let mut coverage = Coverage::default();
    let (containers, partial, limit_hit) =
        discover_native(sink, capability, max_items, deadline, &mut coverage)?;

    emit_provider_selected(sink, capability, "procfs")?;
    for container in containers {
        let observed = native_observation(container, false);
        if !emit_container(sink, capability, "procfs", &observed, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }
    Ok(finish(coverage, partial, limit_hit.then_some("max_items")))
}

fn container_inspect<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let Request::ContainerInspect { name } = &invocation.request else {
        return Err(ProtocolError::Internal(
            "container.inspect request mismatch".into(),
        ));
    };
    let capability = CapabilityId::ContainerInspect;
    let mut coverage = Coverage::default();
    if name == "self" {
        let container = inspect_self(deadline, &mut coverage)?;
        emit_provider_selected(sink, capability, "procfs")?;
        if !emit_container(sink, capability, "procfs", &container, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
        let partial = coverage.denied > 0 || coverage.vanished > 0 || coverage.truncated;
        return Ok(finish(coverage, partial, None));
    }

    let (containers, partial, limit_hit) =
        discover_native(sink, capability, 10_000, deadline, &mut coverage)?;
    if let Some(container) = containers
        .into_iter()
        .find(|container| container.id == *name)
    {
        emit_provider_selected(sink, capability, "procfs")?;
        let container = native_observation(container, false);
        if !emit_container(sink, capability, "procfs", &container, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
        return Ok(finish(
            coverage,
            partial,
            limit_hit.then_some("native_scan_limit"),
        ));
    }
    unavailable_container(sink, capability, name, coverage)
}

fn porto_list<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let Request::PortoList {
        max_items,
        socket,
        show_sensitive,
        include_streams,
        max_stream_bytes,
    } = &invocation.request
    else {
        return Err(ProtocolError::Internal(
            "porto.list request mismatch".into(),
        ));
    };
    let options = Options {
        name: None,
        socket,
        max_items: *max_items,
        show_sensitive: *show_sensitive,
        include_streams: *include_streams,
        max_stream_bytes: *max_stream_bytes,
    };

    let capability = CapabilityId::PortoList;
    let mut coverage = Coverage::default();
    let observation = match collect_porto(options, deadline) {
        Ok(observation) => observation,
        Err(PortoCollectError::Control(error)) => return Err(error),
        Err(PortoCollectError::Api(error)) => {
            let _ = porto_failure(sink, capability, socket, &error, &mut coverage)?;
            return Ok(CapabilityReport {
                outcome: Outcome::Unavailable,
                coverage,
                limits_hit: Vec::new(),
            });
        }
    };

    emit_provider_selected(sink, capability, "porto_api")?;
    coverage.skipped = coverage.skipped.saturating_add(observation.skipped as u64);
    if observation.property_errors > 0 {
        emit_property_errors(sink, capability, socket, observation.property_errors)?;
    }
    if !emit_catalog(sink, capability, &observation.catalog, &mut coverage)? {
        return Ok(output_limit(coverage));
    }
    for container in &observation.containers {
        if !emit_container(sink, capability, "porto_api", container, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    let mut report = finish(
        coverage,
        observation.property_errors > 0 || observation.truncated,
        observation.item_limit_hit.then_some("max_items"),
    );
    if observation.stream_truncated {
        report.limits_hit.push("max_stream_bytes");
    }
    if observation.catalog_truncated {
        report.limits_hit.push("max_property_chunk_bytes");
    }
    Ok(report)
}

fn porto_inspect<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let Request::PortoInspect {
        name,
        socket,
        show_sensitive,
        include_streams,
        max_stream_bytes,
    } = &invocation.request
    else {
        return Err(ProtocolError::Internal(
            "porto.inspect request mismatch".into(),
        ));
    };

    let options = Options {
        name: Some(name),
        socket,
        max_items: 1,
        show_sensitive: *show_sensitive,
        include_streams: *include_streams,
        max_stream_bytes: *max_stream_bytes,
    };
    let capability = CapabilityId::PortoInspect;
    let mut coverage = Coverage::default();
    let mut observation = match collect_porto(options, deadline) {
        Ok(observation) => observation,
        Err(PortoCollectError::Control(error)) => return Err(error),
        Err(PortoCollectError::Api(error)) => {
            let _ = porto_failure(sink, capability, socket, &error, &mut coverage)?;
            return Ok(CapabilityReport {
                outcome: Outcome::Unavailable,
                coverage,
                limits_hit: Vec::new(),
            });
        }
    };

    emit_provider_selected(sink, capability, "porto_api")?;
    if observation.property_errors > 0 {
        emit_property_errors(sink, capability, socket, observation.property_errors)?;
    }
    if !emit_catalog(sink, capability, &observation.catalog, &mut coverage)? {
        return Ok(output_limit(coverage));
    }
    let Some(container) = observation.containers.pop() else {
        return unavailable_container(sink, capability, name, coverage);
    };
    if !emit_container(sink, capability, "porto_api", &container, &mut coverage)? {
        return Ok(output_limit(coverage));
    }

    let mut report = finish(
        coverage,
        observation.property_errors > 0 || observation.truncated,
        observation.stream_truncated.then_some("max_stream_bytes"),
    );
    if observation.catalog_truncated {
        report.limits_hit.push("max_property_chunk_bytes");
    }
    Ok(report)
}

fn collect_porto(
    options: Options<'_>,
    deadline: ExecutionContext<'_>,
) -> Result<PortoObservation, PortoCollectError> {
    let timeout = porto_timeout(deadline)?;
    let mut client = Client::connect(options.socket).map_err(PortoCollectError::Api)?;
    let properties = client
        .list_properties(timeout)
        .map_err(PortoCollectError::Api)?;
    let property_count = properties.len();
    let mut catalog = properties
        .into_iter()
        .filter(|property| {
            !property.name.is_empty() && property.name.len() <= MAX_PROPERTY_NAME_BYTES
        })
        .map(|property| PropertyMetadata {
            name: property.name,
            description: property.desc,
            read_only: property.read_only.unwrap_or(false),
            dynamic: property.dynamic.unwrap_or(false),
        })
        .collect::<Vec<_>>();
    let mut skipped = property_count.saturating_sub(catalog.len());
    catalog.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    let catalog_count = catalog.len();
    catalog.dedup_by(|left, right| left.name == right.name);
    skipped = skipped.saturating_add(catalog_count.saturating_sub(catalog.len()));
    let catalog_truncated_count = catalog
        .iter()
        .filter(|property| property.description.len() > PROPERTY_CHUNK_BYTES)
        .count();
    skipped = skipped.saturating_add(catalog_truncated_count);
    let catalog_truncated = catalog_truncated_count > 0;
    let variables = catalog
        .iter()
        .filter(|property| {
            (options.show_sensitive || !is_sensitive(&property.name))
                && (options.include_streams || !is_stream(&property.name))
        })
        .map(|property| porto_query(&property.name, options.max_stream_bytes))
        .collect::<Vec<_>>();

    let mut names = if let Some(name) = options.name {
        vec![name.to_owned()]
    } else {
        client
            .list(porto_timeout(deadline)?)
            .map_err(PortoCollectError::Api)?
    };
    let listed_names = names.len();
    let reserved_names = if options.name.is_none() {
        names
            .iter()
            .filter(|name| name.as_str() == "/" || name.as_str() == "self")
            .count()
    } else {
        0
    };
    names.retain(|name| {
        if options.name.is_none() && (name == "/" || name == "self") {
            return false;
        }
        !name.is_empty() && name.len() <= MAX_CONTAINER_NAME_BYTES
    });
    skipped = skipped.saturating_add(
        listed_names
            .saturating_sub(names.len())
            .saturating_sub(reserved_names),
    );
    names.sort_unstable();
    let valid_names = names.len();
    names.dedup();
    skipped = skipped.saturating_add(valid_names.saturating_sub(names.len()));

    let item_limit_skipped = if options.name.is_none() {
        names.len().saturating_sub(options.max_items)
    } else {
        0
    };
    let item_limit_hit = item_limit_skipped > 0;
    skipped = skipped.saturating_add(item_limit_skipped);
    if options.name.is_none() {
        names.truncate(options.max_items);
    }

    let mut snapshots = Vec::with_capacity(names.len().saturating_add(1));
    for batch in names.chunks(PORTO_BATCH_SIZE) {
        let response = client
            .get(batch.to_vec(), variables.clone(), porto_timeout(deadline)?)
            .map_err(PortoCollectError::Api)?;
        skipped = skipped.saturating_add(batch.len().saturating_sub(response.len()));
        snapshots.extend(response);
    }

    if options.name.is_none() {
        let response = client
            .get(
                vec!["self".into()],
                variables.clone(),
                porto_timeout(deadline)?,
            )
            .map_err(PortoCollectError::Api)?;
        skipped = skipped.saturating_add(1_usize.saturating_sub(response.len()));
        snapshots.extend(response);
    }

    let mut containers = Vec::with_capacity(snapshots.len());
    let mut property_errors = 0;
    let mut stream_truncated = false;
    for snapshot in snapshots {
        if snapshot.name.is_empty() || snapshot.name.len() > MAX_CONTAINER_NAME_BYTES {
            skipped = skipped.saturating_add(1);
            continue;
        }
        if let Some(error) = porto_snapshot_error(&snapshot) {
            if options.name.is_some() {
                return Err(PortoCollectError::Api(PortoError::Response(error)));
            }
            let is_self = snapshot.name == "self";
            containers.push(errored_porto_observation(snapshot.name, is_self, error));
            property_errors += 1;
            continue;
        }
        let is_self = snapshot.name == "self";
        let (container, errors, streams_truncated) =
            porto_observation(snapshot, &catalog, options, is_self);
        containers.push(container);
        property_errors += errors;
        stream_truncated |= streams_truncated;
    }

    Ok(PortoObservation {
        catalog,
        containers,
        property_errors,
        truncated: skipped > 0 || catalog_truncated || stream_truncated,
        item_limit_hit,
        stream_truncated,
        catalog_truncated,
        skipped,
    })
}

fn porto_timeout(deadline: ExecutionContext<'_>) -> Result<Duration, PortoCollectError> {
    let remaining = deadline.remaining().map_err(PortoCollectError::Control)?;
    Ok(remaining.min(PORTO_CALL_TIMEOUT))
}

fn porto_snapshot_error(snapshot: &ContainerSnapshot) -> Option<ResponseError> {
    let first = snapshot.keyval.first()?;
    let code = first.error.unwrap_or(porto::SUCCESS);
    if code == porto::SUCCESS
        || !snapshot
            .keyval
            .iter()
            .all(|value| value.error == Some(code))
    {
        return None;
    }

    Some(ResponseError {
        code,
        message: first.error_msg.clone().unwrap_or_default(),
    })
}

fn errored_porto_observation(
    name: String,
    is_self: bool,
    error: ResponseError,
) -> ObservedContainer {
    let summary = Map::from_iter([
        ("record_type".into(), json!("container")),
        ("id".into(), json!(name)),
        ("name".into(), json!(name)),
        ("runtime".into(), json!("porto")),
        ("is_self".into(), json!(is_self)),
    ]);
    let properties = vec![ObservedProperty {
        name: "$container".into(),
        value: None,
        error: Some(PropertyError {
            code: error.code,
            message: error.message,
        }),
        redacted: false,
        truncated: false,
    }];
    ObservedContainer {
        summary,
        properties,
    }
}

fn porto_observation(
    snapshot: ContainerSnapshot,
    catalog: &[PropertyMetadata],
    options: Options<'_>,
    is_self: bool,
) -> (ObservedContainer, usize, bool) {
    let mut summary = Map::new();
    summary.insert("record_type".into(), json!("container"));
    summary.insert("id".into(), json!(snapshot.name));
    summary.insert("name".into(), json!(snapshot.name));
    summary.insert("runtime".into(), json!("porto"));
    summary.insert("is_self".into(), json!(is_self));
    if let Some(change_time) = snapshot.change_time {
        summary.insert("change_time".into(), json!(change_time));
    }
    if let Some(no_changes) = snapshot.no_changes {
        summary.insert("no_changes".into(), json!(no_changes));
    }

    let mut values = BTreeMap::new();
    for mut value in snapshot.keyval {
        if is_stream_query(&value.variable) {
            value.variable.truncate(6);
        }
        if !value.variable.is_empty() && value.variable.len() <= MAX_PROPERTY_NAME_BYTES {
            values.insert(value.variable.clone(), value);
        }
    }

    let mut properties = Vec::with_capacity(catalog.len());
    let mut property_errors = 0;
    let mut stream_truncated = false;
    let mut normalized_truncated = BTreeSet::new();
    for metadata in catalog {
        if is_sensitive(&metadata.name) && !options.show_sensitive
            || is_stream(&metadata.name) && !options.include_streams
        {
            properties.push(ObservedProperty {
                name: metadata.name.clone(),
                value: None,
                error: None,
                redacted: true,
                truncated: false,
            });
            continue;
        }

        let Some(value) = values.remove(&metadata.name) else {
            property_errors += 1;
            properties.push(ObservedProperty {
                name: metadata.name.clone(),
                value: None,
                error: Some(PropertyError {
                    code: porto::UNKNOWN,
                    message: "Porto response omitted requested property".into(),
                }),
                redacted: false,
                truncated: false,
            });
            continue;
        };

        let error_code = value.error.unwrap_or(porto::SUCCESS);
        if error_code != porto::SUCCESS {
            property_errors += 1;
            properties.push(ObservedProperty {
                name: metadata.name.clone(),
                value: None,
                error: Some(PropertyError {
                    code: error_code,
                    message: value.error_msg.unwrap_or_default(),
                }),
                redacted: false,
                truncated: false,
            });
            continue;
        }

        let mut property_value = value.value.unwrap_or_default();
        let mut truncated = false;
        if is_stream(&metadata.name) && property_value.len() > options.max_stream_bytes {
            truncate_utf8(&mut property_value, options.max_stream_bytes);
            truncated = true;
            stream_truncated = true;
        }
        normalize_summary(
            &mut summary,
            &metadata.name,
            &property_value,
            &mut normalized_truncated,
        );
        properties.push(ObservedProperty {
            name: metadata.name.clone(),
            value: Some(property_value),
            error: None,
            redacted: false,
            truncated,
        });
    }

    if !normalized_truncated.is_empty() {
        summary.insert(
            "normalized_truncated".into(),
            json!(normalized_truncated.into_iter().collect::<Vec<_>>()),
        );
    }

    (
        ObservedContainer {
            summary,
            properties,
        },
        property_errors,
        stream_truncated,
    )
}

fn normalize_summary(
    summary: &mut Map<String, Value>,
    property: &str,
    value: &str,
    truncated: &mut BTreeSet<String>,
) {
    match property {
        "root_pid" => {
            if let Ok(pid) = value.parse::<u32>()
                && pid > 0
            {
                summary.insert("init_pid".into(), json!(pid));
            }
        }
        "capabilities_allowed" => {
            let mut capabilities = Vec::new();
            for item in value
                .split(|character: char| {
                    character == ';' || character == ',' || character.is_whitespace()
                })
                .filter(|item| !item.is_empty())
            {
                if capabilities.len() == 1024 {
                    truncated.insert(property.to_owned());
                    break;
                }
                let mut item = item.to_owned();
                if item.len() > 256 {
                    truncate_utf8(&mut item, 256);
                    truncated.insert(property.to_owned());
                }
                capabilities.push(item);
            }
            summary.insert(property.into(), json!(capabilities));
        }
        "state" | "root" | "command" | "user" | "group" | "parent" | "absolute_name"
        | "absolute_namespace" | "owner_user" | "owner_group" | "enable_porto" | "virt_mode" => {
            let mut value = value.to_owned();
            if value.len() > SUMMARY_VALUE_BYTES {
                truncate_utf8(&mut value, SUMMARY_VALUE_BYTES);
                truncated.insert(property.to_owned());
            }
            summary.insert(property.into(), json!(value));
        }
        _ => {}
    }
}

fn emit_catalog<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    catalog: &[PropertyMetadata],
    coverage: &mut Coverage,
) -> Result<bool, ProtocolError> {
    for property in catalog {
        let mut description = property.description.clone();
        let description_truncated = description.len() > PROPERTY_CHUNK_BYTES;
        if description_truncated {
            truncate_utf8(&mut description, PROPERTY_CHUNK_BYTES);
        }
        let data = json!({
            "record_type":"property_catalog",
            "property":property.name,
            "description":description,
            "description_truncated":description_truncated,
            "read_only":property.read_only,
            "dynamic":property.dynamic,
        });
        if !emit_record(
            sink,
            capability,
            "porto_property_catalog",
            "porto_api",
            data,
            coverage,
        )? {
            return Ok(false);
        }
    }

    Ok(true)
}

fn emit_container<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    provider: &'static str,
    container: &ObservedContainer,
    coverage: &mut Coverage,
) -> Result<bool, ProtocolError> {
    if !emit_record(
        sink,
        capability,
        capability.data_kind(),
        provider,
        Value::Object(container.summary.clone()),
        coverage,
    )? {
        return Ok(false);
    }

    let container_id = container
        .summary
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for property in &container.properties {
        if !emit_property(sink, capability, provider, container_id, property, coverage)? {
            return Ok(false);
        }
    }

    Ok(true)
}

fn emit_property<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    provider: &'static str,
    container_id: &str,
    property: &ObservedProperty,
    coverage: &mut Coverage,
) -> Result<bool, ProtocolError> {
    if property.redacted {
        return emit_record(
            sink,
            capability,
            "porto_property",
            provider,
            json!({
                "record_type":"property",
                "container_id":container_id,
                "property":property.name,
                "redacted":true,
            }),
            coverage,
        );
    }

    if let Some(error) = &property.error {
        return emit_record(
            sink,
            capability,
            "porto_property",
            provider,
            json!({
                "record_type":"property",
                "container_id":container_id,
                "property":property.name,
                "error":{
                    "code":error.code,
                    "name":porto::error_name(error.code),
                    "message":bounded_string(&error.message, 4096),
                },
            }),
            coverage,
        );
    }

    let value = property.value.as_deref().unwrap_or_default();
    if value.is_empty() {
        return emit_record(
            sink,
            capability,
            "porto_property",
            provider,
            json!({
                "record_type":"property",
                "container_id":container_id,
                "property":property.name,
                "offset":0,
                "bytes":0,
                "value":"",
                "eof":true,
                "truncated":property.truncated,
            }),
            coverage,
        );
    }

    let mut offset = 0;
    while offset < value.len() {
        let end = chunk_end(value, offset, PROPERTY_CHUNK_BYTES);
        let data = json!({
            "record_type":"property",
            "container_id":container_id,
            "property":property.name,
            "offset":offset,
            "bytes":end - offset,
            "value":&value[offset..end],
            "eof":end == value.len(),
            "truncated":property.truncated && end == value.len(),
        });
        if !emit_record(sink, capability, "porto_property", provider, data, coverage)? {
            return Ok(false);
        }
        offset = end;
    }

    Ok(true)
}

fn emit_record<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    data_kind: &'static str,
    provider: &'static str,
    data: Value,
    coverage: &mut Coverage,
) -> Result<bool, ProtocolError> {
    match sink.data(capability.id(), data_kind, provider, data) {
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

fn discover_native<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    max_items: usize,
    deadline: ExecutionContext<'_>,
    coverage: &mut Coverage,
) -> Result<(Vec<NativeContainer>, bool, bool), ProtocolError> {
    if max_items == 0 {
        return Ok((Vec::new(), false, true));
    }

    let entries = match sorted_entries(Path::new("/proc")) {
        Ok(entries) => entries,
        Err(error) => {
            super::source_failure(sink, capability, Path::new("/proc"), &error, coverage)?;
            return Ok((Vec::new(), true, false));
        }
    };
    let mut containers = BTreeMap::<(String, String), NativeContainer>::new();
    let mut partial = false;
    let mut limit_hit = false;

    for process in entries {
        check_deadline(deadline)?;
        let Some(pid) = process
            .file_name()
            .and_then(|name| std::str::from_utf8(name.as_bytes()).ok())
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };

        coverage.scanned += 1;
        let source = process.join("cgroup");
        let (bytes, _) = match read_bounded(&source, 64 << 10) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                coverage.denied += 1;
                partial = true;
                continue;
            }
            Err(_) => {
                coverage.vanished += 1;
                continue;
            }
        };

        let Some((runtime, id, cgroup)) = detect_container(&bytes) else {
            continue;
        };

        let key = (runtime.to_owned(), id.clone());
        if let Some(current) = containers.get(&key)
            && current.init_pid <= pid
        {
            continue;
        }

        let mut container = NativeContainer {
            id,
            runtime,
            init_pid: pid,
            cgroup,
            source,
            root: None,
            namespaces: BTreeMap::new(),
        };
        enrich_native(&mut container);
        containers.insert(key, container);

        if containers.len() >= max_items {
            limit_hit = true;
            break;
        }
    }

    Ok((containers.into_values().collect(), partial, limit_hit))
}

fn native_observation(container: NativeContainer, is_self: bool) -> ObservedContainer {
    let mut summary = Map::new();
    summary.insert("record_type".into(), json!("container"));
    summary.insert("id".into(), json!(container.id));
    summary.insert("name".into(), json!(container.id));
    summary.insert("runtime".into(), json!(container.runtime));
    summary.insert("state".into(), json!("running"));
    summary.insert("init_pid".into(), json!(container.init_pid));
    summary.insert("cgroup".into(), text(&container.cgroup));
    summary.insert("source".into(), path_value(&container.source));
    summary.insert("is_self".into(), json!(is_self));
    if let Some(root) = container.root {
        summary.insert("root".into(), path_value(&root));
    }
    if !container.namespaces.is_empty() {
        summary.insert(
            "namespaces".into(),
            Value::Object(
                container
                    .namespaces
                    .into_iter()
                    .map(|(name, path)| (name, path_value(&path)))
                    .collect(),
            ),
        );
    }

    ObservedContainer {
        summary,
        properties: Vec::new(),
    }
}

fn inspect_self(
    deadline: ExecutionContext<'_>,
    coverage: &mut Coverage,
) -> Result<ObservedContainer, ProtocolError> {
    let path = Path::new("/proc/self/cgroup");
    let (cgroup, truncated) = read_bounded(path, 64 << 10)
        .map_err(|error| ProtocolError::Internal(format!("read {}: {error}", path.display())))?;
    coverage.scanned += cgroup.len() as u64;
    coverage.truncated |= truncated;

    let detected = detect_container(&cgroup);
    let mut summary = Map::new();
    summary.insert("record_type".into(), json!("container"));
    summary.insert(
        "id".into(),
        json!(detected.as_ref().map_or("self", |(_, id, _)| id.as_str())),
    );
    summary.insert("name".into(), json!("self"));
    summary.insert("is_self".into(), json!(true));
    summary.insert("scope".into(), json!("self"));
    summary.insert(
        "docker_marker".into(),
        json!(Path::new("/.dockerenv").exists()),
    );
    summary.insert(
        "podman_marker".into(),
        json!(Path::new("/run/.containerenv").exists()),
    );
    summary.insert("cgroup".into(), text(&cgroup));
    summary.insert("source".into(), path_value(path));
    summary.insert(
        "runtime".into(),
        json!(
            detected
                .as_ref()
                .map_or("unknown", |(runtime, _, _)| *runtime)
        ),
    );

    let mut namespaces = Map::new();
    for name in super::NAMESPACES {
        check_deadline(deadline)?;
        let path = PathBuf::from(format!("/proc/self/ns/{name}"));
        match fs::read_link(&path) {
            Ok(target) => {
                coverage.scanned += 1;
                namespaces.insert(name.into(), path_value(&target));
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                coverage.denied += 1;
            }
            Err(_) => coverage.vanished += 1,
        }
    }
    summary.insert("namespaces".into(), Value::Object(namespaces));

    Ok(ObservedContainer {
        summary,
        properties: Vec::new(),
    })
}

fn enrich_native(container: &mut NativeContainer) {
    let base = PathBuf::from("/proc").join(container.init_pid.to_string());
    container.root = fs::read_link(base.join("root")).ok();
    for name in super::NAMESPACES {
        if let Ok(target) = fs::read_link(base.join("ns").join(name)) {
            container.namespaces.insert(name.into(), target);
        }
    }
}

fn detect_container(bytes: &[u8]) -> Option<(&'static str, String, Vec<u8>)> {
    for line in bytes.split(|byte| *byte == b'\n') {
        let mut fields = line.splitn(3, |byte| *byte == b':');
        let (Some(_hierarchy), Some(_controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let segments = path.split(|byte| *byte == b'/').collect::<Vec<_>>();
        for (index, segment) in segments.iter().enumerate().rev() {
            for (prefix, suffix, runtime) in [
                (b"docker-".as_slice(), b".scope".as_slice(), "docker"),
                (b"libpod-".as_slice(), b".scope".as_slice(), "podman"),
                (
                    b"cri-containerd-".as_slice(),
                    b".scope".as_slice(),
                    "containerd",
                ),
                (
                    b"machine-".as_slice(),
                    b".scope".as_slice(),
                    "systemd-nspawn",
                ),
            ] {
                if let Some(id) = segment
                    .strip_prefix(prefix)
                    .and_then(|value| value.strip_suffix(suffix))
                    && let Ok(id) = std::str::from_utf8(id)
                {
                    return Some((runtime, id.to_owned(), path.to_vec()));
                }
            }
            if **segment == *b"lxc"
                && let Some(id) = segments.get(index + 1)
                && let Ok(id) = std::str::from_utf8(id)
            {
                return Some(("lxc", id.to_owned(), path.to_vec()));
            }
            if **segment == *b"docker"
                && let Some(id) = segments.get(index + 1)
                && let Ok(id) = std::str::from_utf8(id)
            {
                return Some(("docker", id.to_owned(), path.to_vec()));
            }
            if path
                .windows(b"kubepods".len())
                .any(|window| window == b"kubepods")
                && let Ok(id) = std::str::from_utf8(segment)
                && is_container_id(id.trim_end_matches(".scope"))
            {
                return Some((
                    "kubernetes",
                    id.trim_end_matches(".scope").to_owned(),
                    path.to_vec(),
                ));
            }
        }
    }

    None
}

fn is_container_id(value: &str) -> bool {
    value.len() >= 12
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn emit_provider_selected<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    provider: &'static str,
) -> Result<(), ProtocolError> {
    sink.diagnostic(
        capability.id(),
        "provider",
        "selected",
        "provider_selected",
        json!({"display":provider}),
        "provider completed acquisition",
        1,
    )
}

fn emit_property_errors<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    socket: &Path,
    count: usize,
) -> Result<(), ProtocolError> {
    sink.diagnostic(
        capability.id(),
        "coverage",
        "partial",
        "property_errors",
        path_value(socket),
        "one or more Porto properties were unavailable",
        count as u64,
    )
}

fn porto_failure<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    socket: &Path,
    error: &PortoError,
    coverage: &mut Coverage,
) -> Result<bool, ProtocolError> {
    let (status, code, partial) = match error {
        PortoError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            coverage.vanished += 1;
            ("not_found", "source_not_found", false)
        }
        PortoError::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => {
            coverage.denied += 1;
            ("permission_denied", "source_permission_denied", true)
        }
        PortoError::Response(response) if response.code == porto::CONTAINER_DOES_NOT_EXIST => {
            coverage.skipped += 1;
            ("not_found", "container_not_found", true)
        }
        _ => {
            coverage.skipped += 1;
            ("unavailable", "provider_unavailable", true)
        }
    };
    sink.diagnostic(
        capability.id(),
        "provider",
        status,
        code,
        path_value(socket),
        &bounded_string(&error.to_string(), 4096),
        1,
    )?;

    Ok(partial)
}

fn unavailable_container<W: Write>(
    sink: &mut RecordSink<'_, W>,
    capability: CapabilityId,
    name: &str,
    mut coverage: Coverage,
) -> Result<CapabilityReport, ProtocolError> {
    coverage.skipped += 1;
    sink.diagnostic(
        capability.id(),
        "selection",
        "not_found",
        "container_not_found",
        json!({"display":bounded_string(name, MAX_CONTAINER_NAME_BYTES)}),
        "container was not visible through the selected container provider",
        1,
    )?;

    Ok(CapabilityReport {
        outcome: Outcome::Unavailable,
        coverage,
        limits_hit: Vec::new(),
    })
}

fn is_sensitive(property: &str) -> bool {
    property == "env" || property.starts_with("env.")
}

fn is_stream(property: &str) -> bool {
    property == "stdout" || property == "stderr"
}

fn porto_query(property: &str, max_stream_bytes: usize) -> String {
    if is_stream(property) {
        format!("{property}[0:{}]", max_stream_bytes.saturating_add(1))
    } else {
        property.to_owned()
    }
}

fn bounded_string(value: &str, maximum: usize) -> String {
    let mut value = value.to_owned();
    truncate_utf8(&mut value, maximum);
    value
}

fn truncate_utf8(value: &mut String, maximum: usize) {
    if value.len() <= maximum {
        return;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn is_stream_query(property: &str) -> bool {
    (property.starts_with("stdout[") || property.starts_with("stderr[")) && property.ends_with(']')
}

fn chunk_end(value: &str, start: usize, maximum: usize) -> usize {
    let mut end = start.saturating_add(maximum).min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_additional_native_container_layouts() {
        assert_eq!(
            detect_container(b"0::/machine.slice/machine-demo.scope\n")
                .map(|(runtime, id, _)| (runtime, id)),
            Some(("systemd-nspawn", "demo".into()))
        );
        assert_eq!(
            detect_container(b"0::/lxc/example\n").map(|(runtime, id, _)| (runtime, id)),
            Some(("lxc", "example".into()))
        );
    }

    #[test]
    fn porto_properties_preserve_errors_redaction_and_normalized_values() {
        let catalog = vec![
            PropertyMetadata {
                name: "state".into(),
                description: String::new(),
                read_only: true,
                dynamic: false,
            },
            PropertyMetadata {
                name: "capabilities_allowed".into(),
                description: String::new(),
                read_only: true,
                dynamic: false,
            },
            PropertyMetadata {
                name: "env".into(),
                description: String::new(),
                read_only: false,
                dynamic: false,
            },
            PropertyMetadata {
                name: "denied".into(),
                description: String::new(),
                read_only: true,
                dynamic: false,
            },
        ];
        let snapshot = ContainerSnapshot {
            name: "self".into(),
            keyval: vec![
                porto::PropertyValue {
                    variable: "state".into(),
                    value: Some("running".into()),
                    ..Default::default()
                },
                porto::PropertyValue {
                    variable: "capabilities_allowed".into(),
                    value: Some("CHOWN;SETUID, NET_ADMIN".into()),
                    ..Default::default()
                },
                porto::PropertyValue {
                    variable: "denied".into(),
                    error: Some(11),
                    error_msg: Some("permission denied".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let options = Options {
            name: Some("self"),
            socket: Path::new(crate::cli::PORTO_SOCKET_PATH),
            max_items: 1,
            show_sensitive: false,
            include_streams: false,
            max_stream_bytes: 0,
        };

        let (container, errors, streams_truncated) =
            porto_observation(snapshot, &catalog, options, true);

        assert_eq!(errors, 1);
        assert!(!streams_truncated);
        assert_eq!(container.summary["state"], "running");
        assert_eq!(
            container.summary["capabilities_allowed"],
            json!(["CHOWN", "SETUID", "NET_ADMIN"])
        );
        assert!(
            container
                .properties
                .iter()
                .any(|property| property.name == "env" && property.redacted)
        );
        assert!(
            container
                .properties
                .iter()
                .any(|property| property.name == "denied" && property.error.is_some())
        );
    }

    #[test]
    fn uniform_container_errors_are_promoted_to_provider_errors() {
        let missing = porto::CONTAINER_DOES_NOT_EXIST;
        let snapshot = ContainerSnapshot {
            name: "missing".into(),
            keyval: ["state", "root"]
                .into_iter()
                .map(|variable| porto::PropertyValue {
                    variable: variable.into(),
                    error: Some(missing),
                    error_msg: Some("container missing not found".into()),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };

        let error = porto_snapshot_error(&snapshot).expect("uniform response error");
        assert_eq!(error.code, missing);
    }

    #[test]
    fn porto_stream_queries_fetch_one_detection_byte_and_truncate() {
        let catalog = vec![PropertyMetadata {
            name: "stdout".into(),
            description: String::new(),
            read_only: true,
            dynamic: false,
        }];
        let snapshot = ContainerSnapshot {
            name: "self".into(),
            keyval: vec![porto::PropertyValue {
                variable: "stdout[0:5]".into(),
                value: Some("abcde".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let options = Options {
            name: Some("self"),
            socket: Path::new(crate::cli::PORTO_SOCKET_PATH),
            max_items: 1,
            show_sensitive: false,
            include_streams: true,
            max_stream_bytes: 4,
        };

        assert_eq!(porto_query("stdout", 4), "stdout[0:5]");
        let (container, errors, streams_truncated) =
            porto_observation(snapshot, &catalog, options, true);
        let property = &container.properties[0];
        assert_eq!(errors, 0);
        assert!(streams_truncated);
        assert_eq!(property.value.as_deref(), Some("abcd"));
        assert!(property.truncated);
    }

    #[test]
    fn property_chunks_preserve_utf8_boundaries() {
        let value = format!("{}x", "é".repeat(PROPERTY_CHUNK_BYTES));
        let first = chunk_end(&value, 0, PROPERTY_CHUNK_BYTES);
        assert!(value.is_char_boundary(first));
        assert!(first <= PROPERTY_CHUNK_BYTES);
        assert_eq!(
            &value[first..],
            format!("{}x", "é".repeat(PROPERTY_CHUNK_BYTES / 2))
        );
    }
}
