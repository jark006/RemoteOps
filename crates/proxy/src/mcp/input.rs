use super::catalog::Tool;
use super::parameters::*;
use remote_ops_protocol::*;
use serde_json::{Map, Value, json};

pub(super) fn definition(tool_id: Tool) -> Value {
    match tool_id {
        Tool::ReadText => tool(
            tool_id,
            "Read text",
            "Read bounded text from a remote file (max 1 MiB per call). Truncation is reported via metadata.truncated and next_offset; continue from metadata.next_offset to read more.",
            props(&[
                ("path", string_prop("Remote file path")),
                ("offset", integer_prop_default(0, None, 0, "Byte offset")),
                (
                    "max_bytes",
                    integer_prop_default(0, Some(1_048_576), 1_048_576, "Maximum bytes"),
                ),
            ]),
            &["path"],
        ),
        Tool::ReadFileLines => tool(
            tool_id,
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
        ),
        Tool::TailText => tool(
            tool_id,
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
        ),
        Tool::WriteText => tool(
            tool_id,
            "Write text",
            "Atomically write a remote text file",
            props(&[
                ("path", string_prop("Remote file path")),
                ("content", string_prop("Text content")),
            ]),
            &["path", "content"],
        ),
        Tool::ApplyPatch => tool(
            tool_id,
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
        ),
        Tool::ListFiles => tool(
            tool_id,
            "List files",
            "List, filter, and optionally recurse through a remote directory with sorted cursor pagination. Entries include name, kind, size, mtime (epoch seconds), mtime_iso (RFC 3339 local time), and mode_str (ls-style permissions).",
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
        ),
        Tool::Grep => tool(
            tool_id,
            "Search text",
            "Search regular UTF-8 files with a Rust regular expression. Matches include path, 1-based line and column, and the matched line text; result reports files_scanned/skipped and truncated.",
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
        ),
        Tool::Stat => tool(
            tool_id,
            "File status",
            "Read remote path metadata without following symlinks. Returns size (bytes), mtime (epoch seconds), mtime_iso (RFC 3339 local time), mode (raw st_mode; 0 when the platform has no Unix mode), mode_str (ls-style permissions), and kind (file/dir/symlink/other).",
            props(&[("path", string_prop("Remote path"))]),
            &["path"],
        ),
        Tool::FileHash => tool(
            tool_id,
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
        ),
        Tool::Mkdir => tool(
            tool_id,
            "Create directory",
            "Create one remote directory, with explicit recursive parent creation and optional Unix mode",
            props(&[
                ("path", string_prop("Exact remote directory path")),
                ("recursive", json!({"type":"boolean","default":false})),
                (
                    "mode",
                    integer_prop(
                        0,
                        Some(MAX_UNIX_MODE as u64),
                        "Optional Unix permission mode",
                    ),
                ),
            ]),
            &["path"],
        ),
        Tool::Remove => tool(
            tool_id,
            "Remove path",
            "Remove one exact remote regular file, symlink, empty directory, or explicitly recursive directory",
            props(&[
                ("path", string_prop("Exact remote path")),
                ("recursive", json!({"type":"boolean","default":false})),
            ]),
            &["path"],
        ),
        Tool::Move => tool(
            tool_id,
            "Move path",
            "Move one remote path without crossing filesystems; overwrite is limited to non-directories",
            props(&[
                ("source", string_prop("Exact remote source path")),
                ("destination", string_prop("Exact remote destination path")),
                ("overwrite", json!({"type":"boolean","default":false})),
            ]),
            &["source", "destination"],
        ),
        Tool::Copy => tool(
            tool_id,
            "Copy path",
            "Copy a remote regular file or an explicitly recursive directory without following symlinks",
            props(&[
                ("source", string_prop("Exact remote source path")),
                ("destination", string_prop("Exact remote destination path")),
                ("overwrite", json!({"type":"boolean","default":false})),
                ("recursive", json!({"type":"boolean","default":false})),
            ]),
            &["source", "destination"],
        ),
        Tool::Chmod => tool(
            tool_id,
            "Change mode",
            "Set a Unix permission mode on one remote regular file or directory without following symlinks",
            props(&[
                ("path", string_prop("Exact remote path")),
                (
                    "mode",
                    integer_prop(0, Some(MAX_UNIX_MODE as u64), "Unix permission mode"),
                ),
            ]),
            &["path", "mode"],
        ),
        Tool::Symlink => tool(
            tool_id,
            "Create symbolic link",
            "Create or atomically replace one remote symbolic link; Windows requires target_kind",
            props(&[
                (
                    "target",
                    string_prop("Stored link target, which may be relative"),
                ),
                ("link_path", string_prop("Exact remote link path")),
                ("overwrite", json!({"type":"boolean","default":false})),
                (
                    "target_kind",
                    json!({"type":"string","enum":["file","dir"],"description":"Required on Windows"}),
                ),
            ]),
            &["target", "link_path"],
        ),
        Tool::SyncDirectory => tool(
            tool_id,
            "Sync directory",
            "Synchronize a proxy-local directory to a verified remote staging tree, upload changed files only, then commit with a retained backup",
            props(&[
                ("local_path", string_prop("Directory path on the proxy PC")),
                (
                    "remote_path",
                    string_prop("Exact destination directory on the remote"),
                ),
                (
                    "excludes",
                    json!({"type":"array","maxItems":MAX_SYNC_EXCLUDE_PATTERNS,"items":{"type":"string","minLength":1,"maxLength":MAX_SYNC_GLOB_BYTES},"default":[]}),
                ),
                (
                    "max_files",
                    integer_prop_default(
                        1,
                        Some(MAX_SYNC_FILES as u64),
                        DEFAULT_SYNC_MAX_FILES as u64,
                        "Maximum manifest entries",
                    ),
                ),
                (
                    "max_total_bytes",
                    integer_prop_default(
                        0,
                        Some(DEFAULT_MAX_TRANSFER_BYTES),
                        DEFAULT_MAX_TRANSFER_BYTES,
                        "Maximum total regular-file bytes",
                    ),
                ),
                (
                    "max_depth",
                    integer_prop_default(
                        1,
                        Some(MAX_SYNC_DEPTH as u64),
                        DEFAULT_SYNC_MAX_DEPTH as u64,
                        "Maximum relative directory depth",
                    ),
                ),
            ]),
            &["local_path", "remote_path"],
        ),
        Tool::DeployRelease => tool(
            tool_id,
            "Deploy release",
            "Preflight, stage an independent release, atomically switch the current symlink, start and health-check it, and roll back on failure",
            props(&[
                (
                    "local_path",
                    string_prop("Release directory on the proxy PC"),
                ),
                (
                    "releases_path",
                    string_prop("Remote parent directory containing releases"),
                ),
                (
                    "current_path",
                    string_prop("Remote symlink switched atomically"),
                ),
                (
                    "release_id",
                    json!({"type":"string","minLength":1,"maxLength":MAX_RELEASE_ID_BYTES,"pattern":"^[A-Za-z0-9._-]+$"}),
                ),
                (
                    "expected_arch",
                    string_prop("Optional required remote architecture"),
                ),
                (
                    "min_free_bytes",
                    integer_prop_default(
                        0,
                        None,
                        0,
                        "Free bytes retained in addition to release size",
                    ),
                ),
                (
                    "dependencies",
                    json!({"type":"array","maxItems":MAX_DEPLOY_DEPENDENCIES,"items":{"type":"string","minLength":1,"maxLength":1024},"default":[]}),
                ),
                ("stop", deploy_command_schema()),
                ("start", deploy_command_schema()),
                ("health", deploy_command_schema()),
                ("rollback_start", deploy_command_schema()),
                (
                    "excludes",
                    json!({"type":"array","maxItems":MAX_SYNC_EXCLUDE_PATTERNS,"items":{"type":"string","minLength":1,"maxLength":MAX_SYNC_GLOB_BYTES},"default":[]}),
                ),
                (
                    "max_files",
                    integer_prop_default(
                        1,
                        Some(MAX_SYNC_FILES as u64),
                        DEFAULT_SYNC_MAX_FILES as u64,
                        "Maximum manifest entries",
                    ),
                ),
                (
                    "max_total_bytes",
                    integer_prop_default(
                        0,
                        Some(DEFAULT_MAX_TRANSFER_BYTES),
                        DEFAULT_MAX_TRANSFER_BYTES,
                        "Maximum total regular-file bytes",
                    ),
                ),
                (
                    "max_depth",
                    integer_prop_default(
                        1,
                        Some(MAX_SYNC_DEPTH as u64),
                        DEFAULT_SYNC_MAX_DEPTH as u64,
                        "Maximum relative directory depth",
                    ),
                ),
            ]),
            &[
                "local_path",
                "releases_path",
                "current_path",
                "release_id",
                "start",
                "health",
            ],
        ),
        Tool::Pids => tool(
            tool_id,
            "List processes",
            "List Linux, Windows, or macOS processes with bounded PID pagination. Follow next_cursor even for an empty filtered page. Command lines report availability/truncation; scan.filter_incomplete warns that hidden or clipped command lines may hide filter matches.",
            props(&[
                ("filter", string_prop("Name or command substring")),
                ("cursor", string_prop("Exclusive PID cursor")),
                (
                    "limit",
                    integer_prop_default(1, Some(1024), 1024, "Maximum processes"),
                ),
            ]),
            &[],
        ),
        Tool::ProcessInfo => tool(
            tool_id,
            "Process information",
            "Read bounded Linux, Windows, or macOS process details: cmdline, state, uid, memory, start_time_seconds (seconds since boot) and start_time_iso (wall-clock RFC 3339 local time).",
            props(&[("pid", integer_prop(1, None, "Process ID"))]),
            &["pid"],
        ),
        Tool::Kill => tool(
            tool_id,
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
        ),
        Tool::Pkill => tool(
            tool_id,
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
        ),
        Tool::ShExec => tool(
            tool_id,
            "Run shell command",
            "Run a command in an explicit shell; inspect environment first. native uses sh on Unix, pwsh then Windows PowerShell on Windows. Other choices never fall back. Returns actual shell, stdout/stderr, exit code, timeout, duration and truncation flags.",
            props(&[
                (
                    "command",
                    json!({"type":"string","maxLength":remote_ops_protocol::execution::MAX_SHELL_COMMAND_BYTES,"description":"Shell script, at most 256 KiB UTF-8 without NUL; cmd additionally allows at most 8000 UTF-16 units"}),
                ),
                (
                    "shell",
                    json!({"type":"string","enum":remote_ops_protocol::ShellKind::ALL,"default":"native","description":"Explicit interpreter. pwsh is PowerShell 7; powershell is Windows PowerShell 5.1; busybox runs its sh applet."}),
                ),
                (
                    "cwd",
                    json!({"type":"string","minLength":1,"maxLength":remote_ops_protocol::execution::MAX_EXECUTION_PATH_BYTES,"description":"Working directory, at most 4096 UTF-8 bytes"}),
                ),
                (
                    "env",
                    json!({"type":"object","maxProperties":remote_ops_protocol::execution::MAX_SHELL_ENV_ENTRIES,"additionalProperties":{"type":"string"},"description":"Environment overrides, at most 64 KiB total UTF-8 name/value bytes; shell startup hooks are disabled"}),
                ),
                (
                    "timeout_ms",
                    integer_prop_default(0, Some(300_000), 10_000, "Timeout in milliseconds"),
                ),
            ]),
            &["command"],
        ),
        Tool::Exec => tool(
            tool_id,
            "Run program",
            "Run a remote program without a shell. Returns stdout, stderr, exit_code, timed_out, duration_ms, and truncation flags.",
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
        ),
        Tool::ProcessStart => tool(
            tool_id,
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
        ),
        Tool::ProcessOutput => tool(
            tool_id,
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
        ),
        Tool::ProcessWait => tool(
            tool_id,
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
        ),
        Tool::ProcessSignal => tool(
            tool_id,
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
        ),
        Tool::ProcessClose => tool(
            tool_id,
            "Close background job",
            "Release a finished background job and its retained output",
            props(&[("job_id", integer_prop(1, None, "Background job ID"))]),
            &["job_id"],
        ),
        Tool::SystemInfo => tool(
            tool_id,
            "System information",
            "Read bounded system information",
            props(&[]),
            &[],
        ),
        Tool::Environment => tool(
            tool_id,
            "Inspect execution environment",
            "Discover actual shell paths, native shell selection, PID 1, service utilities, cwd and execution capabilities without running commands. File detection does not verify a shell, BusyBox applet or service manager works. No embedded/server classification is guessed.",
            props(&[]),
            &[],
        ),
        Tool::UploadFile => tool(
            tool_id,
            "Upload file",
            "Transfer one binary file from the proxy PC to the remote with optional Unix mode and verified resume",
            props(&[
                ("local_path", string_prop("Path on the proxy PC")),
                ("remote_path", string_prop("Destination path on the remote")),
                ("overwrite", json!({"type":"boolean","default":true})),
                (
                    "mode",
                    integer_prop(
                        0,
                        Some(MAX_UNIX_MODE as u64),
                        "Optional Unix permission mode",
                    ),
                ),
                ("resume", json!({"type":"boolean","default":false})),
            ]),
            &["local_path", "remote_path"],
        ),
        Tool::DownloadFile => tool(
            tool_id,
            "Download file",
            "Transfer one binary file from the remote to the proxy PC with verified resume",
            props(&[
                ("remote_path", string_prop("Path on the remote")),
                (
                    "local_path",
                    string_prop("Destination path on the proxy PC"),
                ),
                ("overwrite", json!({"type":"boolean","default":true})),
                ("resume", json!({"type":"boolean","default":false})),
            ]),
            &["remote_path", "local_path"],
        ),
        Tool::RemoteStatus => tool(
            tool_id,
            "Remote status",
            "Read the configured remote and cached connection state without connecting",
            props(&[]),
            &[],
        ),
        Tool::SetRemote => tool(
            tool_id,
            "Set remote",
            "Change the remote IPv4 address, port, or both for subsequent calls; at least one must be provided. Always drops the cached session so the next call reconnects, including when the address is unchanged",
            props(&[
                (
                    "ip",
                    json!({"type":"string","format":"ipv4","description":"Remote IPv4 address"}),
                ),
                ("port", integer_prop(1, Some(65_535), "Remote TCP port")),
            ]),
            &[],
        ),
        Tool::AgentInfo => tool(
            tool_id,
            "Agent information",
            "Read the remote Agent version, build, runtime identity, capabilities, and limits",
            props(&[]),
            &[],
        ),
        Tool::RemoteProbe => tool(
            tool_id,
            "Probe remote",
            "Actively connect or health-check the configured remote and return latency and Agent information. Reconnects within the probe timeout when a cached session is stale or half open",
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
        ),
        Tool::WaitRemote => tool(
            tool_id,
            "Wait for remote",
            "Wait for the remote to become offline, online, or cycle offline then online with a healthy Agent. offline_then_online also completes when the Agent instance changes, even without an observed outage",
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
        ),
        Tool::Reboot => tool(
            tool_id,
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
        ),
        Tool::AgentUpdate => tool(
            tool_id,
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
        ),
        Tool::Batch => tool(
            tool_id,
            "Batch tool calls",
            "Run 1..=16 calls sequentially after validating all arguments. Results keep input order as {tool, ok, result|error}. Ordinary remote errors stay per item; uncertain transport/protocol, busy-manager or unavailable-output errors stop remaining calls as not_executed. Commands can mutate state; no transaction or rollback. Children inherit response_mode.",
            props(&[(
                "calls",
                json!({
                    "type":"array",
                    "minItems":1,
                    "maxItems":super::batch::MAX_CALLS,
                    "description":"Tool calls executed in order; each entry is {tool, arguments}",
                    "items":{
                        "type":"object",
                        "properties":{
                            "tool":{"type":"string","enum":Tool::batch_names()},
                            "arguments":{"type":"object","default":{},"description":"Arguments object forwarded to the tool"}
                        },
                        "required":["tool"],
                        "additionalProperties":false
                    }
                }),
            )]),
            &["calls"],
        ),
    }
}

fn tool(
    tool_id: Tool,
    title: &str,
    description: &str,
    mut properties: Map<String, Value>,
    required: &[&str],
) -> Value {
    let name = tool_id.name();
    properties.insert(
        "response_mode".to_string(),
        crate::response::mode_schema(crate::response::ResponseMode::default()),
    );
    let input_schema = json!({
        "type":"object",
        "properties":properties,
        "required":required,
        "additionalProperties":false
    });
    json!({
        "name": name, "title": title, "description": description,
        "inputSchema": input_schema,
        "outputSchema": super::output::output_schema(tool_id),
        "annotations": tool_id.annotations()
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

fn deploy_command_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "program":{"type":"string","minLength":1},
            "args":{"type":"array","items":{"type":"string"},"default":[]},
            "cwd":{"type":"string","minLength":1},
            "env":{"type":"object","additionalProperties":{"type":"string"},"default":{}},
            "timeout_ms":{"type":"integer","minimum":0,"maximum":300000,"default":10000}
        },
        "required":["program"],
        "additionalProperties":false
    })
}
