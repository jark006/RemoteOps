pub mod error;
pub mod service;
pub mod tools;

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use error::{AgentError, AgentResult};

pub fn dispatch(operation: &str, arguments: Value) -> AgentResult<Value> {
    match operation {
        "read_text" => {
            let args: ReadTextArgs = decode(arguments)?;
            tools::files::read_text(&args.path, args.offset, args.max_bytes)
        }
        "tail_text" => {
            let args: TailTextArgs = decode(arguments)?;
            tools::files::tail_text(&args.path, args.lines, args.max_bytes)
        }
        "write_text" => {
            let args: WriteTextArgs = decode(arguments)?;
            tools::files::write_text(&args.path, &args.content)
        }
        "ls" => {
            let args: ListArgs = decode(arguments)?;
            tools::files::list_dir(&args.path, args.cursor.as_deref(), args.limit)
        }
        "stat" => {
            let args: PathArgs = decode(arguments)?;
            tools::files::stat(&args.path)
        }
        "file_hash" => {
            let args: HashArgs = decode(arguments)?;
            tools::files::file_hash(&args.path, args.max_bytes)
        }
        "pids" => {
            let args: PidsArgs = decode(arguments)?;
            tools::process::pids(args.filter.as_deref(), args.cursor.as_deref(), args.limit)
        }
        "process_info" => {
            let args: PidArgs = decode(arguments)?;
            tools::process::process_info(args.pid)
        }
        "kill" => {
            let args: KillArgs = decode(arguments)?;
            tools::process::kill(args.pid, args.signal)
        }
        "pkill" => {
            let args: PkillArgs = decode(arguments)?;
            tools::process::pkill(&args.name, args.signal)
        }
        "sh_exec" => {
            let args: ShArgs = decode(arguments)?;
            tools::command::sh_exec(&args.command, args.timeout_ms)
        }
        "exec" => {
            let args: ExecArgs = decode(arguments)?;
            tools::command::exec(
                &args.program,
                &args.args,
                args.cwd.as_deref(),
                &args.env,
                args.timeout_ms,
            )
        }
        "system_info" => {
            let _: EmptyArgs = decode(arguments)?;
            tools::system::system_info()
        }
        _ => Err(AgentError::invalid(format!(
            "unknown operation: {operation}"
        ))),
    }
}

fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> AgentResult<T> {
    serde_json::from_value(value).map_err(|err| AgentError::invalid(err.to_string()))
}

fn default_read_bytes() -> usize {
    1024 * 1024
}
fn default_tail_bytes() -> usize {
    256 * 1024
}
fn default_lines() -> usize {
    100
}
fn default_list_limit() -> usize {
    200
}
fn default_pid_limit() -> usize {
    1024
}
fn default_timeout() -> u64 {
    tools::command::DEFAULT_TIMEOUT_MS
}
fn default_signal() -> i32 {
    15
}
fn default_hash_bytes() -> u64 {
    tools::files::FILE_HASH_MAX_BYTES
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadTextArgs {
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_read_bytes")]
    max_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TailTextArgs {
    path: String,
    #[serde(default = "default_lines")]
    lines: usize,
    #[serde(default = "default_tail_bytes")]
    max_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteTextArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path: String,
    cursor: Option<String>,
    #[serde(default = "default_list_limit")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HashArgs {
    path: String,
    #[serde(default = "default_hash_bytes")]
    max_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PidsArgs {
    filter: Option<String>,
    cursor: Option<String>,
    #[serde(default = "default_pid_limit")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PidArgs {
    pid: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KillArgs {
    pid: i32,
    #[serde(default = "default_signal")]
    signal: i32,
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
struct ShArgs {
    command: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_unknown_arguments() {
        let error = dispatch("stat", json!({"path": ".", "extra": true})).unwrap_err();
        assert_eq!(error.kind, "invalid_params");
    }

    #[test]
    fn file_tools_operate_on_binary_safe_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        dispatch("write_text", json!({"path": path, "content": "hello"})).unwrap();
        let result = dispatch("read_text", json!({"path": path})).unwrap();
        assert_eq!(result["text"], "hello");
    }
}
