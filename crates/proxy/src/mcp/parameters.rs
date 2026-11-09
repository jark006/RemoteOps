use crate::client::ClientError;
use crate::deployment::{DeployOptions, SyncOptions};
use globset::Glob;
use regex::RegexBuilder;
pub(super) use remote_ops_protocol::arguments::{
    HashArgs, KillArgs, PathArgs, PidArgs, PidsArgs, ReadTextArgs, TailTextArgs, ValidateArguments,
    WriteTextArgs,
};
pub(super) use remote_ops_protocol::execution::ExecArgs;
use remote_ops_protocol::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;

pub(super) fn decode_transfer<T: for<'de> Deserialize<'de>>(
    value: &Value,
) -> Result<T, ClientError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ClientError::local("invalid_params", error.to_string()))
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn default_signal() -> i32 {
    15
}

pub(super) const PKILL_MAX_NAME_CHARS: usize = 260;
pub(super) const READ_FILE_LINES_MAX_BYTES: usize = 1024 * 1024;
pub(super) const READ_FILE_LINES_MAX_LINES: u64 = 10_000;
pub(super) const GREP_MAX_PATTERN_BYTES: usize = 4 * 1024;
pub(super) const MAX_GLOB_BYTES: usize = 1024;
pub(super) const GREP_MAX_RESULTS: usize = 1000;
pub(super) const GREP_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const LIST_FILES_MAX_DEPTH: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UploadArgs {
    pub(super) local_path: String,
    pub(super) remote_path: String,
    #[serde(default = "default_true")]
    pub(super) overwrite: bool,
    pub(super) mode: Option<u32>,
    #[serde(default)]
    pub(super) resume: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DownloadArgs {
    pub(super) remote_path: String,
    pub(super) local_path: String,
    #[serde(default = "default_true")]
    pub(super) overwrite: bool,
    #[serde(default)]
    pub(super) resume: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MkdirArgs {
    pub(super) path: String,
    #[serde(default)]
    pub(super) recursive: bool,
    pub(super) mode: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveArgs {
    pub(super) path: String,
    #[serde(default)]
    pub(super) recursive: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MoveArgs {
    pub(super) source: String,
    pub(super) destination: String,
    #[serde(default)]
    pub(super) overwrite: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CopyArgs {
    pub(super) source: String,
    pub(super) destination: String,
    #[serde(default)]
    pub(super) overwrite: bool,
    #[serde(default)]
    pub(super) recursive: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChmodArgs {
    pub(super) path: String,
    pub(super) mode: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SymlinkArgs {
    pub(super) target: String,
    pub(super) link_path: String,
    #[serde(default)]
    pub(super) overwrite: bool,
    pub(super) target_kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SyncDirectoryArgs {
    pub(super) local_path: String,
    pub(super) remote_path: String,
    #[serde(default)]
    pub(super) excludes: Vec<String>,
    #[serde(default = "default_sync_max_files")]
    pub(super) max_files: usize,
    #[serde(default = "default_sync_max_total_bytes")]
    pub(super) max_total_bytes: u64,
    #[serde(default = "default_sync_max_depth")]
    pub(super) max_depth: usize,
}

impl SyncDirectoryArgs {
    pub(super) fn sync_options(&self) -> SyncOptions {
        SyncOptions {
            excludes: self.excludes.clone(),
            max_files: self.max_files,
            max_total_bytes: self.max_total_bytes,
            max_depth: self.max_depth,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeployCommandArgs {
    pub(super) program: String,
    #[serde(default)]
    pub(super) args: Vec<String>,
    pub(super) cwd: Option<String>,
    #[serde(default)]
    pub(super) env: BTreeMap<String, String>,
    #[serde(default = "default_command_timeout")]
    pub(super) timeout_ms: u64,
}

impl DeployCommandArgs {
    pub(super) fn into_spec(self) -> CommandSpec {
        CommandSpec {
            program: self.program,
            args: self.args,
            cwd: self.cwd,
            env: self.env,
            timeout_ms: self.timeout_ms,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeployReleaseArgs {
    pub(super) local_path: String,
    pub(super) releases_path: String,
    pub(super) current_path: String,
    pub(super) release_id: String,
    pub(super) expected_arch: Option<String>,
    #[serde(default)]
    pub(super) min_free_bytes: u64,
    #[serde(default)]
    pub(super) dependencies: Vec<String>,
    pub(super) stop: Option<DeployCommandArgs>,
    pub(super) start: DeployCommandArgs,
    pub(super) health: DeployCommandArgs,
    pub(super) rollback_start: Option<DeployCommandArgs>,
    #[serde(default)]
    pub(super) excludes: Vec<String>,
    #[serde(default = "default_sync_max_files")]
    pub(super) max_files: usize,
    #[serde(default = "default_sync_max_total_bytes")]
    pub(super) max_total_bytes: u64,
    #[serde(default = "default_sync_max_depth")]
    pub(super) max_depth: usize,
}

impl DeployReleaseArgs {
    pub(super) fn into_options(self) -> DeployOptions {
        DeployOptions {
            local_path: self.local_path,
            releases_path: self.releases_path,
            current_path: self.current_path,
            release_id: self.release_id,
            expected_arch: self.expected_arch,
            min_free_bytes: self.min_free_bytes,
            dependencies: self.dependencies,
            stop: self.stop.map(DeployCommandArgs::into_spec),
            start: self.start.into_spec(),
            health: self.health.into_spec(),
            rollback_start: self.rollback_start.map(DeployCommandArgs::into_spec),
            sync: SyncOptions {
                excludes: self.excludes,
                max_files: self.max_files,
                max_total_bytes: self.max_total_bytes,
                max_depth: self.max_depth,
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetRemoteArgs {
    pub(super) ip: Option<Ipv4Addr>,
    pub(super) port: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoteProbeArgs {
    #[serde(default = "default_remote_probe_timeout")]
    pub(super) timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitRemoteArgs {
    pub(super) wait_for: Option<String>,
    #[serde(default = "default_wait_remote_timeout")]
    pub(super) timeout_ms: u64,
    #[serde(default = "default_wait_remote_poll")]
    pub(super) poll_interval_ms: u64,
    #[serde(default = "default_remote_probe_timeout")]
    pub(super) probe_timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RebootArgs {
    #[serde(default = "default_reboot_delay")]
    pub(super) delay_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentUpdateArgs {
    pub(super) local_path: String,
    #[serde(default = "default_wait_remote_timeout")]
    pub(super) timeout_ms: u64,
    #[serde(default = "default_wait_remote_poll")]
    pub(super) poll_interval_ms: u64,
    #[serde(default = "default_remote_probe_timeout")]
    pub(super) probe_timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PkillArgs {
    pub(super) name: String,
    #[serde(default = "default_signal")]
    pub(super) signal: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProcessStartArgs {
    pub(super) program: String,
    #[serde(default)]
    pub(super) args: Vec<String>,
    pub(super) cwd: Option<String>,
    #[serde(default)]
    pub(super) env: BTreeMap<String, String>,
    #[serde(default = "default_process_job_timeout")]
    pub(super) timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProcessOutputArgs {
    pub(super) job_id: u64,
    #[serde(default)]
    pub(super) stdout_cursor: u64,
    #[serde(default)]
    pub(super) stderr_cursor: u64,
    #[serde(default = "default_process_output_bytes")]
    pub(super) max_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProcessWaitArgs {
    pub(super) job_id: u64,
    #[serde(default = "default_process_wait")]
    pub(super) wait_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProcessSignalArgs {
    pub(super) job_id: u64,
    #[serde(default = "default_signal")]
    pub(super) signal: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JobIdArgs {
    pub(super) job_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApplyPatchArgs {
    pub(super) path: String,
    pub(super) patch: String,
    pub(super) expected_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadFileLinesArgs {
    pub(super) path: String,
    #[serde(default = "default_start_line")]
    pub(super) start_line: u64,
    pub(super) end_line: Option<u64>,
    #[serde(default = "default_line_bytes")]
    pub(super) max_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GrepArgs {
    pub(super) path: String,
    pub(super) pattern: String,
    pub(super) glob: Option<String>,
    #[serde(default = "default_true")]
    pub(super) case_sensitive: bool,
    #[serde(default = "default_grep_results")]
    pub(super) max_results: usize,
    #[serde(default = "default_grep_file_bytes")]
    pub(super) max_file_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListFilesArgs {
    pub(super) path: String,
    pub(super) cursor: Option<String>,
    #[serde(default = "default_list_limit")]
    pub(super) limit: usize,
    #[serde(default)]
    pub(super) recursive: bool,
    pub(super) pattern: Option<String>,
    #[serde(default = "default_list_depth")]
    pub(super) max_depth: usize,
}

pub(super) fn default_start_line() -> u64 {
    1
}

pub(super) fn default_line_bytes() -> usize {
    256 * 1024
}

pub(super) fn default_grep_results() -> usize {
    200
}

pub(super) fn default_grep_file_bytes() -> u64 {
    1024 * 1024
}

pub(super) fn default_list_limit() -> usize {
    200
}

pub(super) fn default_list_depth() -> usize {
    16
}

pub(super) fn default_sync_max_files() -> usize {
    DEFAULT_SYNC_MAX_FILES
}

pub(super) fn default_sync_max_total_bytes() -> u64 {
    DEFAULT_MAX_TRANSFER_BYTES
}

pub(super) fn default_sync_max_depth() -> usize {
    DEFAULT_SYNC_MAX_DEPTH
}

pub(super) fn default_command_timeout() -> u64 {
    10_000
}

pub(super) fn default_process_job_timeout() -> u64 {
    DEFAULT_PROCESS_JOB_TIMEOUT_MS
}

pub(super) fn default_process_output_bytes() -> usize {
    DEFAULT_PROCESS_OUTPUT_BYTES
}

pub(super) fn default_process_wait() -> u64 {
    DEFAULT_PROCESS_WAIT_MS
}

pub(super) fn default_remote_probe_timeout() -> u64 {
    DEFAULT_REMOTE_PROBE_TIMEOUT_MS
}

pub(super) fn default_wait_remote_timeout() -> u64 {
    DEFAULT_WAIT_REMOTE_TIMEOUT_MS
}

pub(super) fn default_wait_remote_poll() -> u64 {
    DEFAULT_WAIT_REMOTE_POLL_MS
}

pub(super) fn default_reboot_delay() -> u64 {
    DEFAULT_REBOOT_DELAY_MS
}

pub(super) fn validate_upload_args(args: UploadArgs) -> Result<UploadArgs, ClientError> {
    validate_local_path(&args.local_path, "local_path")?;
    validate_remote_path(&args.remote_path)?;
    validate_mode(args.mode)?;
    Ok(args)
}

pub(super) fn validate_download_args(args: DownloadArgs) -> Result<DownloadArgs, ClientError> {
    validate_remote_path(&args.remote_path)?;
    validate_local_path(&args.local_path, "local_path")?;
    Ok(args)
}

pub(super) fn validate_mkdir_args(args: MkdirArgs) -> Result<MkdirArgs, ClientError> {
    validate_remote_path(&args.path)?;
    validate_mode(args.mode)?;
    let _ = args.recursive;
    Ok(args)
}

pub(super) fn validate_remove_args(args: RemoveArgs) -> Result<RemoveArgs, ClientError> {
    validate_remote_path(&args.path)?;
    let _ = args.recursive;
    Ok(args)
}

pub(super) fn validate_move_args(args: MoveArgs) -> Result<MoveArgs, ClientError> {
    validate_remote_path(&args.source)?;
    validate_remote_path(&args.destination)?;
    if args.source == args.destination {
        return invalid_params("source and destination must differ");
    }
    let _ = args.overwrite;
    Ok(args)
}

pub(super) fn validate_copy_args(args: CopyArgs) -> Result<CopyArgs, ClientError> {
    validate_remote_path(&args.source)?;
    validate_remote_path(&args.destination)?;
    if args.source == args.destination {
        return invalid_params("source and destination must differ");
    }
    let _ = (args.overwrite, args.recursive);
    Ok(args)
}

pub(super) fn validate_chmod_args(args: ChmodArgs) -> Result<ChmodArgs, ClientError> {
    validate_remote_path(&args.path)?;
    validate_mode(Some(args.mode))?;
    Ok(args)
}

pub(super) fn validate_symlink_args(args: SymlinkArgs) -> Result<SymlinkArgs, ClientError> {
    validate_remote_path(&args.target)?;
    validate_remote_path(&args.link_path)?;
    if args.target == args.link_path {
        return invalid_params("target and link_path must differ");
    }
    if args
        .target_kind
        .as_deref()
        .is_some_and(|kind| !matches!(kind, "file" | "dir"))
    {
        return invalid_params("target_kind must be file or dir");
    }
    let _ = args.overwrite;
    Ok(args)
}

pub(super) fn validate_sync_directory_args(
    args: SyncDirectoryArgs,
) -> Result<SyncDirectoryArgs, ClientError> {
    validate_local_path(&args.local_path, "local_path")?;
    validate_remote_path(&args.remote_path)?;
    validate_sync_limits(
        &args.excludes,
        args.max_files,
        args.max_total_bytes,
        args.max_depth,
    )?;
    Ok(args)
}

pub(super) fn validate_deploy_release_args(
    args: DeployReleaseArgs,
) -> Result<DeployReleaseArgs, ClientError> {
    validate_local_path(&args.local_path, "local_path")?;
    validate_remote_path(&args.releases_path)?;
    validate_remote_path(&args.current_path)?;
    if args.release_id.is_empty()
        || args.release_id.len() > MAX_RELEASE_ID_BYTES
        || matches!(args.release_id.as_str(), "." | "..")
        || !args
            .release_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return invalid_params(
            "release_id must use 1..=128 ASCII letters, digits, '.', '_', or '-'",
        );
    }
    if args
        .expected_arch
        .as_deref()
        .is_some_and(|arch| arch.is_empty() || arch.contains('\0'))
    {
        return invalid_params("expected_arch must not be empty or contain NUL");
    }
    if args.dependencies.len() > MAX_DEPLOY_DEPENDENCIES
        || args
            .dependencies
            .iter()
            .any(|value| value.is_empty() || value.len() > 1024 || value.contains('\0'))
    {
        return invalid_params(
            "dependencies must contain at most 64 non-empty paths or names of at most 1024 bytes",
        );
    }
    validate_deploy_command(&args.start)?;
    validate_deploy_command(&args.health)?;
    if let Some(command) = &args.stop {
        validate_deploy_command(command)?;
    }
    if let Some(command) = &args.rollback_start {
        validate_deploy_command(command)?;
    }
    validate_sync_limits(
        &args.excludes,
        args.max_files,
        args.max_total_bytes,
        args.max_depth,
    )?;
    Ok(args)
}

pub(super) fn validate_deploy_command(command: &DeployCommandArgs) -> Result<(), ClientError> {
    if command.program.is_empty() || command.program.contains('\0') {
        return invalid_params("deployment command program must not be empty or contain NUL");
    }
    if command.args.iter().any(|value| value.contains('\0')) {
        return invalid_params("deployment command args must not contain NUL");
    }
    if command
        .cwd
        .as_deref()
        .is_some_and(|cwd| cwd.is_empty() || cwd.contains('\0'))
    {
        return invalid_params("deployment command cwd must not be empty or contain NUL");
    }
    if command
        .env
        .iter()
        .any(|(name, value)| name.is_empty() || name.contains(['\0', '=']) || value.contains('\0'))
    {
        return invalid_params("deployment command environment is invalid");
    }
    if command.timeout_ms > 300_000 {
        return invalid_params("deployment command timeout_ms must be in range 0..=300000");
    }
    Ok(())
}

pub(super) fn validate_sync_limits(
    excludes: &[String],
    max_files: usize,
    max_total_bytes: u64,
    max_depth: usize,
) -> Result<(), ClientError> {
    if excludes.len() > MAX_SYNC_EXCLUDE_PATTERNS {
        return invalid_params(format!(
            "excludes must contain at most {MAX_SYNC_EXCLUDE_PATTERNS} patterns"
        ));
    }
    for pattern in excludes {
        if pattern.is_empty() || pattern.len() > MAX_SYNC_GLOB_BYTES {
            return invalid_params(format!(
                "exclude patterns must contain 1..={MAX_SYNC_GLOB_BYTES} bytes"
            ));
        }
        Glob::new(pattern).map_err(|error| {
            ClientError::local("invalid_params", format!("invalid exclude glob: {error}"))
        })?;
    }
    if max_files == 0 || max_files > MAX_SYNC_FILES {
        return invalid_params(format!("max_files must be in range 1..={MAX_SYNC_FILES}"));
    }
    if max_total_bytes > DEFAULT_MAX_TRANSFER_BYTES {
        return invalid_params(format!(
            "max_total_bytes must be in range 0..={DEFAULT_MAX_TRANSFER_BYTES}"
        ));
    }
    if max_depth == 0 || max_depth > MAX_SYNC_DEPTH {
        return invalid_params(format!("max_depth must be in range 1..={MAX_SYNC_DEPTH}"));
    }
    Ok(())
}

pub(super) fn validate_mode(mode: Option<u32>) -> Result<(), ClientError> {
    if mode.is_some_and(|mode| mode > MAX_UNIX_MODE) {
        invalid_params(format!("mode must be in range 0..={MAX_UNIX_MODE}"))
    } else {
        Ok(())
    }
}

pub(super) fn validate_local_path(path: &str, name: &str) -> Result<(), ClientError> {
    if path.is_empty() || path.contains('\0') {
        invalid_params(format!("{name} must not be empty or contain NUL"))
    } else {
        Ok(())
    }
}

pub(super) fn validate_remote_probe_args(
    args: RemoteProbeArgs,
) -> Result<RemoteProbeArgs, ClientError> {
    validate_probe_timeout(args.timeout_ms)?;
    Ok(args)
}

pub(super) fn validate_wait_remote_args(
    args: WaitRemoteArgs,
) -> Result<WaitRemoteArgs, ClientError> {
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

pub(super) fn validate_reboot_args(args: RebootArgs) -> Result<RebootArgs, ClientError> {
    if !(MIN_REBOOT_DELAY_MS..=MAX_REBOOT_DELAY_MS).contains(&args.delay_ms) {
        return invalid_params(format!(
            "delay_ms must be in range {MIN_REBOOT_DELAY_MS}..={MAX_REBOOT_DELAY_MS}"
        ));
    }
    Ok(args)
}

pub(super) fn validate_agent_update_args(
    args: AgentUpdateArgs,
) -> Result<AgentUpdateArgs, ClientError> {
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

pub(super) fn validate_wait_settings(
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

pub(super) fn validate_probe_timeout(timeout_ms: u64) -> Result<(), ClientError> {
    if !(MIN_REMOTE_PROBE_TIMEOUT_MS..=MAX_REMOTE_PROBE_TIMEOUT_MS).contains(&timeout_ms) {
        invalid_params(format!(
            "probe timeout must be in range {MIN_REMOTE_PROBE_TIMEOUT_MS}..={MAX_REMOTE_PROBE_TIMEOUT_MS}"
        ))
    } else {
        Ok(())
    }
}

pub(super) fn validate_apply_patch_args(args: ApplyPatchArgs) -> Result<(), ClientError> {
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
        Some(message) => Err(ClientError::local("invalid_params", message)),
        None => Ok(()),
    }
}

pub(super) fn valid_patch_envelope(path: &str, patch: &str) -> bool {
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

pub(super) fn validate_read_file_lines_args(args: ReadFileLinesArgs) -> Result<(), ClientError> {
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

pub(super) fn validate_grep_args(args: GrepArgs) -> Result<(), ClientError> {
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
        .map_err(|error| {
            ClientError::local("invalid_params", format!("invalid regex pattern: {error}"))
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

pub(super) fn validate_list_files_args(args: ListFilesArgs) -> Result<(), ClientError> {
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

pub(super) fn validate_process_start_args(args: ProcessStartArgs) -> Result<(), ClientError> {
    remote_ops_protocol::execution::validate_program(
        &args.program,
        &args.args,
        args.cwd.as_deref(),
        &args.env,
    )
    .map_err(|message| ClientError::local("invalid_params", message))?;
    if args.timeout_ms == 0 || args.timeout_ms > MAX_PROCESS_JOB_TIMEOUT_MS {
        return invalid_params(format!(
            "timeout_ms must be in range 1..={MAX_PROCESS_JOB_TIMEOUT_MS}"
        ));
    }
    Ok(())
}

pub(super) fn validate_process_output_args(args: ProcessOutputArgs) -> Result<(), ClientError> {
    validate_job_id(args.job_id)?;
    if args.max_bytes > MAX_PROCESS_OUTPUT_BYTES {
        return invalid_params(format!(
            "max_bytes must be in range 0..={MAX_PROCESS_OUTPUT_BYTES}"
        ));
    }
    let _ = (args.stdout_cursor, args.stderr_cursor);
    Ok(())
}

pub(super) fn validate_process_wait_args(args: ProcessWaitArgs) -> Result<(), ClientError> {
    validate_job_id(args.job_id)?;
    if args.wait_ms > MAX_PROCESS_WAIT_MS {
        return invalid_params(format!(
            "wait_ms must be in range 0..={MAX_PROCESS_WAIT_MS}"
        ));
    }
    Ok(())
}

pub(super) fn validate_process_signal_args(args: ProcessSignalArgs) -> Result<(), ClientError> {
    validate_job_id(args.job_id)?;
    if !(1..=64).contains(&args.signal) {
        return invalid_params("signal must be in range 1..=64");
    }
    Ok(())
}

pub(super) fn validate_job_id_args(args: JobIdArgs) -> Result<(), ClientError> {
    validate_job_id(args.job_id)
}

pub(super) fn validate_job_id(job_id: u64) -> Result<(), ClientError> {
    if job_id == 0 {
        invalid_params("job_id must be at least 1")
    } else {
        Ok(())
    }
}

pub(super) fn validate_remote_path(path: &str) -> Result<(), ClientError> {
    if path.is_empty() {
        invalid_params("path must not be empty")
    } else if path.contains('\0') {
        invalid_params("path must not contain NUL")
    } else {
        Ok(())
    }
}

pub(super) fn validate_glob(pattern: Option<&str>) -> Result<(), ClientError> {
    let Some(pattern) = pattern else {
        return Ok(());
    };
    if pattern.is_empty() {
        return invalid_params("glob pattern must not be empty");
    }
    if pattern.len() > MAX_GLOB_BYTES {
        return invalid_params(format!("glob pattern exceeds {MAX_GLOB_BYTES} bytes"));
    }
    Glob::new(pattern).map(|_| ()).map_err(|error| {
        ClientError::local("invalid_params", format!("invalid glob pattern: {error}"))
    })
}

pub(super) fn invalid_params<T>(message: impl Into<String>) -> Result<T, ClientError> {
    Err(ClientError::local("invalid_params", message))
}

pub(super) fn validate_pkill_args(args: PkillArgs) -> Result<(), ClientError> {
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
        Some(message) => Err(ClientError::local("invalid_params", message)),
        None => Ok(()),
    }
}
