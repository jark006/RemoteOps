//! MCP presentation policy. Remote requests and lifecycle decisions always use
//! the complete wire result; projection only happens at the MCP boundary.

use serde_json::{Value, json};

use crate::client::ClientError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResponseMode {
    #[default]
    Compact,
    Full,
}

impl ResponseMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "compact" => Ok(Self::Compact),
            "full" => Ok(Self::Full),
            _ => Err("response_mode must be compact or full".to_string()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Full => "full",
        }
    }

    /// Consume the proxy-only option before decoding or forwarding arguments.
    pub(crate) fn take(arguments: &mut Value) -> Result<Option<Self>, ClientError> {
        let Some(value) = arguments
            .as_object_mut()
            .and_then(|args| args.remove("response_mode"))
        else {
            return Ok(None);
        };
        value
            .as_str()
            .ok_or_else(|| "response_mode must be compact or full".to_string())
            .and_then(Self::parse)
            .map(Some)
            .map_err(|message| ClientError::local("invalid_params", message))
    }
}

pub(crate) fn mode_schema(default: ResponseMode) -> Value {
    json!({
        "type":"string", "enum":["compact","full"], "default":default.name(),
        "description":"Proxy detail: compact reports omitted diagnostic fields in _response; full keeps all fields. Batch children inherit."
    })
}

pub(crate) fn failed_result(error: &ClientError) -> Value {
    let details = error.details();
    json!({"content":[{"type":"text","text":serde_json::to_string(&json!({"error":details})).expect("serializable error")}],"isError":true})
}

// A deliberately small allowlist: never generically remove null/false/empty
// values, since those can carry essential outcome and continuation information.
fn omitted_paths(name: &str) -> &'static [&'static str] {
    match name {
        "system_info" => &["/network", "/filesystems"],
        "agent_info" => &["/build", "/supported_operations", "/limits", "/update"],
        "remote_probe" => &[
            "/agent_info/build",
            "/agent_info/supported_operations",
            "/agent_info/limits",
            "/agent_info/update",
        ],
        "remote_status" | "set_remote" => &[
            "/agent_info/build",
            "/agent_info/supported_operations",
            "/agent_info/limits",
            "/agent_info/update",
            "/last_probe/agent_info/build",
            "/last_probe/agent_info/supported_operations",
            "/last_probe/agent_info/limits",
            "/last_probe/agent_info/update",
        ],
        _ => &[],
    }
}

pub(crate) fn project(name: &str, mut value: Value, mode: ResponseMode) -> Value {
    if mode == ResponseMode::Full {
        return value;
    }
    let mut omitted = Vec::new();
    for path in omitted_paths(name) {
        let (parent, key) = path.rsplit_once('/').expect("static JSON pointer");
        if value
            .pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .and_then(|object| object.remove(key))
            .is_some()
        {
            omitted.push(*path);
        }
    }
    if !omitted.is_empty() {
        value["_response"] = json!({"mode":"compact", "omitted_fields":omitted});
    }
    value
}

#[cfg(test)]
fn successful_result(name: &str, value: Value, mode: ResponseMode) -> Value {
    render_result(name, value, mode).expect("test result fits the response budget")
}

pub(crate) fn render_result(
    name: &str,
    value: Value,
    mode: ResponseMode,
) -> Result<Value, ClientError> {
    if matches!(name, "read_text" | "read_file_lines" | "tail_text") {
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let metadata = value.get("metadata").cloned().unwrap_or_else(|| json!({}));
        return Ok(
            json!({"content":[{"type":"text","text":text}], "structuredContent":metadata, "isError":false}),
        );
    }
    let value = project(name, value, mode);
    let text = match mode {
        ResponseMode::Compact => remote_ops_protocol::encoding::to_vec_bounded(&value,crate::stdio::MAX_MCP_RESPONSE_BYTES/2),
        ResponseMode::Full => remote_ops_protocol::encoding::to_vec_pretty_bounded(&value,crate::stdio::MAX_MCP_RESPONSE_BYTES/2),
    }
    .map_err(|error|ClientError::response_limited(format!("tool finished but its formatted result is unavailable: {error}; reduce the requested output; do not replay a command based on this error")))?;
    let text = String::from_utf8(text).expect("JSON is UTF-8");
    Ok(json!({"content":[{"type":"text","text":text}], "structuredContent":value, "isError":false}))
}

/// Keep one output schema valid for both modes, including the explicit omission
/// report. Existing fields retain their types and their full-mode requirements.
pub(crate) fn output_schema(name: &str, mut schema: Value) -> Value {
    let paths = omitted_paths(name);
    if paths.is_empty() {
        return schema;
    }
    let full_required = schema.get("required").cloned();
    for path in paths {
        let mut node = &mut schema;
        let mut parts = path.trim_start_matches('/').split('/').peekable();
        while let Some(key) = parts.next() {
            if parts.peek().is_none() {
                if let Some(required) = node.get_mut("required").and_then(Value::as_array_mut) {
                    required.retain(|field| field.as_str() != Some(key));
                }
                break;
            }
            let Some(child) = node
                .get_mut("properties")
                .and_then(|properties| properties.get_mut(key))
            else {
                break;
            };
            node = child;
        }
    }
    schema["properties"]["_response"] = json!({
        "type":"object", "properties":{
            "mode":{"type":"string","const":"compact"},
            "omitted_fields":{"type":"array","minItems":1,"maxItems":paths.len(),"uniqueItems":true,"items":{"type":"string","enum":paths}}
        },
        "required":["mode","omitted_fields"], "additionalProperties":false
    });
    if let Some(required) = full_required {
        schema["anyOf"] = json!([{"required":required},{"required":["_response"]}]);
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_diagnostics_are_explicit_and_full_is_lossless() {
        let original = json!({
            "hostname":"router", "init_system":{"name":"busybox"},
            "toolchains":{"available":true,"truncated":false,"items":[]},
            "network":{"interfaces":{"items":[{"name":"eth0","addresses":["192.0.2.1"]}]},"routes":{"items":vec!["route";256]}},
            "filesystems":{"available":true,"mounts":vec!["mount";256]}
        });
        let compact = successful_result("system_info", original.clone(), ResponseMode::Compact);
        let data = &compact["structuredContent"];
        assert_eq!(data["hostname"], "router");
        assert_eq!(data["toolchains"], original["toolchains"]);
        assert_eq!(
            data["_response"]["omitted_fields"],
            json!(["/network", "/filesystems"])
        );
        assert!(data.get("network").is_none());
        let full = successful_result("system_info", original.clone(), ResponseMode::Full);
        assert_eq!(full["structuredContent"], original);
        for result in [&compact, &full] {
            let fallback: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(fallback, result["structuredContent"]);
        }
        assert!(compact.to_string().len() * 4 < full.to_string().len());
    }

    #[test]
    fn command_outcomes_cursors_and_text_are_never_summarized_away() {
        for (name, data) in [
            (
                "exec",
                json!({"stdout":"result\n", "stderr":"failure\n", "exit_code":1,"timed_out":false,"duration_ms":7,"stdout_truncated":true,"stderr_truncated":false}),
            ),
            (
                "list_files",
                json!({"entries":[],"next_cursor":"continue","truncated":true}),
            ),
            (
                "process_output",
                json!({"stdout":"part","stdout_start_cursor":12,"stdout_next_cursor":16,"stdout_truncated":true,"state":"running"}),
            ),
            (
                "deploy_release",
                json!({"status":"rollback_failed","backup_path":"/backup","steps":[{"stderr":"failed"}]}),
            ),
        ] {
            assert_eq!(
                successful_result(name, data.clone(), ResponseMode::Compact)["structuredContent"],
                data
            );
        }
        for mode in [ResponseMode::Compact, ResponseMode::Full] {
            for name in ["read_text", "read_file_lines", "tail_text"] {
                let text = "\u{feff}line\r\n末尾";
                let metadata = json!({"truncated":true,"next_offset":42});
                let result =
                    successful_result(name, json!({"text":text,"metadata":metadata}), mode);
                assert_eq!(result["content"][0]["text"], text);
                assert_eq!(result["structuredContent"], metadata);
            }
        }
    }

    #[test]
    fn cached_probe_errors_and_instance_identity_survive_projection() {
        let info = json!({"runtime":{"instance_id":"unique"},"platform":{"os":"linux"},"build":{"target":"mips"},"limits":{},"supported_operations":[],"update":{}});
        let original = json!({"agent_info":info,"last_probe":{"reachable":false,"error":{"kind":"io","message":"offline"},"agent_info":info},"last_error":{"kind":"io","message":"offline"}});
        let result = project("remote_status", original.clone(), ResponseMode::Compact);
        assert_eq!(result["last_error"], original["last_error"]);
        assert_eq!(
            result["last_probe"]["error"],
            original["last_probe"]["error"]
        );
        assert_eq!(result["agent_info"]["runtime"]["instance_id"], "unique");
        assert_eq!(
            result["_response"]["omitted_fields"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert_eq!(
            project(
                "remote_status",
                json!({"agent_info":null,"last_probe":null}),
                ResponseMode::Compact
            ),
            json!({"agent_info":null,"last_probe":null})
        );
    }
}
