use globset::Glob;
use regex::RegexBuilder;
use remote_ops_protocol::{
    APPLY_PATCH_MAX_HUNKS, APPLY_PATCH_MAX_PATCH_BYTES, PROTOCOL_VERSION as REMOTE_PROTOCOL_VERSION,
};
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
        "pkill" => decode_transfer::<PkillArgs>(&arguments)
            .and_then(validate_pkill_args)
            .and_then(|_| client.call(name, arguments)),
        "apply_patch" => decode_transfer::<ApplyPatchArgs>(&arguments)
            .and_then(validate_apply_patch_args)
            .and_then(|_| client.call(name, arguments)),
        "read_file_lines" => decode_transfer::<ReadFileLinesArgs>(&arguments)
            .and_then(validate_read_file_lines_args)
            .and_then(|_| client.call(name, arguments)),
        "grep" => decode_transfer::<GrepArgs>(&arguments)
            .and_then(validate_grep_args)
            .and_then(|_| client.call(name, arguments)),
        "list_files" => decode_transfer::<ListFilesArgs>(&arguments)
            .and_then(validate_list_files_args)
            .and_then(|_| client.call(name, arguments)),
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
    if matches!(name, "read_text" | "read_file_lines" | "tail_text") {
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

fn default_signal() -> i32 {
    15
}

const PKILL_MAX_NAME_CHARS: usize = 260;
const READ_FILE_LINES_MAX_BYTES: usize = 1024 * 1024;
const READ_FILE_LINES_MAX_LINES: u64 = 10_000;
const GREP_MAX_PATTERN_BYTES: usize = 4 * 1024;
const MAX_GLOB_BYTES: usize = 1024;
const GREP_MAX_RESULTS: usize = 1000;
const GREP_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const LIST_FILES_MAX_DEPTH: usize = 64;

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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PkillArgs {
    name: String,
    #[serde(default = "default_signal")]
    signal: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArgs {
    path: String,
    patch: String,
    expected_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileLinesArgs {
    path: String,
    #[serde(default = "default_start_line")]
    start_line: u64,
    end_line: Option<u64>,
    #[serde(default = "default_line_bytes")]
    max_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepArgs {
    path: String,
    pattern: String,
    glob: Option<String>,
    #[serde(default = "default_true")]
    case_sensitive: bool,
    #[serde(default = "default_grep_results")]
    max_results: usize,
    #[serde(default = "default_grep_file_bytes")]
    max_file_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListFilesArgs {
    path: String,
    cursor: Option<String>,
    #[serde(default = "default_list_limit")]
    limit: usize,
    #[serde(default)]
    recursive: bool,
    pattern: Option<String>,
    #[serde(default = "default_list_depth")]
    max_depth: usize,
}

fn default_start_line() -> u64 {
    1
}

fn default_line_bytes() -> usize {
    256 * 1024
}

fn default_grep_results() -> usize {
    200
}

fn default_grep_file_bytes() -> u64 {
    1024 * 1024
}

fn default_list_limit() -> usize {
    200
}

fn default_list_depth() -> usize {
    16
}

fn validate_apply_patch_args(args: ApplyPatchArgs) -> Result<(), ClientError> {
    let message = if args.path.is_empty() {
        Some("path must not be empty")
    } else if args.path.contains('\0') {
        Some("path must not contain NUL")
    } else if args.patch.is_empty() {
        Some("patch must not be empty")
    } else if args.patch.len() > APPLY_PATCH_MAX_PATCH_BYTES {
        Some("patch exceeds 262144 bytes")
    } else if !valid_patch_envelope(&args.path, &args.patch) {
        Some("patch must contain matching Begin/Update/End markers and 1..=128 hunks")
    } else if args.expected_sha256.as_ref().is_some_and(|expected| {
        expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        Some("expected_sha256 must contain exactly 64 hexadecimal characters")
    } else {
        None
    };
    match message {
        Some(message) => Err(ClientError {
            kind: "invalid_params".to_string(),
            message: message.to_string(),
        }),
        None => Ok(()),
    }
}

fn valid_patch_envelope(path: &str, patch: &str) -> bool {
    let lines: Vec<&str> = patch
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let declared_path = lines
        .get(1)
        .and_then(|line| line.strip_prefix("*** Update File: "));
    let hunks = lines.iter().filter(|line| **line == "@@").count();
    lines.first() == Some(&"*** Begin Patch")
        && lines.last() == Some(&"*** End Patch")
        && declared_path == Some(path)
        && (1..=APPLY_PATCH_MAX_HUNKS).contains(&hunks)
}

fn validate_read_file_lines_args(args: ReadFileLinesArgs) -> Result<(), ClientError> {
    validate_remote_path(&args.path)?;
    if args.start_line == 0 {
        return invalid_params("start_line must be at least 1");
    }
    if let Some(end_line) = args.end_line {
        if end_line < args.start_line {
            return invalid_params("end_line must be greater than or equal to start_line");
        }
        if end_line - args.start_line >= READ_FILE_LINES_MAX_LINES {
            return invalid_params(format!(
                "line range must not exceed {READ_FILE_LINES_MAX_LINES} lines"
            ));
        }
    }
    if args.max_bytes > READ_FILE_LINES_MAX_BYTES {
        return invalid_params(format!(
            "max_bytes must be in range 0..={READ_FILE_LINES_MAX_BYTES}"
        ));
    }
    Ok(())
}

fn validate_grep_args(args: GrepArgs) -> Result<(), ClientError> {
    validate_remote_path(&args.path)?;
    if args.pattern.is_empty() {
        return invalid_params("pattern must not be empty");
    }
    if args.pattern.len() > GREP_MAX_PATTERN_BYTES {
        return invalid_params(format!("pattern exceeds {GREP_MAX_PATTERN_BYTES} bytes"));
    }
    RegexBuilder::new(&args.pattern)
        .case_insensitive(!args.case_sensitive)
        .build()
        .map_err(|error| ClientError {
            kind: "invalid_params".to_string(),
            message: format!("invalid regex pattern: {error}"),
        })?;
    validate_glob(args.glob.as_deref())?;
    if args.max_results == 0 || args.max_results > GREP_MAX_RESULTS {
        return invalid_params(format!(
            "max_results must be in range 1..={GREP_MAX_RESULTS}"
        ));
    }
    if args.max_file_bytes == 0 || args.max_file_bytes > GREP_MAX_FILE_BYTES {
        return invalid_params(format!(
            "max_file_bytes must be in range 1..={GREP_MAX_FILE_BYTES}"
        ));
    }
    Ok(())
}

fn validate_list_files_args(args: ListFilesArgs) -> Result<(), ClientError> {
    validate_remote_path(&args.path)?;
    if args
        .cursor
        .as_ref()
        .is_some_and(|value| value.contains('\0'))
    {
        return invalid_params("cursor must not contain NUL");
    }
    if args.limit == 0 || args.limit > 1000 {
        return invalid_params("limit must be in range 1..=1000");
    }
    if args.max_depth == 0 || args.max_depth > LIST_FILES_MAX_DEPTH {
        return invalid_params(format!(
            "max_depth must be in range 1..={LIST_FILES_MAX_DEPTH}"
        ));
    }
    validate_glob(args.pattern.as_deref())?;
    let _ = args.recursive;
    Ok(())
}

fn validate_remote_path(path: &str) -> Result<(), ClientError> {
    if path.is_empty() {
        invalid_params("path must not be empty")
    } else if path.contains('\0') {
        invalid_params("path must not contain NUL")
    } else {
        Ok(())
    }
}

fn validate_glob(pattern: Option<&str>) -> Result<(), ClientError> {
    let Some(pattern) = pattern else {
        return Ok(());
    };
    if pattern.is_empty() {
        return invalid_params("glob pattern must not be empty");
    }
    if pattern.len() > MAX_GLOB_BYTES {
        return invalid_params(format!("glob pattern exceeds {MAX_GLOB_BYTES} bytes"));
    }
    Glob::new(pattern).map(|_| ()).map_err(|error| ClientError {
        kind: "invalid_params".to_string(),
        message: format!("invalid glob pattern: {error}"),
    })
}

fn invalid_params<T>(message: impl Into<String>) -> Result<T, ClientError> {
    Err(ClientError {
        kind: "invalid_params".to_string(),
        message: message.into(),
    })
}

fn validate_pkill_args(args: PkillArgs) -> Result<(), ClientError> {
    let message = if args.name.is_empty() {
        Some("name must not be empty")
    } else if args.name.chars().count() > PKILL_MAX_NAME_CHARS {
        Some("name must not exceed 260 characters")
    } else if args.name.contains('\0') {
        Some("name must not contain NUL")
    } else if !(1..=64).contains(&args.signal) {
        Some("signal must be in range 1..=64")
    } else {
        None
    };
    match message {
        Some(message) => Err(ClientError {
            kind: "invalid_params".to_string(),
            message: message.to_string(),
        }),
        None => Ok(()),
    }
}

pub fn tool_names() -> &'static [&'static str] {
    &[
        "read_text",
        "read_file_lines",
        "tail_text",
        "write_text",
        "apply_patch",
        "list_files",
        "grep",
        "stat",
        "file_hash",
        "pids",
        "process_info",
        "kill",
        "pkill",
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
            "read_file_lines",
            "Read file lines",
            "Read a bounded 1-based line range from a remote UTF-8 text file",
            props(&[
                ("path", string_prop("Remote file path")),
                (
                    "start_line",
                    integer_prop_default(1, None, 1, "First line, inclusive"),
                ),
                (
                    "end_line",
                    integer_prop(
                        1,
                        None,
                        "Last line, inclusive; defaults to 200 lines from start_line",
                    ),
                ),
                (
                    "max_bytes",
                    integer_prop_default(0, Some(1_048_576), 262_144, "Maximum returned bytes"),
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
            "apply_patch",
            "Apply text patch",
            "Atomically apply a context-checked patch to one remote UTF-8 text file",
            props(&[
                ("path", string_prop("Remote file path")),
                (
                    "patch",
                    json!({
                        "type":"string",
                        "minLength":1,
                        "maxLength":APPLY_PATCH_MAX_PATCH_BYTES,
                        "description":"Patch format: *** Begin Patch\\n*** Update File: PATH\\n@@\\n-old\\n+new\\n unchanged context\\n*** End Patch"
                    }),
                ),
                (
                    "expected_sha256",
                    json!({"type":"string","pattern":"^[0-9A-Fa-f]{64}$","description":"Optional SHA-256 required to match the current file"}),
                ),
            ]),
            &["path", "patch"],
            (false, true, true),
        ),
        tool(
            "list_files",
            "List files",
            "List, filter, and optionally recurse through a remote directory with pagination",
            props(&[
                ("path", string_prop("Remote directory path")),
                (
                    "cursor",
                    string_prop("Exclusive relative-path cursor from a previous response"),
                ),
                (
                    "limit",
                    integer_prop_default(1, Some(1000), 200, "Maximum entries"),
                ),
                ("recursive", json!({"type":"boolean","default":false})),
                (
                    "pattern",
                    json!({"type":"string","minLength":1,"maxLength":MAX_GLOB_BYTES,"description":"Optional glob matched against relative paths"}),
                ),
                (
                    "max_depth",
                    integer_prop_default(
                        1,
                        Some(LIST_FILES_MAX_DEPTH as u64),
                        16,
                        "Maximum recursive depth; ignored when recursive is false",
                    ),
                ),
            ]),
            &["path"],
            (true, false, true),
        ),
        tool(
            "grep",
            "Search text",
            "Search regular UTF-8 files with a Rust regular expression",
            props(&[
                ("path", string_prop("Remote file or directory path")),
                (
                    "pattern",
                    json!({"type":"string","minLength":1,"maxLength":GREP_MAX_PATTERN_BYTES,"description":"Rust regular expression matched against each line"}),
                ),
                (
                    "glob",
                    json!({"type":"string","minLength":1,"maxLength":MAX_GLOB_BYTES,"description":"Optional glob matched against relative file paths"}),
                ),
                ("case_sensitive", json!({"type":"boolean","default":true})),
                (
                    "max_results",
                    integer_prop_default(
                        1,
                        Some(GREP_MAX_RESULTS as u64),
                        200,
                        "Maximum matching lines",
                    ),
                ),
                (
                    "max_file_bytes",
                    integer_prop_default(
                        1,
                        Some(GREP_MAX_FILE_BYTES),
                        1_048_576,
                        "Maximum bytes scanned per file",
                    ),
                ),
            ]),
            &["path", "pattern"],
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
            "pkill",
            "Terminate processes by name",
            "Terminate Linux, Windows, or macOS processes whose platform name exactly matches",
            props(&[
                (
                    "name",
                    json!({"type":"string","minLength":1,"maxLength":260,"description":"Exact platform process name; Linux allows 15 bytes, macOS 31 bytes, and Windows 260 UTF-16 code units"}),
                ),
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
            &["name"],
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
            "Change the remote IPv4 address, port, or both for subsequent calls; at least one must be provided",
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
    let input_schema = json!({
        "type":"object",
        "properties":properties,
        "required":required,
        "additionalProperties":false
    });
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
        "read_file_lines" => strict_output(
            json!({
                "start_line":{"type":"integer","minimum":1},
                "end_line":{"type":"integer","minimum":1},
                "lines_returned":{"type":"integer","minimum":0,"maximum":READ_FILE_LINES_MAX_LINES},
                "bytes_returned":{"type":"integer","minimum":0,"maximum":READ_FILE_LINES_MAX_BYTES},
                "next_line":{"type":["integer","null"],"minimum":1},
                "truncated":{"type":"boolean"}
            }),
            &[
                "start_line",
                "end_line",
                "lines_returned",
                "bytes_returned",
                "next_line",
                "truncated",
            ],
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
        "apply_patch" => strict_output(
            json!({
                "bytes_before":{"type":"integer","minimum":0},
                "bytes_after":{"type":"integer","minimum":0},
                "hunks_applied":{"type":"integer","minimum":1,"maximum":APPLY_PATCH_MAX_HUNKS},
                "sha256_before":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                "sha256_after":{"type":"string","pattern":"^[0-9a-f]{64}$"}
            }),
            &[
                "bytes_before",
                "bytes_after",
                "hunks_applied",
                "sha256_before",
                "sha256_after",
            ],
        ),
        "list_files" => strict_output(
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
        "grep" => strict_output(
            json!({
                "matches":{"type":"array","maxItems":GREP_MAX_RESULTS,"items":{
                    "type":"object",
                    "properties":{
                        "path":{"type":"string"},
                        "line":{"type":"integer","minimum":1},
                        "column":{"type":"integer","minimum":1},
                        "text":{"type":"string"},
                        "text_truncated":{"type":"boolean"}
                    },
                    "required":["path","line","column","text","text_truncated"],
                    "additionalProperties":false
                }},
                "files_scanned":{"type":"integer","minimum":0,"maximum":10000},
                "files_skipped":{"type":"integer","minimum":0},
                "bytes_scanned":{"type":"integer","minimum":0,"maximum":67108864},
                "truncated":{"type":"boolean"}
            }),
            &[
                "matches",
                "files_scanned",
                "files_skipped",
                "bytes_scanned",
                "truncated",
            ],
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
        "pkill" => strict_output(
            json!({
                "name":{"type":"string","maxLength":260},
                "signal":{"type":"integer","minimum":1,"maximum":64},
                "matched":{"type":"integer","minimum":0,"maximum":1024},
                "signaled_pids":{"type":"array","maxItems":1024,"items":{"type":"integer","minimum":1}},
                "failed_pids":{"type":"array","maxItems":1024,"items":{"type":"integer","minimum":1}}
            }),
            &["name", "signal", "matched", "signaled_pids", "failed_pids"],
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
        assert_eq!(tools.len(), 20);
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
        assert!(tools.iter().all(|tool| {
            ["anyOf", "oneOf", "allOf"]
                .iter()
                .all(|keyword| tool["inputSchema"].get(keyword).is_none())
        }));
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
        let pkill = tools.iter().find(|tool| tool["name"] == "pkill").unwrap();
        assert_eq!(
            pkill["description"],
            "Terminate Linux, Windows, or macOS processes whose platform name exactly matches"
        );
        assert_eq!(pkill["inputSchema"]["properties"]["name"]["maxLength"], 260);
        assert_eq!(
            pkill["inputSchema"]["properties"]["signal"]["description"],
            "Unix signal number; Windows accepts 9 or 15"
        );
        assert_eq!(pkill["annotations"]["destructiveHint"], true);
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
        assert_eq!(setter["inputSchema"]["required"], json!([]));
        assert!(
            setter["description"]
                .as_str()
                .unwrap()
                .contains("at least one must be provided")
        );
        assert_eq!(setter["annotations"]["destructiveHint"], true);
        let patch = tools
            .iter()
            .find(|tool| tool["name"] == "apply_patch")
            .unwrap();
        assert_eq!(
            patch["inputSchema"]["properties"]["patch"]["maxLength"],
            APPLY_PATCH_MAX_PATCH_BYTES
        );
        assert!(
            patch["inputSchema"]["properties"]["patch"]["description"]
                .as_str()
                .unwrap()
                .contains("*** Update File: PATH")
        );
        assert_eq!(patch["inputSchema"]["required"], json!(["path", "patch"]));
        assert_eq!(
            patch["outputSchema"]["properties"]["hunks_applied"]["maximum"],
            APPLY_PATCH_MAX_HUNKS
        );
        assert_eq!(patch["annotations"]["idempotentHint"], true);
        assert!(!tools.iter().any(|tool| tool["name"] == "ls"));
        let line_reader = tools
            .iter()
            .find(|tool| tool["name"] == "read_file_lines")
            .unwrap();
        assert_eq!(
            line_reader["inputSchema"]["properties"]["max_bytes"]["maximum"],
            READ_FILE_LINES_MAX_BYTES
        );
        let listing = tools
            .iter()
            .find(|tool| tool["name"] == "list_files")
            .unwrap();
        assert_eq!(
            listing["inputSchema"]["properties"]["recursive"]["default"],
            false
        );
        assert_eq!(
            listing["inputSchema"]["properties"]["max_depth"]["maximum"],
            LIST_FILES_MAX_DEPTH
        );
        let grep = tools.iter().find(|tool| tool["name"] == "grep").unwrap();
        assert_eq!(
            grep["inputSchema"]["properties"]["max_results"]["maximum"],
            GREP_MAX_RESULTS
        );
        assert_eq!(grep["annotations"]["readOnlyHint"], true);
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

    #[test]
    fn pkill_arguments_are_validated_before_connecting() {
        let mut client = RemoteClient::new(
            "127.0.0.1:1".parse().unwrap(),
            std::time::Duration::from_millis(10),
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        );
        for arguments in [
            json!({}),
            json!({"name":""}),
            json!({"name":"x".repeat(261)}),
            json!({"name":"has\u{0}nul"}),
            json!({"name":"valid","signal":0}),
            json!({"name":"valid","signal":65}),
            json!({"name":"valid","extra":true}),
        ] {
            let response = call_tool(
                json!(1),
                Some(&json!({"name":"pkill","arguments":arguments})),
                &mut client,
            );
            assert_eq!(response["error"]["code"], -32602);
        }
    }

    #[test]
    fn apply_patch_arguments_are_validated_before_connecting() {
        let mut client = RemoteClient::new(
            "127.0.0.1:1".parse().unwrap(),
            std::time::Duration::from_millis(10),
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        );
        for arguments in [
            json!({}),
            json!({"path":"","patch":"x"}),
            json!({"path":"has\u{0}nul","patch":"x"}),
            json!({"path":"file.txt","patch":""}),
            json!({"path":"file.txt","patch":"invalid"}),
            json!({"path":"file.txt","patch":"*** Begin Patch\n*** Update File: other.txt\n@@\n-old\n+new\n*** End Patch"}),
            json!({"path":"file.txt","patch":"x".repeat(APPLY_PATCH_MAX_PATCH_BYTES + 1)}),
            json!({"path":"file.txt","patch":"x","expected_sha256":"invalid"}),
            json!({"path":"file.txt","patch":"x","extra":true}),
        ] {
            let response = call_tool(
                json!(1),
                Some(&json!({"name":"apply_patch","arguments":arguments})),
                &mut client,
            );
            assert_eq!(response["error"]["code"], -32602);
        }
    }

    #[test]
    fn file_discovery_arguments_are_validated_before_connecting() {
        let mut client = RemoteClient::new(
            "127.0.0.1:1".parse().unwrap(),
            std::time::Duration::from_millis(10),
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        );
        let invalid_calls = [
            json!({"name":"read_file_lines","arguments":{"path":"","start_line":1}}),
            json!({"name":"read_file_lines","arguments":{"path":"file","start_line":0}}),
            json!({"name":"read_file_lines","arguments":{"path":"file","start_line":2,"end_line":1}}),
            json!({"name":"read_file_lines","arguments":{"path":"file","max_bytes":READ_FILE_LINES_MAX_BYTES + 1}}),
            json!({"name":"grep","arguments":{"path":".","pattern":""}}),
            json!({"name":"grep","arguments":{"path":".","pattern":"["}}),
            json!({"name":"grep","arguments":{"path":".","pattern":"x","glob":"["}}),
            json!({"name":"grep","arguments":{"path":".","pattern":"x","max_results":0}}),
            json!({"name":"list_files","arguments":{"path":"","recursive":true}}),
            json!({"name":"list_files","arguments":{"path":".","limit":0}}),
            json!({"name":"list_files","arguments":{"path":".","pattern":"["}}),
            json!({"name":"list_files","arguments":{"path":".","max_depth":0}}),
            json!({"name":"ls","arguments":{"path":"."}}),
        ];
        for params in invalid_calls {
            let response = call_tool(
                json!(1),
                Some(&json!({"name":params["name"],"arguments":params["arguments"]})),
                &mut client,
            );
            assert_eq!(response["error"]["code"], -32602, "{params}");
        }
    }
}
