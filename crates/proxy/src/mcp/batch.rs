use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    catalog::Tool,
    dispatch::{self, PreparedCall},
    parameters::{decode_transfer, invalid_params},
};
use crate::{
    client::{ClientError, RemoteClient},
    response::{self, ResponseMode},
};

pub(super) const MAX_CALLS: usize = 16;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchArgs {
    calls: Vec<BatchCall>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchCall {
    tool: String,
    #[serde(default = "empty_arguments")]
    arguments: Value,
}

fn empty_arguments() -> Value {
    json!({})
}

struct Child {
    tool: Tool,
    call: PreparedCall,
    response_mode: Option<ResponseMode>,
}

pub(super) struct PreparedBatch {
    calls: Vec<Child>,
}

pub(super) fn prepare(arguments: &Value) -> Result<PreparedBatch, ClientError> {
    let args: BatchArgs = decode_transfer(arguments)?;
    if args.calls.is_empty() || args.calls.len() > MAX_CALLS {
        return invalid_params(format!("calls must contain 1..={MAX_CALLS} entries"));
    }
    let mut calls = Vec::with_capacity(args.calls.len());
    for mut call in args.calls {
        let tool = Tool::parse(&call.tool).filter(|tool|tool.batch_allowed()).ok_or_else(||ClientError::local("invalid_params",format!("tool {:?} is not allowed in batch; use the advertised inspection and command tool allowlist",call.tool)))?;
        if !call.arguments.is_object() {
            return invalid_params(format!(
                "arguments for tool {:?} must be an object",
                call.tool
            ));
        }
        let response_mode = ResponseMode::take(&mut call.arguments)?;
        let prepared = dispatch::prepare(tool, call.arguments)?;
        calls.push(Child {
            tool,
            call: prepared,
            response_mode,
        });
    }
    Ok(PreparedBatch { calls })
}

impl PreparedBatch {
    pub(super) fn execute(self, client: &mut RemoteClient, response_mode: ResponseMode) -> Value {
        let mut results = Vec::with_capacity(self.calls.len());
        let mut stopped_after = None;
        for (index, child) in self.calls.into_iter().enumerate() {
            let entry = if let Some(prior) = stopped_after {
                let error=ClientError::local("not_executed",format!("batch stopped after item {prior}; inspect its outcome before sending further requests")).with_recovery(json!({"stopped_after":prior}));
                json!({"tool":child.tool.name(),"ok":false,"error":error.details()})
            } else {
                match child.call.execute(client) {
                    Ok(value) => {
                        json!({"tool":child.tool.name(),"ok":true,"result":response::project(child.tool.name(),value,child.response_mode.unwrap_or(response_mode))})
                    }
                    Err(error) => {
                        if error.halts_workflow() {
                            stopped_after = Some(index + 1);
                        }
                        json!({"tool":child.tool.name(),"ok":false,"error":error.details()})
                    }
                }
            };
            results.push(entry);
        }
        json!({"results":results})
    }
}
