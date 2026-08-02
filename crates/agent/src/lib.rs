pub mod error;
pub mod service;
pub mod tools;

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use remote_ops_protocol::{
    DeployActivateRequest, DeployPreflightRequest, SyncFinishRequest, SyncPrepareRequest,
};

use error::{AgentError, AgentResult};
use tools::jobs::JobManager;

pub fn dispatch(
    operation: &str,
    arguments: Value,
    jobs: &JobManager,
    max_transfer_bytes: u64,
) -> AgentResult<Value> {
    match operation {
        "read_text" => {
            let args: ReadTextArgs = decode(arguments)?;
            tools::files::read_text(&args.path, args.offset, args.max_bytes)
        }
        "read_file_lines" => {
            let args: ReadFileLinesArgs = decode(arguments)?;
            tools::files::read_file_lines(
                &args.path,
                args.start_line,
                args.end_line,
                args.max_bytes,
            )
        }
        "tail_text" => {
            let args: TailTextArgs = decode(arguments)?;
            tools::files::tail_text(&args.path, args.lines, args.max_bytes)
        }
        "write_text" => {
            let args: WriteTextArgs = decode(arguments)?;
            tools::files::write_text(&args.path, &args.content)
        }
        "apply_patch" => {
            let args: ApplyPatchArgs = decode(arguments)?;
            tools::files::apply_patch(&args.path, &args.patch, args.expected_sha256.as_deref())
        }
        "list_files" => {
            let args: ListFilesArgs = decode(arguments)?;
            tools::files::list_files(
                &args.path,
                args.cursor.as_deref(),
                args.limit,
                args.recursive,
                args.pattern.as_deref(),
                args.max_depth,
            )
        }
        "grep" => {
            let args: GrepArgs = decode(arguments)?;
            tools::files::grep(
                &args.path,
                &args.pattern,
                args.glob.as_deref(),
                args.case_sensitive,
                args.max_results,
                args.max_file_bytes,
            )
        }
        "stat" => {
            let args: PathArgs = decode(arguments)?;
            tools::files::stat(&args.path)
        }
        "file_hash" => {
            let args: HashArgs = decode(arguments)?;
            tools::files::file_hash(&args.path, args.max_bytes)
        }
        "mkdir" => {
            let args: MkdirArgs = decode(arguments)?;
            tools::file_ops::mkdir(&args.path, args.recursive, args.mode)
        }
        "remove" => {
            let args: RemoveArgs = decode(arguments)?;
            tools::file_ops::remove(&args.path, args.recursive)
        }
        "move" => {
            let args: MoveArgs = decode(arguments)?;
            tools::file_ops::move_path(&args.source, &args.destination, args.overwrite)
        }
        "copy" => {
            let args: CopyArgs = decode(arguments)?;
            tools::file_ops::copy_path(
                &args.source,
                &args.destination,
                args.overwrite,
                args.recursive,
            )
        }
        "chmod" => {
            let args: ChmodArgs = decode(arguments)?;
            tools::file_ops::chmod(&args.path, args.mode)
        }
        "symlink" => {
            let args: SymlinkArgs = decode(arguments)?;
            tools::file_ops::symlink(
                &args.target,
                &args.link_path,
                args.overwrite,
                args.target_kind.as_deref(),
            )
        }
        "sync_prepare" => {
            let args: SyncPrepareRequest = decode(arguments)?;
            tools::deployment::sync_prepare(args)
        }
        "sync_commit" => {
            let args: SyncFinishRequest = decode(arguments)?;
            tools::deployment::sync_commit(args)
        }
        "sync_abort" => {
            let args: SyncFinishRequest = decode(arguments)?;
            tools::deployment::sync_abort(args)
        }
        "deploy_preflight" => {
            let args: DeployPreflightRequest = decode(arguments)?;
            tools::deployment::deploy_preflight(args)
        }
        "deploy_activate" => {
            let args: DeployActivateRequest = decode(arguments)?;
            tools::deployment::deploy_activate(args)
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
        "process_start" => {
            let args: ProcessStartArgs = decode(arguments)?;
            tools::jobs::process_start(
                jobs,
                &args.program,
                &args.args,
                args.cwd.as_deref(),
                &args.env,
                args.timeout_ms,
            )
        }
        "process_output" => {
            let args: ProcessOutputArgs = decode(arguments)?;
            tools::jobs::process_output(
                jobs,
                args.job_id,
                args.stdout_cursor,
                args.stderr_cursor,
                args.max_bytes,
            )
        }
        "process_wait" => {
            let args: ProcessWaitArgs = decode(arguments)?;
            tools::jobs::process_wait(jobs, args.job_id, args.wait_ms)
        }
        "process_signal" => {
            let args: ProcessSignalArgs = decode(arguments)?;
            tools::jobs::process_signal(jobs, args.job_id, args.signal)
        }
        "process_close" => {
            let args: JobIdArgs = decode(arguments)?;
            tools::jobs::process_close(jobs, args.job_id)
        }
        "system_info" => {
            let _: EmptyArgs = decode(arguments)?;
            tools::system::system_info()
        }
        "agent_info" => {
            let _: EmptyArgs = decode(arguments)?;
            Ok(tools::lifecycle::agent_info(max_transfer_bytes))
        }
        "reboot" => {
            let args: RebootArgs = decode(arguments)?;
            tools::lifecycle::schedule_reboot(args.delay_ms, max_transfer_bytes)
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
fn default_line_bytes() -> usize {
    256 * 1024
}
fn default_lines() -> usize {
    100
}
fn default_list_limit() -> usize {
    200
}
fn default_list_depth() -> usize {
    16
}
fn default_grep_results() -> usize {
    200
}
fn default_grep_file_bytes() -> u64 {
    1024 * 1024
}
fn default_true() -> bool {
    true
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
fn default_process_job_timeout() -> u64 {
    remote_ops_protocol::DEFAULT_PROCESS_JOB_TIMEOUT_MS
}
fn default_process_output_bytes() -> usize {
    remote_ops_protocol::DEFAULT_PROCESS_OUTPUT_BYTES
}
fn default_process_wait() -> u64 {
    remote_ops_protocol::DEFAULT_PROCESS_WAIT_MS
}

fn default_reboot_delay() -> u64 {
    remote_ops_protocol::DEFAULT_REBOOT_DELAY_MS
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
struct ReadFileLinesArgs {
    path: String,
    #[serde(default = "default_start_line")]
    start_line: u64,
    end_line: Option<u64>,
    #[serde(default = "default_line_bytes")]
    max_bytes: usize,
}

fn default_start_line() -> u64 {
    1
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
struct ApplyPatchArgs {
    path: String,
    patch: String,
    expected_sha256: Option<String>,
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
struct MkdirArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
    mode: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    source: String,
    destination: String,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyArgs {
    source: String,
    destination: String,
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    recursive: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChmodArgs {
    path: String,
    mode: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SymlinkArgs {
    target: String,
    link_path: String,
    #[serde(default)]
    overwrite: bool,
    target_kind: Option<String>,
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
struct RebootArgs {
    #[serde(default = "default_reboot_delay")]
    delay_ms: u64,
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
        let jobs = JobManager::new();
        let error = dispatch(
            "stat",
            json!({"path": ".", "extra": true}),
            &jobs,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .unwrap_err();
        assert_eq!(error.kind, "invalid_params");
    }

    #[test]
    fn file_tools_operate_on_binary_safe_paths() {
        let jobs = JobManager::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        dispatch(
            "write_text",
            json!({"path": path, "content": "hello"}),
            &jobs,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .unwrap();
        let result = dispatch(
            "read_text",
            json!({"path": path}),
            &jobs,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .unwrap();
        assert_eq!(result["text"], "hello");
    }

    #[test]
    fn apply_patch_rejects_unknown_arguments() {
        let jobs = JobManager::new();
        let error = dispatch(
            "apply_patch",
            json!({"path":"file.txt","patch":"invalid","extra":true}),
            &jobs,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .unwrap_err();
        assert_eq!(error.kind, "invalid_params");
    }

    #[test]
    fn dispatches_new_file_discovery_tools_and_rejects_old_ls_name() {
        let jobs = JobManager::new();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.rs");
        std::fs::write(&path, "one\nneedle\nthree\n").unwrap();

        let lines = dispatch(
            "read_file_lines",
            json!({"path": path, "start_line": 2, "end_line": 2}),
            &jobs,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .unwrap();
        assert_eq!(lines["text"], "needle\n");

        let search = dispatch(
            "grep",
            json!({"path": directory.path(), "pattern": "needle"}),
            &jobs,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .unwrap();
        assert_eq!(search["matches"].as_array().unwrap().len(), 1);

        let listing = dispatch(
            "list_files",
            json!({"path": directory.path(), "pattern": "*.rs"}),
            &jobs,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .unwrap();
        assert_eq!(listing["entries"][0]["name"], "source.rs");

        let error = dispatch(
            "ls",
            json!({"path": directory.path()}),
            &jobs,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .unwrap_err();
        assert_eq!(error.kind, "invalid_params");
        assert!(error.message.contains("unknown operation"));
    }

    #[test]
    fn background_process_arguments_are_validated_on_agent() {
        let jobs = JobManager::new();
        let invalid_calls = [
            ("process_start", json!({"program":"","timeout_ms":1})),
            ("process_start", json!({"program":"ok","timeout_ms":0})),
            (
                "process_start",
                json!({"program":"ok","timeout_ms":remote_ops_protocol::MAX_PROCESS_JOB_TIMEOUT_MS + 1}),
            ),
            ("process_output", json!({"job_id":0})),
            (
                "process_output",
                json!({"job_id":1,"max_bytes":remote_ops_protocol::MAX_PROCESS_OUTPUT_BYTES + 1}),
            ),
            (
                "process_wait",
                json!({"job_id":1,"wait_ms":remote_ops_protocol::MAX_PROCESS_WAIT_MS + 1}),
            ),
            ("process_signal", json!({"job_id":1,"signal":0})),
            ("process_close", json!({"job_id":0})),
        ];
        for (operation, arguments) in invalid_calls {
            let error = dispatch(
                operation,
                arguments,
                &jobs,
                remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
            )
            .unwrap_err();
            assert_eq!(error.kind, "invalid_params", "{operation}");
        }

        let error = dispatch(
            "process_output",
            json!({"job_id":1,"extra":true}),
            &jobs,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .unwrap_err();
        assert_eq!(error.kind, "invalid_params");
    }

    #[test]
    fn deployment_file_arguments_are_validated_on_agent() {
        let jobs = JobManager::new();
        let invalid_calls = [
            ("mkdir", json!({"path":"","mode":0})),
            ("mkdir", json!({"path":"path","mode":4096})),
            ("remove", json!({"path":""})),
            ("move", json!({"source":"same","destination":"same"})),
            ("copy", json!({"source":"same","destination":"same"})),
            ("chmod", json!({"path":"path","mode":4096})),
            (
                "symlink",
                json!({"target":"a","link_path":"b","target_kind":"other"}),
            ),
            (
                "sync_prepare",
                json!({
                    "remote_path":"target",
                    "manifest_sha256":"0".repeat(64),
                    "entries":[],
                    "max_files":0,
                    "max_total_bytes":0,
                    "max_depth":1
                }),
            ),
        ];
        for (operation, arguments) in invalid_calls {
            let error = dispatch(
                operation,
                arguments,
                &jobs,
                remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
            )
            .unwrap_err();
            assert_eq!(error.kind, "invalid_params", "{operation}");
        }
    }
}
