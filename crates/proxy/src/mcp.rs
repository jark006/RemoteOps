use remote_ops_protocol::PROTOCOL_VERSION as REMOTE_PROTOCOL_VERSION;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::net::Ipv4Addr;

use crate::client::{ClientError, RemoteClient};

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

pub fn handle_message(message: Value, client: &mut RemoteClient) -> Option<Value> {
    let id = message.get("id").cloned();
    if message.get("jsonrpc") != Some(&Value::String("2.0".to_string())) {
        return id.map(|id| rpc_error(id, -32600, "invalid JSON-RPC request"));
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return id.map(|id| rpc_error(id, -32600, "method must be a string"));
    };
    let id = id?;
    match method {
        "initialize" => initialize(id, message.get("params")),
        "ping" => Some(rpc_result(id, json!({}))),
        "tools/list" => Some(rpc_result(id, json!({"tools": tool_definitions()}))),
        "tools/call" => Some(call_tool(id, message.get("params"), client)),
        _ => Some(rpc_error(id, -32601, "method not found")),
    }
}

fn initialize(id: Value, params: Option<&Value>) -> Option<Value> {
    let valid = params.and_then(Value::as_object).is_some_and(|params| {
        params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .is_some()
            && params
                .get("capabilities")
                .and_then(Value::as_object)
                .is_some()
            && params
                .get("clientInfo")
                .and_then(Value::as_object)
                .is_some_and(|info| {
                    info.get("name").and_then(Value::as_str).is_some()
                        && info.get("version").and_then(Value::as_str).is_some()
                })
    });
    if !valid {
        return Some(rpc_error(id, -32602, "invalid initialize params"));
    }
    Some(rpc_result(
        id,
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "remote-ops-proxy", "version": env!("CARGO_PKG_VERSION")},
            "instructions": format!("RemoteOps proxy using remote protocol v{REMOTE_PROTOCOL_VERSION}")
        }),
    ))
}

fn call_tool(id: Value, params: Option<&Value>, client: &mut RemoteClient) -> Value {
    let Some(params) = params.and_then(Value::as_object) else {
        return rpc_error(id, -32602, "tools/call params must be an object");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "tool name must be a string");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return rpc_error(id, -32602, "tool arguments must be an object");
    }
    if !tool_names().contains(&name) {
        return rpc_error(id, -32602, "unknown tool");
    }

    let result = match name {
        "upload_file" => decode_transfer::<UploadArgs>(&arguments)
            .and_then(|args| client.upload(&args.local_path, &args.remote_path, args.overwrite)),
        "download_file" => decode_transfer::<DownloadArgs>(&arguments)
            .and_then(|args| client.download(&args.remote_path, &args.local_path, args.overwrite)),
        "remote_status" => decode_transfer::<EmptyArgs>(&arguments).map(|_| client.remote_status()),
        "set_remote" => decode_transfer::<SetRemoteArgs>(&arguments)
            .and_then(|args| client.set_remote(args.ip, args.port)),
        _ => client.call(name, arguments),
    };
    match result {
        Ok(value) => rpc_result(id, successful_tool_result(name, value)),
        Err(error) if error.kind == "invalid_params" => rpc_error(id, -32602, &error.message),
        Err(error) => rpc_result(
            id,
            json!({
                "content": [{"type": "text", "text": format!("{}: {}", error.kind, error.message)}],
                "isError": true
            }),
        ),
    }
}

fn successful_tool_result(name: &str, value: Value) -> Value {
    if matches!(name, "read_text" | "tail_text") {
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let metadata = value.get("metadata").cloned().unwrap_or_else(|| json!({}));
        json!({"content": [{"type": "text", "text": text}], "structuredContent": metadata, "isError": false})
    } else {
        let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
        json!({"content": [{"type": "text", "text": text}], "structuredContent": value, "isError": false})
    }
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn decode_transfer<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T, ClientError> {
    serde_json::from_value(value.clone()).map_err(|error| ClientError {
        kind: "invalid_params".to_string(),
        message: error.to_string(),
    })
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadArgs {
    local_path: String,
    remote_path: String,
    #[serde(default = "default_true")]
    overwrite: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadArgs {
    remote_path: String,
    local_path: String,
    #[serde(default = "default_true")]
    overwrite: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetRemoteArgs {
    ip: Option<Ipv4Addr>,
    port: Option<u16>,
}

pub fn tool_names() -> &'static [&'static str] {
    &[
        "read_text",
        "tail_text",
        "write_text",
        "ls",
        "stat",
        "file_hash",
        "pids",
        "process_info",
        "kill",
        "sh_exec",
        "exec",
        "system_info",
        "upload_file",
        "download_file",
        "remote_status",
        "set_remote",
    ]
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "read_text",
            "Read text",
            "Read bounded text from a remote file",
            props(&[
                ("path", string_prop("Remote file path")),
                ("offset", integer_prop_default(0, None, 0, "Byte offset")),
                (
                    "max_bytes",
                    integer_prop_default(0, Some(1_048_576), 1_048_576, "Maximum bytes"),
                ),
            ]),
            &["path"],
            (true, false, true),
        ),
        tool(
            "tail_text",
            "Tail text",
            "Read the last lines of a remote text file",
            props(&[
                ("path", string_prop("Remote file path")),
                (
                    "lines",
                    integer_prop_default(0, Some(10_000), 100, "Number of lines"),
                ),
                (
                    "max_bytes",
                    integer_prop_default(0, Some(1_048_576), 262_144, "Maximum scanned bytes"),
                ),
            ]),
            &["path"],
            (true, false, true),
        ),
        tool(
            "write_text",
            "Write text",
            "Atomically write a remote text file",
            props(&[
                ("path", string_prop("Remote file path")),
                ("content", string_prop("Text content")),
            ]),
            &["path", "content"],
            (false, true, true),
        ),
        tool(
            "ls",
            "List directory",
            "List a remote directory with pagination",
            props(&[
                ("path", string_prop("Remote directory path")),
                ("cursor", string_prop("Exclusive name cursor")),
                (
                    "limit",
                    integer_prop_default(1, Some(1000), 200, "Maximum entries"),
                ),
            ]),
            &["path"],
            (true, false, true),
        ),
        tool(
            "stat",
            "File status",
            "Read remote path metadata without following symlinks",
            props(&[("path", string_prop("Remote path"))]),
            &["path"],
            (true, false, true),
        ),
        tool(
            "file_hash",
            "File hash",
            "Calculate a bounded SHA-256 digest on the remote",
            props(&[
                ("path", string_prop("Remote file path")),
                (
                    "max_bytes",
                    integer_prop_default(0, Some(67_108_864), 67_108_864, "Maximum file bytes"),
                ),
            ]),
            &["path"],
            (true, false, true),
        ),
        tool(
            "pids",
            "List processes",
            "List Linux, Windows, or macOS processes with pagination",
            props(&[
                ("filter", string_prop("Name or command substring")),
                ("cursor", string_prop("Exclusive PID cursor")),
                (
                    "limit",
                    integer_prop_default(1, Some(1024), 1024, "Maximum processes"),
                ),
            ]),
            &[],
            (true, false, true),
        ),
        tool(
            "process_info",
            "Process information",
            "Read bounded Linux, Windows, or macOS process details",
            props(&[("pid", integer_prop(1, None, "Process ID"))]),
            &["pid"],
            (true, false, true),
        ),
        tool(
            "kill",
            "Terminate process",
            "Send a Unix signal or terminate a Windows process",
            props(&[
                ("pid", integer_prop(1, None, "Process ID")),
                (
                    "signal",
                    integer_prop_default(
                        1,
                        Some(64),
                        15,
                        "Unix signal number; Windows accepts 9 or 15",
                    ),
                ),
            ]),
            &["pid"],
            (false, true, false),
        ),
        tool(
            "sh_exec",
            "Run shell command",
            "Run a bounded command through /bin/sh or fixed-path Git Bash on Windows",
            props(&[
                ("command", string_prop("Shell command")),
                (
                    "timeout_ms",
                    integer_prop_default(0, Some(300_000), 10_000, "Timeout in milliseconds"),
                ),
            ]),
            &["command"],
            (false, true, false),
        ),
        tool(
            "exec",
            "Run program",
            "Run a remote program without a shell",
            props(&[
                ("program", string_prop("Program path or name")),
                ("args", json!({"type":"array","items":{"type":"string"}})),
                ("cwd", string_prop("Working directory")),
                (
                    "env",
                    json!({"type":"object","additionalProperties":{"type":"string"}}),
                ),
                (
                    "timeout_ms",
                    integer_prop_default(0, Some(300_000), 10_000, "Timeout in milliseconds"),
                ),
            ]),
            &["program"],
            (false, true, false),
        ),
        tool(
            "system_info",
            "System information",
            "Read bounded system information",
            props(&[]),
            &[],
            (true, false, true),
        ),
        tool(
            "upload_file",
            "Upload file",
            "Transfer one binary file from the proxy PC to the remote",
            props(&[
                ("local_path", string_prop("Path on the proxy PC")),
                ("remote_path", string_prop("Destination path on the remote")),
                ("overwrite", json!({"type":"boolean","default":true})),
            ]),
            &["local_path", "remote_path"],
            (false, true, true),
        ),
        tool(
            "download_file",
            "Download file",
            "Transfer one binary file from the remote to the proxy PC",
            props(&[
                ("remote_path", string_prop("Path on the remote")),
                (
                    "local_path",
                    string_prop("Destination path on the proxy PC"),
                ),
                ("overwrite", json!({"type":"boolean","default":true})),
            ]),
            &["remote_path", "local_path"],
            (false, true, true),
        ),
        tool(
            "remote_status",
            "Remote status",
            "Read the configured remote and cached connection state without connecting",
            props(&[]),
            &[],
            (true, false, true),
        ),
        tool(
            "set_remote",
            "Set remote",
            "Change the remote IPv4 address or port for subsequent calls",
            props(&[
                (
                    "ip",
                    json!({"type":"string","format":"ipv4","description":"Remote IPv4 address"}),
                ),
                ("port", integer_prop(1, Some(65_535), "Remote TCP port")),
            ]),
            &[],
            (false, true, true),
        ),
    ]
}

fn tool(
    name: &str,
    title: &str,
    description: &str,
    properties: Map<String, Value>,
    required: &[&str],
    annotations: (bool, bool, bool),
) -> Value {
    let (read_only, destructive, idempotent) = annotations;
    let mut input_schema = json!({
        "type":"object",
        "properties":properties,
        "required":required,
        "additionalProperties":false
    });
    if name == "set_remote" {
        input_schema["anyOf"] = json!([{"required":["ip"]}, {"required":["port"]}]);
    }
    json!({
        "name": name, "title": title, "description": description,
        "inputSchema": input_schema,
        "outputSchema": output_schema(name),
        "annotations": {"readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": idempotent, "openWorldHint": matches!(name, "sh_exec" | "exec")}
    })
}

fn props(values: &[(&'static str, Value)]) -> Map<String, Value> {
    values
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect()
}

fn string_prop(description: &str) -> Value {
    json!({"type":"string", "description": description})
}
fn integer_prop(minimum: u64, maximum: Option<u64>, description: &str) -> Value {
    let mut value = json!({"type":"integer", "minimum": minimum, "description": description});
    if let Some(maximum) = maximum {
        value["maximum"] = json!(maximum);
    }
    value
}

fn integer_prop_default(
    minimum: u64,
    maximum: Option<u64>,
    default: u64,
    description: &str,
) -> Value {
    let mut value = integer_prop(minimum, maximum, description);
    value["default"] = json!(default);
    value
}

fn output_schema(name: &str) -> Value {
    match name {
        "read_text" => strict_output(
            json!({
                "offset":{"type":"integer","minimum":0},
                "bytes_read":{"type":"integer","minimum":0},
                "next_offset":{"type":"integer","minimum":0},
                "truncated":{"type":"boolean"}
            }),
            &["offset", "bytes_read", "next_offset", "truncated"],
        ),
        "tail_text" => strict_output(
            json!({
                "bytes_scanned":{"type":"integer","minimum":0},
                "lines_returned":{"type":"integer","minimum":0},
                "truncated":{"type":"boolean"}
            }),
            &["bytes_scanned", "lines_returned", "truncated"],
        ),
        "write_text" => strict_output(
            json!({"bytes_written":{"type":"integer","minimum":0}}),
            &["bytes_written"],
        ),
        "ls" => strict_output(
            json!({
                "entries":{"type":"array","items":{
                    "type":"object",
                    "properties":{"name":{"type":"string"},"kind":{"type":"string","enum":["file","dir","symlink","other"]},"size":{"type":"integer","minimum":0}},
                    "required":["name","kind","size"],"additionalProperties":false
                }},
                "next_cursor":{"type":["string","null"]},
                "truncated":{"type":"boolean"}
            }),
            &["entries", "next_cursor", "truncated"],
        ),
        "stat" => strict_output(
            json!({
                "size":{"type":"integer","minimum":0},
                "mtime":{"type":"integer"},
                "mode":{"type":"integer","minimum":0},
                "kind":{"type":"string","enum":["file","dir","symlink","other"]}
            }),
            &["size", "mtime", "mode", "kind"],
        ),
        "file_hash" => strict_output(
            json!({
                "algorithm":{"type":"string","const":"sha256"},
                "digest":{"type":"string"},
                "bytes_hashed":{"type":"integer","minimum":0}
            }),
            &["algorithm", "digest", "bytes_hashed"],
        ),
        "pids" => strict_output(
            json!({
                "processes":{"type":"array","items":{
                    "type":"object","properties":{"pid":{"type":"integer","minimum":1},"name":{"type":"string"},"cmdline":{"type":"string"}},
                    "required":["pid","name","cmdline"],"additionalProperties":false
                }},
                "next_cursor":{"type":["string","null"]},
                "truncated":{"type":"boolean"}
            }),
            &["processes", "next_cursor", "truncated"],
        ),
        "process_info" => strict_output(
            json!({
                "pid":{"type":"integer","minimum":1},"ppid":{"type":"integer","minimum":0},
                "name":{"type":"string"},"state":{"type":["string","null"]},"cmdline":{"type":"string"},
                "uid":{"type":["integer","null"],"minimum":0},
                "resident_bytes":{"type":["integer","null"],"minimum":0},
                "virtual_bytes":{"type":["integer","null"],"minimum":0},
                "start_time_ticks":{"type":"integer","minimum":0},
                "start_time_seconds":{"type":"number","minimum":0}
            }),
            &[
                "pid",
                "ppid",
                "name",
                "state",
                "cmdline",
                "uid",
                "resident_bytes",
                "virtual_bytes",
                "start_time_ticks",
                "start_time_seconds",
            ],
        ),
        "kill" => strict_output(
            json!({"pid":{"type":"integer","minimum":1},"signal":{"type":"integer","minimum":1,"maximum":64}}),
            &["pid", "signal"],
        ),
        "sh_exec" | "exec" => strict_output(
            json!({
                "stdout":{"type":"string"},"stderr":{"type":"string"},"exit_code":{"type":["integer","null"]},
                "timed_out":{"type":"boolean"},"stdout_truncated":{"type":"boolean"},"stderr_truncated":{"type":"boolean"}
            }),
            &[
                "stdout",
                "stderr",
                "exit_code",
                "timed_out",
                "stdout_truncated",
                "stderr_truncated",
            ],
        ),
        "system_info" => strict_output(
            json!({
                "hostname":{"type":"string"},
                "kernel":{"type":"object","properties":{"sysname":{"type":"string"},"release":{"type":"string"},"machine":{"type":"string"}},"required":["sysname","release","machine"],"additionalProperties":false},
                "uptime_seconds":{"type":"number","minimum":0},
                "load_average":{"type":"object","properties":{"one":{"type":"number"},"five":{"type":"number"},"fifteen":{"type":"number"}},"required":["one","five","fifteen"],"additionalProperties":false},
                "memory":{"type":"object","properties":{"total_bytes":{"type":"integer","minimum":0},"available_bytes":{"type":"integer","minimum":0}},"required":["total_bytes","available_bytes"],"additionalProperties":false},
                "root_filesystem":{"type":"object","properties":{"total_bytes":{"type":"integer","minimum":0},"available_bytes":{"type":"integer","minimum":0}},"required":["total_bytes","available_bytes"],"additionalProperties":false},
                "temperatures":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"celsius":{"type":"number"}},"required":["name","celsius"],"additionalProperties":false}}
            }),
            &[
                "hostname",
                "kernel",
                "uptime_seconds",
                "load_average",
                "memory",
                "root_filesystem",
                "temperatures",
            ],
        ),
        "upload_file" | "download_file" => strict_output(
            json!({
                "bytes_transferred":{"type":"integer","minimum":0},
                "sha256":{"type":"string"},"source":{"type":"string"},"destination":{"type":"string"}
            }),
            &["bytes_transferred", "sha256", "source", "destination"],
        ),
        "remote_status" | "set_remote" => strict_output(
            json!({
                "ip":{"type":"string","description":"Configured remote IPv4 address"},
                "port":{"type":"integer","minimum":1,"maximum":65535},
                "address":{"type":"string","description":"Configured remote IPv4:PORT"},
                "connected":{"type":"boolean","description":"Whether the proxy holds an authenticated session; no active probe is performed"}
            }),
            &["ip", "port", "address", "connected"],
        ),
        _ => json!({"type":"object"}),
    }
}

fn strict_output(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_compatible_tools_plus_transfers() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 16);
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            tool_names()
        );
        assert!(
            tools
                .iter()
                .all(|tool| tool["inputSchema"]["additionalProperties"] == false)
        );
        let system = tools
            .iter()
            .find(|tool| tool["name"] == "system_info")
            .unwrap();
        assert_eq!(system["description"], "Read bounded system information");
        assert_eq!(
            system["outputSchema"]["properties"]["hostname"]["type"],
            "string"
        );
        assert!(
            system["outputSchema"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "temperatures")
        );
        let processes = tools.iter().find(|tool| tool["name"] == "pids").unwrap();
        assert_eq!(
            processes["description"],
            "List Linux, Windows, or macOS processes with pagination"
        );
        let process_info = tools
            .iter()
            .find(|tool| tool["name"] == "process_info")
            .unwrap();
        assert_eq!(
            process_info["outputSchema"]["properties"]["state"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            process_info["outputSchema"]["properties"]["uid"]["type"],
            json!(["integer", "null"])
        );
        let kill = tools.iter().find(|tool| tool["name"] == "kill").unwrap();
        assert_eq!(
            kill["description"],
            "Send a Unix signal or terminate a Windows process"
        );
        assert_eq!(
            kill["inputSchema"]["properties"]["signal"]["description"],
            "Unix signal number; Windows accepts 9 or 15"
        );
        let shell = tools.iter().find(|tool| tool["name"] == "sh_exec").unwrap();
        assert_eq!(
            shell["description"],
            "Run a bounded command through /bin/sh or fixed-path Git Bash on Windows"
        );
        let status = tools
            .iter()
            .find(|tool| tool["name"] == "remote_status")
            .unwrap();
        assert_eq!(status["annotations"]["readOnlyHint"], true);
        let setter = tools
            .iter()
            .find(|tool| tool["name"] == "set_remote")
            .unwrap();
        assert_eq!(setter["inputSchema"]["properties"]["ip"]["format"], "ipv4");
        assert_eq!(setter["inputSchema"]["anyOf"].as_array().unwrap().len(), 2);
        assert_eq!(setter["annotations"]["destructiveHint"], true);
    }

    #[test]
    fn local_remote_tools_validate_and_update_without_connecting() {
        let mut client = RemoteClient::new(
            "127.0.0.1:8022".parse().unwrap(),
            std::time::Duration::from_millis(10),
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        );
        let status = call_tool(
            json!(1),
            Some(&json!({"name":"remote_status","arguments":{}})),
            &mut client,
        );
        assert_eq!(status["result"]["structuredContent"]["connected"], false);

        let updated = call_tool(
            json!(2),
            Some(&json!({"name":"set_remote","arguments":{"port":9000}})),
            &mut client,
        );
        assert_eq!(
            updated["result"]["structuredContent"]["address"],
            "127.0.0.1:9000"
        );
        let updated = call_tool(
            json!(3),
            Some(&json!({"name":"set_remote","arguments":{"ip":"127.0.0.2"}})),
            &mut client,
        );
        assert_eq!(
            updated["result"]["structuredContent"]["address"],
            "127.0.0.2:9000"
        );

        for arguments in [json!({}), json!({"ip":"localhost"}), json!({"port":0})] {
            let response = call_tool(
                json!(4),
                Some(&json!({"name":"set_remote","arguments":arguments})),
                &mut client,
            );
            assert_eq!(response["error"]["code"], -32602);
        }
    }
}
