use crate::client::RemoteClient;
use catalog::Tool;
use remote_ops_protocol::PROTOCOL_VERSION as REMOTE_PROTOCOL_VERSION;
use serde_json::{Value, json};

mod batch;
mod catalog;
mod dispatch;
mod input;
mod output;
mod parameters;
#[cfg(test)]
mod tests;

pub use catalog::{tool_definitions, tool_names};

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

pub fn handle_message(message: Value, client: &mut RemoteClient) -> Option<Value> {
    handle_message_with_response_mode(message, client, crate::response::ResponseMode::default())
}

pub fn handle_message_with_response_mode(
    message: Value,
    client: &mut RemoteClient,
    response_mode: crate::response::ResponseMode,
) -> Option<Value> {
    let id = message.get("id").cloned();
    if message.get("jsonrpc") != Some(&Value::String("2.0".to_string())) {
        return id.map(|id| rpc_error(id, -32600, "invalid JSON-RPC request"));
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return id.map(|id| rpc_error(id, -32600, "method must be a string"));
    };
    let id = id?;
    match method {
        "initialize" => initialize(id, message.get("params"), response_mode),
        "ping" => Some(rpc_result(id, json!({}))),
        "tools/list" => {
            let mut tools = tool_definitions();
            for tool in &mut tools {
                tool["inputSchema"]["properties"]["response_mode"]["default"] =
                    json!(response_mode.name());
            }
            Some(rpc_result(id, json!({"tools":tools})))
        }
        "tools/call" => Some(call_tool_with_response_mode(
            id,
            message.get("params"),
            client,
            response_mode,
        )),
        _ => Some(rpc_error(id, -32601, "method not found")),
    }
}

fn initialize(
    id: Value,
    params: Option<&Value>,
    response_mode: crate::response::ResponseMode,
) -> Option<Value> {
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
            "instructions": format!("RemoteOps proxy using remote protocol v{REMOTE_PROTOCOL_VERSION}. Call environment before choosing a shell or service command; sh_exec accepts explicit shell, cwd and env. Responses default to {}. Set response_mode=full for complete diagnostics (build, limits, network, filesystems); compact lists omitted fields in _response. Commands in batch may modify state; batch is not a transaction.", response_mode.name())
        }),
    ))
}

#[cfg(test)]
fn call_tool(id: Value, params: Option<&Value>, client: &mut RemoteClient) -> Value {
    call_tool_with_response_mode(id, params, client, crate::response::ResponseMode::default())
}

fn call_tool_with_response_mode(
    id: Value,
    params: Option<&Value>,
    client: &mut RemoteClient,
    default_response_mode: crate::response::ResponseMode,
) -> Value {
    let Some(params) = params.and_then(Value::as_object) else {
        return rpc_error(id, -32602, "tools/call params must be an object");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "tool name must be a string");
    };
    let mut arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return rpc_error(id, -32602, "tool arguments must be an object");
    }
    let Some(tool) = Tool::parse(name) else {
        return rpc_error(id, -32602, "unknown tool");
    };
    let response_mode = match crate::response::ResponseMode::take(&mut arguments) {
        Ok(mode) => mode.unwrap_or(default_response_mode),
        Err(error) => return rpc_error(id, -32602, &error.message),
    };

    let before = client.operation_attempts();
    if tool == Tool::Batch {
        let result = batch::prepare(&arguments).map(|batch| batch.execute(client, response_mode));
        return finish_tool_call(
            id,
            name,
            result,
            response_mode,
            client.operation_attempts() > before,
        );
    }

    let result = dispatch::prepare(tool, arguments).and_then(|call| call.execute(client));
    finish_tool_call(
        id,
        name,
        result,
        response_mode,
        client.operation_attempts() > before,
    )
}

fn finish_tool_call(
    id: Value,
    name: &str,
    result: Result<Value, crate::client::ClientError>,
    response_mode: crate::response::ResponseMode,
    request_started: bool,
) -> Value {
    let result = result.and_then(|value| {
        crate::response::render_result(name, value, response_mode)
            .map_err(|error| error.with_request_started(request_started))
    });
    match result {
        Ok(value) => rpc_result(id, value),
        Err(error) if error.is_preflight_error() => {
            let mut response = rpc_error(id, -32602, &error.message);
            response["error"]["data"] = error.details();
            response
        }
        Err(error) => rpc_result(id, crate::response::failed_result(&error)),
    }
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
