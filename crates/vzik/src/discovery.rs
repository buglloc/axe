use std::io::{self, Write};

use serde_json::{Value, json};

use crate::cli::{CapabilityId, global_limit_specs};

const CAPABILITY_OUTCOMES: [&str; 4] = ["complete", "partial", "unavailable", "unsupported"];

pub fn write_capabilities(
    output: &mut impl Write,
    selected: Option<CapabilityId>,
) -> io::Result<()> {
    let document = selected.map_or_else(capability_index, capability_detail);
    write_document(output, &document)
}

fn capability_index() -> Value {
    let capabilities = CapabilityId::all()
        .map(|capability| {
            let baseline_position = capability.baseline_position();
            json!({
                "id": capability.id(),
                "command": capability.command(),
                "description": capability.description(),
                "safety_class": capability.safety_class(),
                "baseline": {
                    "included":baseline_position.is_some(),
                    "position":baseline_position,
                },
            })
        })
        .collect::<Vec<_>>();
    let limit_contract = |profile| {
        global_limit_specs(profile)
            .into_iter()
            .map(|limit| limit.contract())
            .collect::<Vec<_>>()
    };

    json!({
        "schema_version": 3,
        "outcome": "complete",
        "protocol": {
            "schema_version":3,
            "transport":"jsonl",
            "completion_terminal_record":"stream_end",
            "interruption_terminal_record":"stream_abort",
            "stream_outcomes":["complete", "degraded"],
            "capability_outcomes":CAPABILITY_OUTCOMES,
            "deadline_semantics":"cooperative_between_bounded_operations",
            "stdout":"protocol_only",
            "diagnostics":"stderr_jsonl",
        },
        "exit_status": {
            "complete":0,
            "degraded":3,
            "invalid_request_or_input":2,
            "protocol_or_internal":4,
            "output_write":5,
            "deadline":124,
            "signal":"128+signal",
        },
        "process_error": {
            "schema_version":3,
            "transport":"stderr_jsonl",
            "fields":["code", "operation", "retryable", "message", "details"],
        },
        "global_limits": {
            "baseline":limit_contract(true),
            "targeted":limit_contract(false),
        },
        "output_sensitivity":"potentially_sensitive",
        "capabilities": capabilities,
    })
}

fn capability_detail(capability: CapabilityId) -> Value {
    let baseline_position = capability.baseline_position();
    json!({
        "schema_version": 3,
        "outcome": "complete",
        "capability": {
            "id": capability.id(),
            "command": capability.command(),
            "description": capability.description(),
            "data_kinds": capability.data_kinds(),
            "safety_class": capability.safety_class(),
            "baseline": {
                "included":baseline_position.is_some(),
                "position":baseline_position,
            },
            "request":capability.request_contract(),
            "access":capability.access_contract(),
            "outcomes":CAPABILITY_OUTCOMES,
        },
    })
}

fn write_document(output: &mut impl Write, document: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, document).map_err(io::Error::other)?;
    output.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_index_is_compact_and_declares_process_contract() {
        let mut output = Vec::new();
        write_capabilities(&mut output, None).expect("capability index");
        let value: Value = serde_json::from_slice(&output).expect("JSON output");
        let porto = value["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .find(|capability| capability["id"] == "porto.list")
            .expect("Porto capability");

        assert_eq!(value["schema_version"], 3);
        assert_eq!(value["protocol"]["schema_version"], 3);
        assert_eq!(value["exit_status"]["degraded"], 3);
        assert_eq!(value["process_error"]["transport"], "stderr_jsonl");
        assert_eq!(porto["command"], json!(["portoctl", "list"]));
        assert!(porto.get("request").is_none());
    }

    #[test]
    fn capability_detail_exposes_request_access_and_data_kinds() {
        let mut output = Vec::new();
        write_capabilities(&mut output, Some(CapabilityId::PortoList)).expect("capability detail");
        let value: Value = serde_json::from_slice(&output).expect("JSON output");
        let capability = &value["capability"];

        assert_eq!(capability["id"], "porto.list");
        assert_eq!(
            capability["data_kinds"],
            json!([
                "porto_container",
                "porto_property_catalog",
                "porto_property"
            ])
        );
        assert_eq!(capability["access"]["may_open_unix_socket"], true);
        assert!(
            capability["request"]["options"]
                .as_array()
                .expect("options")
                .iter()
                .any(|option| option["name"] == "max-stream-bytes"
                    && option["requires"] == json!(["include-streams"]))
        );
    }
}
