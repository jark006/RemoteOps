use globset::Glob;
use regex::RegexBuilder;
use remote_ops_protocol::{
    APPLY_PATCH_MAX_HUNKS, APPLY_PATCH_MAX_PATCH_BYTES, DEFAULT_PROCESS_JOB_TIMEOUT_MS,
    DEFAULT_PROCESS_OUTPUT_BYTES, DEFAULT_PROCESS_WAIT_MS, DEFAULT_REBOOT_DELAY_MS,
    DEFAULT_REMOTE_PROBE_TIMEOUT_MS, DEFAULT_WAIT_REMOTE_POLL_MS, DEFAULT_WAIT_REMOTE_TIMEOUT_MS,
    MAX_PROCESS_JOB_TIMEOUT_MS, MAX_PROCESS_OUTPUT_BYTES, MAX_PROCESS_WAIT_MS, MAX_REBOOT_DELAY_MS,
    MAX_REMOTE_PROBE_TIMEOUT_MS, MAX_WAIT_REMOTE_POLL_MS, MAX_WAIT_REMOTE_TIMEOUT_MS,
    MIN_REBOOT_DELAY_MS, MIN_REMOTE_PROBE_TIMEOUT_MS, MIN_WAIT_REMOTE_POLL_MS,
    PROTOCOL_VERSION as REMOTE_PROTOCOL_VERSION,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
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
        "agent_info" => {
            decode_transfer::<EmptyArgs>(&arguments).and_then(|_| client.call(name, arguments))
        }
        "remote_probe" => decode_transfer::<RemoteProbeArgs>(&arguments)
            .and_then(validate_remote_probe_args)
            .map(|args| client.remote_probe(args.timeout_ms)),
        "wait_remote" => decode_transfer::<WaitRemoteArgs>(&arguments)
            .and_then(validate_wait_remote_args)
            .map(|args| {
                client.wait_remote(
                    args.wait_for.as_deref(),
                    args.timeout_ms,
                    args.poll_interval_ms,
                    args.probe_timeout_ms,
                )
            }),
        "reboot" => decode_transfer::<RebootArgs>(&arguments)
            .and_then(validate_reboot_args)
            .and_then(|args| client.reboot(args.delay_ms)),
        "agent_update" => decode_transfer::<AgentUpdateArgs>(&arguments)
            .and_then(validate_agent_update_args)
            .and_then(|args| {
                client.agent_update(
                    &args.local_path,
                    args.timeout_ms,
                    args.poll_interval_ms,
                    args.probe_timeout_ms,
                )
            }),
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
        "process_start" => decode_transfer::<ProcessStartArgs>(&arguments)
            .and_then(validate_process_start_args)
            .and_then(|_| client.call(name, arguments)),
        "process_output" => decode_transfer::<ProcessOutputArgs>(&arguments)
            .and_then(validate_process_output_args)
            .and_then(|_| client.call(name, arguments)),
        "process_wait" => decode_transfer::<ProcessWaitArgs>(&arguments)
            .and_then(validate_process_wait_args)
            .and_then(|_| client.call(name, arguments)),
        "process_signal" => decode_transfer::<ProcessSignalArgs>(&arguments)
            .and_then(validate_process_signal_args)
            .and_then(|_| client.call(name, arguments)),
        "process_close" => decode_transfer::<JobIdArgs>(&arguments)
            .and_then(validate_job_id_args)
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
struct RemoteProbeArgs {
    #[serde(default = "default_remote_probe_timeout")]
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitRemoteArgs {
    wait_for: Option<String>,
    #[serde(default = "default_wait_remote_timeout")]
    timeout_ms: u64,
    #[serde(default = "default_wait_remote_poll")]
    poll_interval_ms: u64,
    #[serde(default = "default_remote_probe_timeout")]
    probe_timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RebootArgs {
    #[serde(default = "default_reboot_delay")]
    delay_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentUpdateArgs {
    local_path: String,
    #[serde(default = "default_wait_remote_timeout")]
    timeout_ms: u64,
    #[serde(default = "default_wait_remote_poll")]
    poll_interval_ms: u64,
    #[serde(default = "default_remote_probe_timeout")]
    probe_timeout_ms: u64,
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
struct ProcessStartArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default = "default_process_job_timeout")]
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessOutputArgs {
    job_id: u64,
    #[serde(default)]
    stdout_cursor: u64,
    #[serde(default)]
    stderr_cursor: u64,
    #[serde(default = "default_process_output_bytes")]
    max_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessWaitArgs {
    job_id: u64,
    #[serde(default = "default_process_wait")]
    wait_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessSignalArgs {
    job_id: u64,
    #[serde(default = "default_signal")]
    signal: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobIdArgs {
    job_id: u64,
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

fn default_process_job_timeout() -> u64 {
    DEFAULT_PROCESS_JOB_TIMEOUT_MS
}

fn default_process_output_bytes() -> usize {
    DEFAULT_PROCESS_OUTPUT_BYTES
}

fn default_process_wait() -> u64 {
    DEFAULT_PROCESS_WAIT_MS
}

fn default_remote_probe_timeout() -> u64 {
    DEFAULT_REMOTE_PROBE_TIMEOUT_MS
}

fn default_wait_remote_timeout() -> u64 {
    DEFAULT_WAIT_REMOTE_TIMEOUT_MS
}

fn default_wait_remote_poll() -> u64 {
    DEFAULT_WAIT_REMOTE_POLL_MS
}

fn default_reboot_delay() -> u64 {
    DEFAULT_REBOOT_DELAY_MS
}

fn validate_remote_probe_args(args: RemoteProbeArgs) -> Result<RemoteProbeArgs, ClientError> {
    validate_probe_timeout(args.timeout_ms)?;
    Ok(args)
}

fn validate_wait_remote_args(args: WaitRemoteArgs) -> Result<WaitRemoteArgs, ClientError> {
    if args
        .wait_for
        .as_deref()
        .is_some_and(|wait_for| !matches!(wait_for, "online" | "offline" | "offline_then_online"))
    {
        return invalid_params("wait_for must be one of online, offline, or offline_then_online");
    }
    validate_wait_settings(
        args.timeout_ms,
        args.poll_interval_ms,
        args.probe_timeout_ms,
    )?;
    Ok(args)
}

fn validate_reboot_args(args: RebootArgs) -> Result<RebootArgs, ClientError> {
    if !(MIN_REBOOT_DELAY_MS..=MAX_REBOOT_DELAY_MS).contains(&args.delay_ms) {
        return invalid_params(format!(
            "delay_ms must be in range {MIN_REBOOT_DELAY_MS}..={MAX_REBOOT_DELAY_MS}"
        ));
    }
    Ok(args)
}

fn validate_agent_update_args(args: AgentUpdateArgs) -> Result<AgentUpdateArgs, ClientError> {
    if args.local_path.is_empty() {
        return invalid_params("local_path must not be empty");
    }
    if args.local_path.contains('\0') {
        return invalid_params("local_path must not contain NUL");
    }
    validate_wait_settings(
        args.timeout_ms,
        args.poll_interval_ms,
        args.probe_timeout_ms,
    )?;
    Ok(args)
}

fn validate_wait_settings(
    timeout_ms: u64,
    poll_interval_ms: u64,
    probe_timeout_ms: u64,
) -> Result<(), ClientError> {
    if timeout_ms == 0 || timeout_ms > MAX_WAIT_REMOTE_TIMEOUT_MS {
        return invalid_params(format!(
            "timeout_ms must be in range 1..={MAX_WAIT_REMOTE_TIMEOUT_MS}"
        ));
    }
    if !(MIN_WAIT_REMOTE_POLL_MS..=MAX_WAIT_REMOTE_POLL_MS).contains(&poll_interval_ms) {
        return invalid_params(format!(
            "poll_interval_ms must be in range {MIN_WAIT_REMOTE_POLL_MS}..={MAX_WAIT_REMOTE_POLL_MS}"
        ));
    }
    validate_probe_timeout(probe_timeout_ms)
}

fn validate_probe_timeout(timeout_ms: u64) -> Result<(), ClientError> {
    if !(MIN_REMOTE_PROBE_TIMEOUT_MS..=MAX_REMOTE_PROBE_TIMEOUT_MS).contains(&timeout_ms) {
        invalid_params(format!(
            "probe timeout must be in range {MIN_REMOTE_PROBE_TIMEOUT_MS}..={MAX_REMOTE_PROBE_TIMEOUT_MS}"
        ))
    } else {
        Ok(())
    }
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

fn validate_process_start_args(args: ProcessStartArgs) -> Result<(), ClientError> {
    if args.program.is_empty() {
        return invalid_params("program must not be empty");
    }
    if args.program.contains('\0') {
        return invalid_params("program must not contain NUL");
    }
    if args.args.iter().any(|arg| arg.contains('\0')) {
        return invalid_params("args must not contain NUL");
    }
    if let Some(cwd) = args.cwd {
        if cwd.is_empty() {
            return invalid_params("cwd must not be empty");
        }
        if cwd.contains('\0') {
            return invalid_params("cwd must not contain NUL");
        }
    }
    if args.env.iter().any(|(name, value)| {
        name.is_empty() || name.contains('\0') || name.contains('=') || value.contains('\0')
    }) {
        return invalid_params(
            "environment names must be non-empty without NUL or '=' and values must not contain NUL",
        );
    }
    if args.timeout_ms == 0 || args.timeout_ms > MAX_PROCESS_JOB_TIMEOUT_MS {
        return invalid_params(format!(
            "timeout_ms must be in range 1..={MAX_PROCESS_JOB_TIMEOUT_MS}"
        ));
    }
    Ok(())
}

fn validate_process_output_args(args: ProcessOutputArgs) -> Result<(), ClientError> {
    validate_job_id(args.job_id)?;
    if args.max_bytes > MAX_PROCESS_OUTPUT_BYTES {
        return invalid_params(format!(
            "max_bytes must be in range 0..={MAX_PROCESS_OUTPUT_BYTES}"
        ));
    }
    let _ = (args.stdout_cursor, args.stderr_cursor);
    Ok(())
}

fn validate_process_wait_args(args: ProcessWaitArgs) -> Result<(), ClientError> {
    validate_job_id(args.job_id)?;
    if args.wait_ms > MAX_PROCESS_WAIT_MS {
        return invalid_params(format!(
            "wait_ms must be in range 0..={MAX_PROCESS_WAIT_MS}"
        ));
    }
    Ok(())
}

fn validate_process_signal_args(args: ProcessSignalArgs) -> Result<(), ClientError> {
    validate_job_id(args.job_id)?;
    if !(1..=64).contains(&args.signal) {
        return invalid_params("signal must be in range 1..=64");
    }
    Ok(())
}

fn validate_job_id_args(args: JobIdArgs) -> Result<(), ClientError> {
    validate_job_id(args.job_id)
}

fn validate_job_id(job_id: u64) -> Result<(), ClientError> {
    if job_id == 0 {
        invalid_params("job_id must be at least 1")
    } else {
        Ok(())
    }
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
        "process_start",
        "process_output",
        "process_wait",
        "process_signal",
        "process_close",
        "system_info",
        "upload_file",
        "download_file",
        "remote_status",
        "set_remote",
        "agent_info",
        "remote_probe",
        "wait_remote",
        "reboot",
        "agent_update",
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
            "process_start",
            "Start background process",
            "Start a managed remote program and return immediately with a job ID",
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
                    integer_prop_default(
                        1,
                        Some(MAX_PROCESS_JOB_TIMEOUT_MS),
                        DEFAULT_PROCESS_JOB_TIMEOUT_MS,
                        "Maximum job runtime in milliseconds",
                    ),
                ),
            ]),
            &["program"],
            (false, true, false),
        ),
        tool(
            "process_output",
            "Read background output",
            "Read bounded incremental stdout and stderr from a managed background job",
            props(&[
                ("job_id", integer_prop(1, None, "Background job ID")),
                (
                    "stdout_cursor",
                    integer_prop_default(0, None, 0, "Absolute stdout byte cursor"),
                ),
                (
                    "stderr_cursor",
                    integer_prop_default(0, None, 0, "Absolute stderr byte cursor"),
                ),
                (
                    "max_bytes",
                    integer_prop_default(
                        0,
                        Some(MAX_PROCESS_OUTPUT_BYTES as u64),
                        DEFAULT_PROCESS_OUTPUT_BYTES as u64,
                        "Maximum bytes returned from each stream",
                    ),
                ),
            ]),
            &["job_id"],
            (true, false, true),
        ),
        tool(
            "process_wait",
            "Wait for background process",
            "Wait for a managed background job to finish for a bounded interval",
            props(&[
                ("job_id", integer_prop(1, None, "Background job ID")),
                (
                    "wait_ms",
                    integer_prop_default(
                        0,
                        Some(MAX_PROCESS_WAIT_MS),
                        DEFAULT_PROCESS_WAIT_MS,
                        "Maximum wait in milliseconds",
                    ),
                ),
            ]),
            &["job_id"],
            (true, false, true),
        ),
        tool(
            "process_signal",
            "Signal background process",
            "Signal a managed Unix process group or terminate its Windows Job Object",
            props(&[
                ("job_id", integer_prop(1, None, "Background job ID")),
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
            &["job_id"],
            (false, true, false),
        ),
        tool(
            "process_close",
            "Close background job",
            "Release a finished background job and its retained output",
            props(&[("job_id", integer_prop(1, None, "Background job ID"))]),
            &["job_id"],
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
        tool(
            "agent_info",
            "Agent information",
            "Read the remote Agent version, build, runtime identity, capabilities, and limits",
            props(&[]),
            &[],
            (true, false, true),
        ),
        tool(
            "remote_probe",
            "Probe remote",
            "Actively connect or health-check the configured remote and return latency and Agent information",
            props(&[(
                "timeout_ms",
                integer_prop_default(
                    MIN_REMOTE_PROBE_TIMEOUT_MS,
                    Some(MAX_REMOTE_PROBE_TIMEOUT_MS),
                    DEFAULT_REMOTE_PROBE_TIMEOUT_MS,
                    "Maximum time for this active probe",
                ),
            )]),
            &[],
            (true, false, true),
        ),
        tool(
            "wait_remote",
            "Wait for remote",
            "Wait for the remote to become offline, online, or cycle offline then online with a healthy Agent",
            props(&[
                (
                    "wait_for",
                    json!({
                        "type":"string",
                        "enum":["online","offline","offline_then_online"],
                        "description":"Condition to wait for; defaults to offline_then_online while rebooting/updating, otherwise online"
                    }),
                ),
                (
                    "timeout_ms",
                    integer_prop_default(
                        1,
                        Some(MAX_WAIT_REMOTE_TIMEOUT_MS),
                        DEFAULT_WAIT_REMOTE_TIMEOUT_MS,
                        "Overall bounded wait time",
                    ),
                ),
                (
                    "poll_interval_ms",
                    integer_prop_default(
                        MIN_WAIT_REMOTE_POLL_MS,
                        Some(MAX_WAIT_REMOTE_POLL_MS),
                        DEFAULT_WAIT_REMOTE_POLL_MS,
                        "Delay between probes",
                    ),
                ),
                (
                    "probe_timeout_ms",
                    integer_prop_default(
                        MIN_REMOTE_PROBE_TIMEOUT_MS,
                        Some(MAX_REMOTE_PROBE_TIMEOUT_MS),
                        DEFAULT_REMOTE_PROBE_TIMEOUT_MS,
                        "Maximum time for each active probe",
                    ),
                ),
            ]),
            &[],
            (true, false, true),
        ),
        tool(
            "reboot",
            "Reboot remote",
            "Request a delayed device reboot with expected-disconnect lifecycle semantics",
            props(&[(
                "delay_ms",
                integer_prop_default(
                    MIN_REBOOT_DELAY_MS,
                    Some(MAX_REBOOT_DELAY_MS),
                    DEFAULT_REBOOT_DELAY_MS,
                    "Delay before the Agent triggers the reboot",
                ),
            )]),
            &[],
            (false, true, false),
        ),
        tool(
            "agent_update",
            "Update Agent",
            "Stage, verify, atomically replace, restart, and verify the remote Agent with automatic rollback",
            props(&[
                (
                    "local_path",
                    string_prop("Candidate Agent binary on the proxy PC"),
                ),
                (
                    "timeout_ms",
                    integer_prop_default(
                        1,
                        Some(MAX_WAIT_REMOTE_TIMEOUT_MS),
                        DEFAULT_WAIT_REMOTE_TIMEOUT_MS,
                        "Maximum time to wait for restart or rollback",
                    ),
                ),
                (
                    "poll_interval_ms",
                    integer_prop_default(
                        MIN_WAIT_REMOTE_POLL_MS,
                        Some(MAX_WAIT_REMOTE_POLL_MS),
                        DEFAULT_WAIT_REMOTE_POLL_MS,
                        "Delay between restart probes",
                    ),
                ),
                (
                    "probe_timeout_ms",
                    integer_prop_default(
                        MIN_REMOTE_PROBE_TIMEOUT_MS,
                        Some(MAX_REMOTE_PROBE_TIMEOUT_MS),
                        DEFAULT_REMOTE_PROBE_TIMEOUT_MS,
                        "Maximum time for each restart probe",
                    ),
                ),
            ]),
            &["local_path"],
            (false, true, false),
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
        "annotations": {"readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": idempotent, "openWorldHint": matches!(name, "sh_exec" | "exec" | "process_start" | "reboot" | "agent_update")}
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
        "process_start" | "process_wait" => process_status_output(json!({}), &[]),
        "process_output" => process_status_output(
            json!({
                "stdout":{"type":"string"},
                "stderr":{"type":"string"},
                "stdout_start_cursor":{"type":"integer","minimum":0},
                "stderr_start_cursor":{"type":"integer","minimum":0},
                "next_stdout_cursor":{"type":"integer","minimum":0},
                "next_stderr_cursor":{"type":"integer","minimum":0},
                "stdout_truncated":{"type":"boolean"},
                "stderr_truncated":{"type":"boolean"}
            }),
            &[
                "stdout",
                "stderr",
                "stdout_start_cursor",
                "stderr_start_cursor",
                "next_stdout_cursor",
                "next_stderr_cursor",
                "stdout_truncated",
                "stderr_truncated",
            ],
        ),
        "process_signal" => process_status_output(
            json!({"signal":{"type":"integer","minimum":1,"maximum":64}}),
            &["signal"],
        ),
        "process_close" => strict_output(
            json!({
                "job_id":{"type":"integer","minimum":1},
                "closed":{"type":"boolean","const":true}
            }),
            &["job_id", "closed"],
        ),
        "system_info" => system_info_output_schema(),
        "agent_info" => agent_info_output_schema(),
        "remote_probe" => remote_probe_output_schema(),
        "wait_remote" => strict_output(
            json!({
                "address":{"type":"string"},
                "wait_for":{"type":"string","enum":["online","offline","offline_then_online"]},
                "reached":{"type":"boolean"},
                "timed_out":{"type":"boolean"},
                "observed_offline":{"type":"boolean"},
                "attempts":{"type":"integer","minimum":0},
                "elapsed_ms":{"type":"integer","minimum":0},
                "connected":{"type":"boolean"},
                "connection_state":{"type":"string","enum":["cached","disconnected"]},
                "lifecycle_state":{"type":"string","enum":["ready","rebooting","updating"]},
                "last_probe":{"type":["object","null"]},
                "agent_info":{"type":["object","null"]}
            }),
            &[
                "address",
                "wait_for",
                "reached",
                "timed_out",
                "observed_offline",
                "attempts",
                "elapsed_ms",
                "connected",
                "connection_state",
                "lifecycle_state",
                "last_probe",
                "agent_info",
            ],
        ),
        "reboot" => strict_output(
            json!({
                "accepted":{"type":"boolean","const":true},
                "acknowledged":{"type":"boolean"},
                "disconnect_observed":{"type":"boolean"},
                "requested_at_ms":{"type":"integer","minimum":0},
                "delay_ms":{"type":"integer","minimum":MIN_REBOOT_DELAY_MS,"maximum":MAX_REBOOT_DELAY_MS},
                "previous_instance_id":{"type":["string","null"]},
                "lifecycle_state":{"type":"string","const":"rebooting"},
                "agent_response":{"type":["object","null"]}
            }),
            &[
                "accepted",
                "acknowledged",
                "disconnect_observed",
                "requested_at_ms",
                "delay_ms",
                "previous_instance_id",
                "lifecycle_state",
                "agent_response",
            ],
        ),
        "agent_update" => strict_output(
            json!({
                "status":{"type":"string","enum":["updated","rolled_back","timed_out","unconfirmed"]},
                "updated":{"type":"boolean"},
                "rolled_back":{"type":"boolean"},
                "timed_out":{"type":"boolean"},
                "restart_acknowledged":{"type":"boolean"},
                "previous_agent":{"type":"object"},
                "candidate":{"type":["object","null"]},
                "current_agent":{"type":["object","null"]},
                "staging_path":{"type":"string"},
                "bytes_transferred":{"type":"integer","minimum":0},
                "sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                "wait":{"type":"object"},
                "elapsed_ms":{"type":"integer","minimum":0}
            }),
            &[
                "status",
                "updated",
                "rolled_back",
                "timed_out",
                "restart_acknowledged",
                "previous_agent",
                "candidate",
                "current_agent",
                "staging_path",
                "bytes_transferred",
                "sha256",
                "wait",
                "elapsed_ms",
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
                "connected":{"type":"boolean","description":"Whether the proxy holds an authenticated session; no active probe is performed"},
                "connection_state":{"type":"string","enum":["cached","disconnected"]},
                "lifecycle_state":{"type":"string","enum":["ready","rebooting","updating"]},
                "last_success_at_ms":{"type":["integer","null"],"minimum":0},
                "last_error":{"type":["object","null"]},
                "last_probe":{"type":["object","null"]},
                "agent_info":{"type":["object","null"]}
            }),
            &[
                "ip",
                "port",
                "address",
                "connected",
                "connection_state",
                "lifecycle_state",
                "last_success_at_ms",
                "last_error",
                "last_probe",
                "agent_info",
            ],
        ),
        _ => json!({"type":"object"}),
    }
}

fn system_info_output_schema() -> Value {
    let nullable_string = |max_length| json!({"type":["string","null"],"maxLength":max_length});
    let account = || {
        json!({
            "type":"object",
            "properties":{
                "id":{"type":["integer","null"],"minimum":0},
                "name":nullable_string(256)
            },
            "required":["id","name"],
            "additionalProperties":false
        })
    };
    let capability_set = || {
        json!({
            "type":"object",
            "properties":{
                "mask":{"type":"string","pattern":"^[0-9A-Fa-f]{1,16}$"},
                "names":{"type":"array","maxItems":64,"items":{"type":"string","pattern":"^CAP_[A-Z0-9_]+$"}}
            },
            "required":["mask","names"],
            "additionalProperties":false
        })
    };
    let bounded_collection = |items: Value, max_items: usize| {
        json!({
            "type":"object",
            "properties":{
                "available":{"type":"boolean"},
                "items":{"type":"array","maxItems":max_items,"items":items},
                "truncated":{"type":"boolean"}
            },
            "required":["available","items","truncated"],
            "additionalProperties":false
        })
    };
    let interface_address = json!({
        "type":"object",
        "properties":{
            "family":{"type":"string","enum":["ipv4","ipv6"]},
            "address":{"type":"string","maxLength":64},
            "prefix_length":{"type":"integer","minimum":0,"maximum":128},
            "scope":{"type":"string","enum":["host","unspecified","multicast","link","global"]}
        },
        "required":["family","address","prefix_length","scope"],
        "additionalProperties":false
    });
    let network_interface = json!({
        "type":"object",
        "properties":{
            "name":{"type":"string","maxLength":256},
            "index":{"type":["integer","null"],"minimum":0},
            "up":{"type":"boolean"},
            "loopback":{"type":"boolean"},
            "point_to_point":{"type":"boolean"},
            "mac_address":nullable_string(128),
            "mtu":{"type":["integer","null"],"minimum":0},
            "addresses":{"type":"array","maxItems":512,"items":interface_address}
        },
        "required":["name","index","up","loopback","point_to_point","mac_address","mtu","addresses"],
        "additionalProperties":false
    });
    let route = json!({
        "type":"object",
        "properties":{
            "family":{"type":"string","enum":["ipv4","ipv6"]},
            "destination":{"type":"string","maxLength":128},
            "gateway":nullable_string(64),
            "interface":{"type":"string","maxLength":256},
            "metric":{"type":"integer","minimum":0},
            "flags":{"type":"integer","minimum":0}
        },
        "required":["family","destination","gateway","interface","metric","flags"],
        "additionalProperties":false
    });
    let listening_port = json!({
        "type":"object",
        "properties":{
            "protocol":{"type":"string","enum":["tcp","udp"]},
            "family":{"type":"string","enum":["ipv4","ipv6"]},
            "local_address":{"type":"string","maxLength":64},
            "port":{"type":"integer","minimum":1,"maximum":65535}
        },
        "required":["protocol","family","local_address","port"],
        "additionalProperties":false
    });
    let mount = json!({
        "type":"object",
        "properties":{
            "source":{"type":"string","maxLength":1024},
            "mount_point":{"type":"string","maxLength":1024},
            "fs_type":nullable_string(128),
            "total_bytes":{"type":["integer","null"],"minimum":0},
            "available_bytes":{"type":["integer","null"],"minimum":0},
            "total_inodes":{"type":["integer","null"],"minimum":0},
            "available_inodes":{"type":["integer","null"],"minimum":0},
            "read_only":{"type":["boolean","null"]}
        },
        "required":["source","mount_point","fs_type","total_bytes","available_bytes","total_inodes","available_inodes","read_only"],
        "additionalProperties":false
    });
    let toolchain = json!({
        "type":"object",
        "properties":{
            "name":{"type":"string","maxLength":64},
            "path":{"type":"string","maxLength":1024}
        },
        "required":["name","path"],
        "additionalProperties":false
    });
    strict_output(
        json!({
            "hostname":{"type":"string","maxLength":256},
            "kernel":{"type":"object","properties":{"sysname":{"type":"string","maxLength":256},"release":{"type":"string","maxLength":256},"machine":{"type":"string","maxLength":256}},"required":["sysname","release","machine"],"additionalProperties":false},
            "uptime_seconds":{"type":"number","minimum":0},
            "load_average":{"type":"object","properties":{"one":{"type":"number"},"five":{"type":"number"},"fifteen":{"type":"number"}},"required":["one","five","fifteen"],"additionalProperties":false},
            "memory":{"type":"object","properties":{"total_bytes":{"type":"integer","minimum":0},"available_bytes":{"type":"integer","minimum":0}},"required":["total_bytes","available_bytes"],"additionalProperties":false},
            "root_filesystem":{"type":"object","properties":{"total_bytes":{"type":"integer","minimum":0},"available_bytes":{"type":"integer","minimum":0}},"required":["total_bytes","available_bytes"],"additionalProperties":false},
            "temperatures":{"type":"array","maxItems":64,"items":{"type":"object","properties":{"name":{"type":"string","maxLength":256},"celsius":{"type":"number"}},"required":["name","celsius"],"additionalProperties":false}},
            "os":{
                "type":"object",
                "properties":{
                    "id":nullable_string(4096),
                    "id_like":{"type":"array","maxItems":16,"items":{"type":"string","maxLength":128}},
                    "name":nullable_string(4096),
                    "pretty_name":nullable_string(4096),
                    "version":nullable_string(4096),
                    "version_id":nullable_string(4096),
                    "version_codename":nullable_string(4096),
                    "variant":nullable_string(4096),
                    "variant_id":nullable_string(4096),
                    "build_id":nullable_string(4096),
                    "image_id":nullable_string(4096),
                    "image_version":nullable_string(4096)
                },
                "required":["id","id_like","name","pretty_name","version","version_id","version_codename","variant","variant_id","build_id","image_id","image_version"],
                "additionalProperties":false
            },
            "cpu":{
                "type":"object",
                "properties":{
                    "model":nullable_string(512),
                    "logical_cores":{"type":"integer","minimum":1},
                    "physical_cores":{"type":["integer","null"],"minimum":1},
                    "architecture":{"type":"string","maxLength":128},
                    "byte_order":{"type":"string","enum":["little","big"]},
                    "abi":{"type":"string","maxLength":128},
                    "build_target":{"type":"string","maxLength":256},
                    "libc":{"type":"object","properties":{"family":{"type":"string","maxLength":128},"version":nullable_string(256)},"required":["family","version"],"additionalProperties":false}
                },
                "required":["model","logical_cores","physical_cores","architecture","byte_order","abi","build_target","libc"],
                "additionalProperties":false
            },
            "identity":{
                "type":"object",
                "properties":{
                    "real_user":account(),
                    "effective_user":account(),
                    "real_group":account(),
                    "effective_group":account(),
                    "supplementary_groups":bounded_collection(account(), 128),
                    "is_root":{"type":["boolean","null"]},
                    "umask":nullable_string(16),
                    "capabilities":{
                        "type":["object","null"],
                        "properties":{
                            "inheritable":capability_set(),
                            "permitted":capability_set(),
                            "effective":capability_set(),
                            "bounding":capability_set(),
                            "ambient":capability_set(),
                            "last_capability":{"type":"integer","minimum":0,"maximum":63}
                        },
                        "required":["inheritable","permitted","effective","bounding","ambient","last_capability"],
                        "additionalProperties":false
                    }
                },
                "required":["real_user","effective_user","real_group","effective_group","supplementary_groups","is_root","umask","capabilities"],
                "additionalProperties":false
            },
            "network":{
                "type":"object",
                "properties":{
                    "interfaces":bounded_collection(network_interface, 128),
                    "routes":bounded_collection(route, 256),
                    "dns":{
                        "type":"object",
                        "properties":{
                            "available":{"type":"boolean"},
                            "servers":{"type":"array","maxItems":16,"items":{"type":"string","maxLength":256}},
                            "search_domains":{"type":"array","maxItems":16,"items":{"type":"string","maxLength":256}},
                            "truncated":{"type":"boolean"}
                        },
                        "required":["available","servers","search_domains","truncated"],
                        "additionalProperties":false
                    },
                    "listening_ports":bounded_collection(listening_port, 512)
                },
                "required":["interfaces","routes","dns","listening_ports"],
                "additionalProperties":false
            },
            "filesystems":{
                "type":"object",
                "properties":{
                    "available":{"type":"boolean"},
                    "mounts":{"type":"array","maxItems":256,"items":mount},
                    "truncated":{"type":"boolean"}
                },
                "required":["available","mounts","truncated"],
                "additionalProperties":false
            },
            "time":{
                "type":"object",
                "properties":{
                    "unix_seconds":{"type":"integer","minimum":0},
                    "timezone":nullable_string(256),
                    "utc_offset_seconds":{"type":["integer","null"],"minimum":-86400,"maximum":86400}
                },
                "required":["unix_seconds","timezone","utc_offset_seconds"],
                "additionalProperties":false
            },
            "init_system":{
                "type":"object",
                "properties":{"name":nullable_string(256),"pid1_comm":nullable_string(256)},
                "required":["name","pid1_comm"],
                "additionalProperties":false
            },
            "toolchains":bounded_collection(toolchain, 24)
        }),
        &[
            "hostname",
            "kernel",
            "uptime_seconds",
            "load_average",
            "memory",
            "root_filesystem",
            "temperatures",
            "os",
            "cpu",
            "identity",
            "network",
            "filesystems",
            "time",
            "init_system",
            "toolchains",
        ],
    )
}

fn agent_info_output_schema() -> Value {
    strict_output(
        json!({
            "name":{"type":"string","const":"remote-ops-agent"},
            "version":{"type":"string"},
            "protocol_version":{"type":"integer","minimum":1},
            "build":{
                "type":"object",
                "properties":{
                    "target":{"type":"string"},
                    "profile":{"type":"string"},
                    "git_revision":{"type":"string"}
                },
                "required":["target","profile","git_revision"],
                "additionalProperties":false
            },
            "runtime":{
                "type":"object",
                "properties":{
                    "instance_id":{"type":"string","pattern":"^[0-9a-f]{32}$"},
                    "pid":{"type":"integer","minimum":1},
                    "started_at_ms":{"type":"integer","minimum":0},
                    "uptime_ms":{"type":"integer","minimum":0}
                },
                "required":["instance_id","pid","started_at_ms","uptime_ms"],
                "additionalProperties":false
            },
            "platform":{
                "type":"object",
                "properties":{
                    "os":{"type":"string"},
                    "arch":{"type":"string"},
                    "family":{"type":"string"}
                },
                "required":["os","arch","family"],
                "additionalProperties":false
            },
            "supported_operations":{"type":"array","items":{"type":"string"}},
            "capabilities":{
                "type":"object",
                "properties":{
                    "background_processes":{"type":"boolean"},
                    "incremental_output":{"type":"boolean"},
                    "active_probe":{"type":"boolean"},
                    "wait_remote":{"type":"boolean"},
                    "reboot":{"type":"boolean"},
                    "self_update":{"type":"boolean"}
                },
                "required":[
                    "background_processes","incremental_output","active_probe","wait_remote",
                    "reboot","self_update"
                ],
                "additionalProperties":false
            },
            "limits":{
                "type":"object",
                "properties":{
                    "max_control_bytes":{"type":"integer","minimum":1},
                    "chunk_bytes":{"type":"integer","minimum":1},
                    "max_transfer_bytes":{"type":"integer","minimum":1},
                    "max_process_jobs":{"type":"integer","minimum":1},
                    "default_process_timeout_ms":{"type":"integer","minimum":1},
                    "max_process_timeout_ms":{"type":"integer","minimum":1},
                    "process_output_buffer_bytes":{"type":"integer","minimum":1},
                    "default_process_output_bytes":{"type":"integer","minimum":1},
                    "max_process_output_bytes":{"type":"integer","minimum":1},
                    "default_process_wait_ms":{"type":"integer","minimum":0},
                    "max_process_wait_ms":{"type":"integer","minimum":0},
                    "apply_patch_max_patch_bytes":{"type":"integer","minimum":1},
                    "apply_patch_max_file_bytes":{"type":"integer","minimum":1},
                    "apply_patch_max_hunks":{"type":"integer","minimum":1}
                },
                "required":[
                    "max_control_bytes","chunk_bytes","max_transfer_bytes","max_process_jobs",
                    "default_process_timeout_ms","max_process_timeout_ms",
                    "process_output_buffer_bytes","default_process_output_bytes",
                    "max_process_output_bytes","default_process_wait_ms","max_process_wait_ms",
                    "apply_patch_max_patch_bytes","apply_patch_max_file_bytes",
                    "apply_patch_max_hunks"
                ],
                "additionalProperties":false
            },
            "update":{
                "type":"object",
                "properties":{
                    "executable_path":{"type":["string","null"]},
                    "staging_path":{"type":["string","null"]},
                    "self_check_timeout_ms":{"type":"integer","minimum":1}
                },
                "required":["executable_path","staging_path","self_check_timeout_ms"],
                "additionalProperties":false
            }
        }),
        &[
            "name",
            "version",
            "protocol_version",
            "build",
            "runtime",
            "platform",
            "supported_operations",
            "capabilities",
            "limits",
            "update",
        ],
    )
}

fn remote_probe_output_schema() -> Value {
    strict_output(
        json!({
            "address":{"type":"string"},
            "reachable":{"type":"boolean"},
            "connected":{"type":"boolean"},
            "connection_reused":{"type":"boolean"},
            "latency_ms":{"type":"integer","minimum":0},
            "probed_at_ms":{"type":"integer","minimum":0},
            "lifecycle_state":{"type":"string","enum":["ready","rebooting","updating"]},
            "agent_info":{"type":["object","null"]},
            "error":{"type":["object","null"]}
        }),
        &[
            "address",
            "reachable",
            "connected",
            "connection_reused",
            "latency_ms",
            "probed_at_ms",
            "lifecycle_state",
            "agent_info",
            "error",
        ],
    )
}

fn process_status_output(extra: Value, extra_required: &[&str]) -> Value {
    let mut properties = match json!({
        "job_id":{"type":"integer","minimum":1},
        "pid":{"type":"integer","minimum":1},
        "state":{"type":"string","enum":["running","exited","failed"]},
        "exit_code":{"type":["integer","null"]},
        "timed_out":{"type":"boolean"},
        "error":{"type":["string","null"]},
        "started_at_ms":{"type":"integer","minimum":0},
        "finished_at_ms":{"type":["integer","null"],"minimum":0},
        "timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_PROCESS_JOB_TIMEOUT_MS},
        "stdout_complete":{"type":"boolean"},
        "stderr_complete":{"type":"boolean"}
    }) {
        Value::Object(properties) => properties,
        _ => unreachable!("literal object"),
    };
    if let Value::Object(extra) = extra {
        properties.extend(extra);
    }
    let mut required = vec![
        "job_id",
        "pid",
        "state",
        "exit_code",
        "timed_out",
        "error",
        "started_at_ms",
        "finished_at_ms",
        "timeout_ms",
        "stdout_complete",
        "stderr_complete",
    ];
    required.extend_from_slice(extra_required);
    strict_output(Value::Object(properties), &required)
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
        assert_eq!(tools.len(), 30);
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
                .all(|field| system["outputSchema"]["properties"]
                    .get(field.as_str().unwrap())
                    .is_some())
        );
        assert_eq!(
            system["outputSchema"]["properties"]["cpu"]["properties"]["byte_order"]["enum"],
            json!(["little", "big"])
        );
        assert_eq!(
            system["outputSchema"]["properties"]["network"]["properties"]["routes"]["properties"]["items"]
                ["maxItems"],
            256
        );
        assert_eq!(
            system["outputSchema"]["properties"]["filesystems"]["properties"]["mounts"]["maxItems"],
            256
        );
        assert_eq!(
            system["outputSchema"]["properties"]["toolchains"]["properties"]["items"]["maxItems"],
            24
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
        let process_start = tools
            .iter()
            .find(|tool| tool["name"] == "process_start")
            .unwrap();
        assert_eq!(
            process_start["inputSchema"]["properties"]["timeout_ms"]["maximum"],
            MAX_PROCESS_JOB_TIMEOUT_MS
        );
        assert_eq!(process_start["annotations"]["openWorldHint"], true);
        let process_output = tools
            .iter()
            .find(|tool| tool["name"] == "process_output")
            .unwrap();
        assert_eq!(
            process_output["inputSchema"]["properties"]["max_bytes"]["maximum"],
            MAX_PROCESS_OUTPUT_BYTES
        );
        assert_eq!(
            process_output["outputSchema"]["properties"]["state"]["enum"],
            json!(["running", "exited", "failed"])
        );
        assert_eq!(process_output["annotations"]["readOnlyHint"], true);
        let process_wait = tools
            .iter()
            .find(|tool| tool["name"] == "process_wait")
            .unwrap();
        assert_eq!(
            process_wait["inputSchema"]["properties"]["wait_ms"]["maximum"],
            MAX_PROCESS_WAIT_MS
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
    fn background_process_arguments_are_validated_before_connecting() {
        let mut client = RemoteClient::new(
            "127.0.0.1:1".parse().unwrap(),
            std::time::Duration::from_millis(10),
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        );
        let invalid_calls = [
            json!({"name":"process_start","arguments":{}}),
            json!({"name":"process_start","arguments":{"program":""}}),
            json!({"name":"process_start","arguments":{"program":"has\u{0}nul"}}),
            json!({"name":"process_start","arguments":{"program":"ok","args":["has\u{0}nul"]}}),
            json!({"name":"process_start","arguments":{"program":"ok","cwd":""}}),
            json!({"name":"process_start","arguments":{"program":"ok","env":{"":"value"}}}),
            json!({"name":"process_start","arguments":{"program":"ok","env":{"A":"has\u{0}nul"}}}),
            json!({"name":"process_start","arguments":{"program":"ok","timeout_ms":0}}),
            json!({"name":"process_start","arguments":{"program":"ok","timeout_ms":MAX_PROCESS_JOB_TIMEOUT_MS + 1}}),
            json!({"name":"process_start","arguments":{"program":"ok","extra":true}}),
            json!({"name":"process_output","arguments":{"job_id":0}}),
            json!({"name":"process_output","arguments":{"job_id":1,"max_bytes":MAX_PROCESS_OUTPUT_BYTES + 1}}),
            json!({"name":"process_output","arguments":{"job_id":1,"extra":true}}),
            json!({"name":"process_wait","arguments":{"job_id":0}}),
            json!({"name":"process_wait","arguments":{"job_id":1,"wait_ms":MAX_PROCESS_WAIT_MS + 1}}),
            json!({"name":"process_signal","arguments":{"job_id":0}}),
            json!({"name":"process_signal","arguments":{"job_id":1,"signal":0}}),
            json!({"name":"process_signal","arguments":{"job_id":1,"signal":65}}),
            json!({"name":"process_close","arguments":{"job_id":0}}),
        ];
        for params in invalid_calls {
            let response = call_tool(json!(1), Some(&params), &mut client);
            assert_eq!(response["error"]["code"], -32602, "{params}");
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
