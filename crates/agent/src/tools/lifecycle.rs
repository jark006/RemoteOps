use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use remote_ops_protocol::{
    APPLY_PATCH_MAX_FILE_BYTES, APPLY_PATCH_MAX_HUNKS, APPLY_PATCH_MAX_PATCH_BYTES,
    DEFAULT_CHUNK_BYTES, DEFAULT_MAX_CONTROL_BYTES, DEFAULT_PROCESS_JOB_TIMEOUT_MS,
    DEFAULT_PROCESS_OUTPUT_BYTES, DEFAULT_PROCESS_WAIT_MS, MAX_PROCESS_JOB_TIMEOUT_MS,
    MAX_PROCESS_JOBS, MAX_PROCESS_OUTPUT_BYTES, MAX_PROCESS_WAIT_MS, MAX_REBOOT_DELAY_MS,
    MIN_REBOOT_DELAY_MS, PROCESS_OUTPUT_BUFFER_BYTES, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{AgentError, AgentResult};
use crate::tools::timefmt::format_epoch_iso;

use super::command;

pub const SELF_CHECK_TIMEOUT_MS: u64 = 10_000;
const UPDATE_START_TIMEOUT: Duration = Duration::from_secs(20);
const UPDATE_POLL_INTERVAL: Duration = Duration::from_millis(50);
// Consecutive active observations required before a supervised restart counts as
// stable, spaced far enough apart to catch an Agent that exits after forking.
const STABILIZE_CHECKS: u32 = 3;
const STABILIZE_POLL_INTERVAL: Duration = Duration::from_millis(500);

pub const SUPPORTED_OPERATIONS: &[&str] = &[
    "read_text",
    "read_file_lines",
    "tail_text",
    "write_text",
    "apply_patch",
    "list_files",
    "grep",
    "stat",
    "file_hash",
    "mkdir",
    "remove",
    "move",
    "copy",
    "chmod",
    "symlink",
    "sync_prepare",
    "sync_commit",
    "sync_abort",
    "deploy_preflight",
    "deploy_activate",
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
    "environment",
    "agent_info",
    "reboot",
    "agent_update_prepare",
    "upload_file",
    "download_file",
];

fn supported_operations() -> Vec<&'static str> {
    SUPPORTED_OPERATIONS
        .iter()
        .copied()
        .filter(|operation| match *operation {
            "chmod" | "deploy_preflight" | "deploy_activate" => cfg!(unix),
            "reboot" => reboot_supported(),
            "sh_exec" | "environment" => cfg!(any(unix, windows)),
            "system_info" | "pids" | "process_info" | "kill" | "pkill" => {
                cfg!(any(target_os = "linux", target_os = "macos", windows))
            }
            _ => true,
        })
        .collect()
}

struct RuntimeIdentity {
    instance_id: String,
    started_at_ms: u64,
    started: Instant,
}

static RUNTIME_IDENTITY: LazyLock<RuntimeIdentity> = LazyLock::new(runtime_identity);

fn runtime_identity() -> RuntimeIdentity {
    let mut random = [0u8; 16];
    if getrandom::fill(&mut random).is_err() {
        let fallback = format!("{}:{}", unix_time_ms(), std::process::id());
        random.copy_from_slice(&Sha256::digest(fallback.as_bytes())[..16]);
    }
    RuntimeIdentity {
        instance_id: random.iter().map(|byte| format!("{byte:02x}")).collect(),
        started_at_ms: unix_time_ms(),
        started: Instant::now(),
    }
}

/// Force the runtime identity to be recorded at process start instead of on
/// first use, so `started_at_ms` and `uptime_ms` reflect the real process
/// lifetime. Called from the service startup path in `main`.
pub fn init_runtime_identity() {
    LazyLock::force(&RUNTIME_IDENTITY);
}

static RESTART_ARGS: OnceLock<Vec<String>> = OnceLock::new();

#[derive(Debug, Serialize, Deserialize)]
struct UpdateManifest {
    parent_pid: u32,
    target: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    failed: PathBuf,
    health: PathBuf,
    helper: PathBuf,
    restart_args: Vec<String>,
    // Systemd unit supervising the Agent, when there is one. The helper hands the
    // restarted Agent back to that unit so the final process stays supervised.
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    attempt_id: Option<String>,
    #[serde(default)]
    candidate_sha256: Option<String>,
    #[serde(default)]
    previous_instance_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateReceipt {
    attempt_id: String,
    candidate_sha256: String,
    previous_instance_id: String,
    status: String,
    completed_at_ms: u64,
    detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery: Option<Value>,
}

fn update_receipt_path(target: &Path) -> PathBuf {
    sibling_with_suffix(target, ".update-result.json")
}

fn last_update_receipt(target: &Path) -> Option<Value> {
    let (file, _) = super::files::ensure_regular_file(&update_receipt_path(target)).ok()?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 16 * 1024 {
        return None;
    }
    let receipt: UpdateReceipt = serde_json::from_slice(&bytes).ok()?;
    serde_json::to_value(receipt).ok()
}

fn write_update_receipt(
    manifest: &UpdateManifest,
    status: &str,
    detail: Option<String>,
) -> Result<(), String> {
    let (Some(attempt), Some(hash), Some(previous)) = (
        &manifest.attempt_id,
        &manifest.candidate_sha256,
        &manifest.previous_instance_id,
    ) else {
        return Ok(());
    };
    let receipt = UpdateReceipt {
        attempt_id: attempt.clone(),
        candidate_sha256: hash.clone(),
        previous_instance_id: previous.clone(),
        status: status.into(),
        completed_at_ms: unix_time_ms(),
        detail: detail.map(|detail| detail.chars().take(2048).collect()),
        recovery:matches!(status,"rollback_failed"|"unconfirmed").then(||json!({"target_path":manifest.target,"backup_path":manifest.backup,"staging_path":manifest.staged,"failed_path":manifest.failed,"helper_path":manifest.helper})),
    };
    let target = update_receipt_path(&manifest.target);
    let parent = target.parent().unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut temp, &receipt).map_err(|error| error.to_string())?;
    temp.flush()
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    super::files::persist_path_replace(temp.path(), &target, true)
        .map_err(|error| error.to_string())
}

// The updater starts the candidate Agent directly to verify it before committing.
// A supervisor would otherwise restart the unit in parallel and race for the
// listening port, so the helper asks the supervisor to restart once the new
// binary is proven healthy.
fn systemd_unit_of_self() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let (contents, truncated) = super::system::bounded_text("/proc/self/cgroup", 64 * 1024)?;
        if truncated {
            return None;
        }
        parse_systemd_unit(&contents)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

// Only Linux reads its own cgroup, but the parser is exercised on every target.
#[cfg(any(target_os = "linux", test))]
fn parse_systemd_unit(cgroup_contents: &str) -> Option<String> {
    cgroup_contents
        .lines()
        .flat_map(|line| line.rsplit(':').next().unwrap_or("").split('/'))
        .find(|part| {
            part.len() > ".service".len()
                && part.len() <= 256
                && !part.starts_with('-')
                && !part.contains('\0')
                && part.ends_with(".service")
        })
        .map(str::to_string)
}

fn systemctl_path() -> Option<PathBuf> {
    [
        "/usr/bin/systemctl",
        "/bin/systemctl",
        "/usr/sbin/systemctl",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

pub fn agent_info(max_transfer_bytes: u64) -> Value {
    let executable = std::env::current_exe().ok();
    let staging = executable.as_deref().and_then(update_staging_path_for);
    json!({
        "name": "remote-ops-agent",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": PROTOCOL_VERSION,
        "build": build_info(),
        "runtime": {
            "instance_id": RUNTIME_IDENTITY.instance_id,
            "pid": std::process::id(),
            "started_at_ms": RUNTIME_IDENTITY.started_at_ms,
            "started_at_iso": format_epoch_iso((RUNTIME_IDENTITY.started_at_ms / 1000) as i64),
            "uptime_ms": u64::try_from(RUNTIME_IDENTITY.started.elapsed().as_millis()).unwrap_or(u64::MAX)
        },
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "family": std::env::consts::FAMILY
        },
        "supported_operations": supported_operations(),
        "capabilities": {
            "background_processes": true,
            "incremental_output": true,
            "active_probe": true,
            "wait_remote": true,
            "reboot": reboot_supported(),
            "self_update": executable.is_some() && staging.is_some(),
            "resumable_transfers": true,
            "directory_sync": true,
            "release_deployment": cfg!(unix),
            "unix_mode": cfg!(unix),
            "shell_selection": cfg!(any(unix, windows)),
            "environment_profile": cfg!(any(unix, windows)),
            "update_receipts":true
        },
        "limits": {
            "max_control_bytes": DEFAULT_MAX_CONTROL_BYTES,
            "chunk_bytes": DEFAULT_CHUNK_BYTES,
            "max_transfer_bytes": max_transfer_bytes,
            "max_process_jobs": MAX_PROCESS_JOBS,
            "default_process_timeout_ms": DEFAULT_PROCESS_JOB_TIMEOUT_MS,
            "max_process_timeout_ms": MAX_PROCESS_JOB_TIMEOUT_MS,
            "process_output_buffer_bytes": PROCESS_OUTPUT_BUFFER_BYTES,
            "default_process_output_bytes": DEFAULT_PROCESS_OUTPUT_BYTES,
            "max_process_output_bytes": MAX_PROCESS_OUTPUT_BYTES,
            "default_process_wait_ms": DEFAULT_PROCESS_WAIT_MS,
            "max_process_wait_ms": MAX_PROCESS_WAIT_MS,
            "apply_patch_max_patch_bytes": APPLY_PATCH_MAX_PATCH_BYTES,
            "apply_patch_max_file_bytes": APPLY_PATCH_MAX_FILE_BYTES,
            "apply_patch_max_hunks": APPLY_PATCH_MAX_HUNKS,
            "max_file_operation_entries": remote_ops_protocol::MAX_FILE_OPERATION_ENTRIES,
            "default_sync_max_files": remote_ops_protocol::DEFAULT_SYNC_MAX_FILES,
            "max_sync_files": remote_ops_protocol::MAX_SYNC_FILES,
            "default_sync_max_depth": remote_ops_protocol::DEFAULT_SYNC_MAX_DEPTH,
            "max_sync_depth": remote_ops_protocol::MAX_SYNC_DEPTH,
            "max_sync_exclude_patterns": remote_ops_protocol::MAX_SYNC_EXCLUDE_PATTERNS
        },
        "update": {
            "executable_path": executable.as_ref().map(|path| path.to_string_lossy().into_owned()),
            "staging_path": staging.as_ref().map(|path| path.to_string_lossy().into_owned()),
            "self_check_timeout_ms": SELF_CHECK_TIMEOUT_MS,
            "last_result":executable.as_deref().and_then(last_update_receipt)
        }
    })
}

pub fn self_check_info() -> Value {
    json!({
        "name": "remote-ops-agent",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": PROTOCOL_VERSION,
        "build": build_info()
    })
}

pub fn configure_restart_args(args: Vec<String>) {
    let _ = RESTART_ARGS.set(args);
}

pub fn restart_args() -> &'static [String] {
    RESTART_ARGS.get().map(Vec::as_slice).unwrap_or(&[])
}

fn build_info() -> Value {
    json!({
        "target": env!("REMOTE_OPS_BUILD_TARGET"),
        "profile": env!("REMOTE_OPS_BUILD_PROFILE"),
        "git_revision": env!("REMOTE_OPS_GIT_REVISION")
    })
}

pub fn schedule_reboot(delay_ms: u64, max_transfer_bytes: u64) -> AgentResult<Value> {
    if !(MIN_REBOOT_DELAY_MS..=MAX_REBOOT_DELAY_MS).contains(&delay_ms) {
        return Err(AgentError::invalid(format!(
            "delay_ms must be in range {MIN_REBOOT_DELAY_MS}..={MAX_REBOOT_DELAY_MS}"
        )));
    }
    ensure_reboot_allowed()?;
    let instance_id = RUNTIME_IDENTITY.instance_id.clone();
    thread::Builder::new()
        .name("remote-ops-reboot".to_string())
        .spawn(move || {
            thread::sleep(Duration::from_millis(delay_ms));
            if let Err(error) = trigger_reboot() {
                eprintln!("scheduled reboot failed: {error}");
            }
        })
        .map_err(|error| AgentError::io("schedule reboot", error))?;
    Ok(json!({
        "accepted": true,
        "delay_ms": delay_ms,
        "requested_at_ms": unix_time_ms(),
        "previous_instance_id": instance_id,
        "agent": agent_info(max_transfer_bytes)
    }))
}

pub fn prepare_agent_update(
    expected_sha256: &str,
    max_transfer_bytes: u64,
    restart_args: &[String],
) -> AgentResult<Value> {
    validate_sha256(expected_sha256)?;
    let target = std::env::current_exe()
        .map_err(|error| AgentError::io("determine current executable", error))?;
    let staged = update_staging_path_for(&target)
        .ok_or_else(|| AgentError::command("could not derive update staging path"))?;
    let result = prepare_agent_update_inner(
        expected_sha256,
        max_transfer_bytes,
        restart_args,
        &target,
        &staged,
    );
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

fn prepare_agent_update_inner(
    expected_sha256: &str,
    max_transfer_bytes: u64,
    restart_args: &[String],
    target: &Path,
    staged: &Path,
) -> AgentResult<Value> {
    let metadata = fs::symlink_metadata(staged)
        .map_err(|error| AgentError::io(format!("stat {}", staged.display()), error))?;
    if !metadata.file_type().is_file() {
        return Err(AgentError::invalid(
            "update staging path must be a regular file and not a symlink",
        ));
    }
    if metadata.len() == 0 || metadata.len() > max_transfer_bytes {
        return Err(AgentError::invalid(format!(
            "update candidate size must be in range 1..={max_transfer_bytes}"
        )));
    }
    let actual_sha256 = sha256_file(staged, max_transfer_bytes)?;
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(AgentError::invalid(format!(
            "update candidate SHA-256 mismatch: expected {expected_sha256}, got {actual_sha256}"
        )));
    }
    copy_executable_permissions(target, staged)?;
    let candidate = check_candidate(staged)?;
    if candidate["name"] != "remote-ops-agent"
        || candidate["protocol_version"] != PROTOCOL_VERSION
        || candidate["build"]["target"] != env!("REMOTE_OPS_BUILD_TARGET")
    {
        return Err(AgentError::invalid(
            "update candidate name, protocol version, or build target is incompatible",
        ));
    }
    if let Some(unit) = systemd_unit_of_self() {
        validate_supervisor(&unit)?;
    }

    let suffix = format!(
        "{}-{}-{}",
        std::process::id(),
        &RUNTIME_IDENTITY.instance_id[..12],
        unix_time_ms()
    );
    let backup = sibling_with_suffix(target, &format!(".rollback-{suffix}"));
    let failed = sibling_with_suffix(target, &format!(".failed-{suffix}"));
    let health = sibling_with_suffix(target, &format!(".health-{suffix}.json"));
    let helper = helper_path(target, &suffix);
    let manifest_path = sibling_with_suffix(target, &format!(".update-{suffix}.json"));
    fs::copy(target, &helper).map_err(|error| {
        AgentError::io(format!("copy update helper {}", helper.display()), error)
    })?;
    if let Err(error) = copy_executable_permissions(target, &helper) {
        let _ = fs::remove_file(&helper);
        return Err(error);
    }

    let manifest = UpdateManifest {
        parent_pid: std::process::id(),
        target: target.to_path_buf(),
        staged: staged.to_path_buf(),
        backup,
        failed,
        health,
        helper: helper.clone(),
        restart_args: restart_args.to_vec(),
        unit: systemd_unit_of_self(),
        attempt_id: Some(suffix.clone()),
        candidate_sha256: Some(actual_sha256.clone()),
        previous_instance_id: Some(RUNTIME_IDENTITY.instance_id.clone()),
    };
    if let Err(error) = write_manifest(&manifest_path, &manifest) {
        let _ = fs::remove_file(&helper);
        return Err(error);
    }
    if let Err(error) = write_update_receipt(&manifest, "pending", None) {
        let _ = fs::remove_file(&manifest_path);
        let _ = fs::remove_file(&helper);
        return Err(AgentError::command(format!(
            "write update receipt: {error}"
        )));
    }
    let spawn = Command::new(&helper)
        .arg("--update-helper")
        .arg(&manifest_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(error) = spawn {
        let _ = write_update_receipt(
            &manifest,
            "unconfirmed",
            Some(format!("start helper: {error}")),
        );
        let _ = fs::remove_file(&manifest_path);
        let _ = fs::remove_file(&helper);
        return Err(AgentError::io("start update helper", error));
    }

    Ok(json!({
        "accepted": true,
        "attempt_id":suffix,
        "restart_required": true,
        "previous_instance_id": RUNTIME_IDENTITY.instance_id,
        "candidate": candidate,
        "bytes_staged": metadata.len(),
        "sha256": actual_sha256,
        "staging_path": staged.to_string_lossy(),
        "recovery":{"target_path":manifest.target,"backup_path":manifest.backup,"failed_path":manifest.failed,"manifest_path":manifest_path,"helper_path":manifest.helper,"receipt_path":update_receipt_path(&manifest.target)},
        "prepared_at_ms": unix_time_ms()
    }))
}

pub fn run_update_helper(manifest_path: &Path) -> Result<(), String> {
    let (file, _) =
        super::files::ensure_regular_file(manifest_path).map_err(|error| error.to_string())?;
    let mut manifest_bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut manifest_bytes)
        .map_err(|error| error.to_string())?;
    if manifest_bytes.len() > 64 * 1024 {
        return Err("update manifest exceeds 64 KiB".into());
    }
    let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("parse update manifest: {error}"))?;
    if let Err(error) = wait_for_parent_exit(manifest.parent_pid) {
        let _ = write_update_receipt(&manifest, "unconfirmed", Some(error.clone()));
        return Err(error);
    }
    let result = apply_update(manifest_path, &manifest);
    let status = if result.is_ok() {
        "updated"
    } else if manifest.backup.exists() {
        "rollback_failed"
    } else {
        "rolled_back"
    };
    write_update_receipt(&manifest, status, result.as_ref().err().cloned())?;
    if result.is_err() && !manifest.backup.exists() {
        let _ = fs::remove_file(&manifest.staged);
        let _ = fs::remove_file(&manifest.health);
        let _ = fs::remove_file(manifest_path);
    }
    result
}

pub fn write_health_marker(path: &Path, max_transfer_bytes: u64) -> AgentResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| AgentError::io("create update health marker", error))?;
    serde_json::to_writer(&mut temporary, &agent_info(max_transfer_bytes))
        .map_err(|error| AgentError::command(format!("serialize update health marker: {error}")))?;
    temporary
        .flush()
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| AgentError::io("flush update health marker", error))?;
    temporary.persist(path).map_err(|error| {
        AgentError::io(
            format!("persist update health marker {}", path.display()),
            error.error,
        )
    })?;
    Ok(())
}

pub fn schedule_cleanup(paths: Vec<PathBuf>) {
    let _ = thread::Builder::new()
        .name("remote-ops-update-cleanup".to_string())
        .spawn(move || {
            for _ in 0..100 {
                let mut remaining = false;
                for path in &paths {
                    match fs::remove_file(path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => remaining = true,
                    }
                }
                if !remaining {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
}

fn apply_update(manifest_path: &Path, manifest: &UpdateManifest) -> Result<(), String> {
    if fs::symlink_metadata(&manifest.backup).is_ok()
        || fs::symlink_metadata(&manifest.failed).is_ok()
    {
        restart_agent(manifest);
        return Err("update recovery paths already exist; refusing to overwrite them".into());
    }
    if let Some(expected) = &manifest.candidate_sha256 {
        let actual = sha256_file(
            &manifest.staged,
            remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
        )
        .map_err(|error| error.to_string());
        if !actual
            .as_ref()
            .is_ok_and(|actual| actual.eq_ignore_ascii_case(expected))
        {
            restart_agent(manifest);
            return Err(
                "staged update changed after candidate validation; old binary was retained".into(),
            );
        }
    }
    let _ = fs::remove_file(&manifest.health);
    if let Err(error) = fs::rename(&manifest.target, &manifest.backup) {
        restart_agent(manifest);
        return Err(format!("move current agent to rollback path: {error}"));
    }
    if let Err(error) = fs::rename(&manifest.staged, &manifest.target) {
        if let Err(restore) = fs::rename(&manifest.backup, &manifest.target) {
            return Err(format!(
                "activate staged agent: {error}; restore rollback binary failed: {restore}; backup retained at {}",
                manifest.backup.display()
            ));
        }
        restart_agent(manifest);
        return Err(format!("activate staged agent: {error}"));
    }
    if let Err(error) = sync_parent(&manifest.target) {
        rollback_files(manifest)?;
        restart_agent(manifest);
        return Err(format!("sync agent directory: {error}"));
    }

    let mut child = match spawn_restarted_agent(manifest, true) {
        Ok(child) => child,
        Err(error) => {
            rollback_files(manifest)?;
            restart_agent(manifest);
            return Err(format!("start updated agent: {error}"));
        }
    };
    let deadline = Instant::now() + UPDATE_START_TIMEOUT;
    loop {
        if manifest.health.is_file() {
            thread::sleep(Duration::from_millis(500));
            match child.try_wait() {
                Ok(None) => {
                    if let Err(error) = hand_over_to_supervisor(manifest, &mut child) {
                        let _ = child.kill();
                        let _ = child.wait();
                        if let Some(unit) = manifest.unit.as_deref()
                            && let Some(systemctl) = systemctl_path()
                        {
                            let _ = run_systemctl(&systemctl, &["stop", "--", unit], 10_000);
                        }
                        rollback_files(manifest)?;
                        restart_agent(manifest);
                        let _ = fs::remove_file(manifest_path);
                        return Err(error);
                    }
                    let _ = fs::remove_file(&manifest.backup);
                    let _ = fs::remove_file(&manifest.health);
                    // A supervisor restart drops --cleanup-update-helper, so the
                    // helper also retires its own executable.
                    let _ = fs::remove_file(&manifest.helper);
                    let _ = fs::remove_file(manifest_path);
                    return Ok(());
                }
                Ok(Some(status)) => {
                    rollback_files(manifest)?;
                    restart_agent(manifest);
                    let _ = fs::remove_file(manifest_path);
                    return Err(format!(
                        "updated agent exited during health stabilization: {status}"
                    ));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    rollback_files(manifest)?;
                    restart_agent(manifest);
                    let _ = fs::remove_file(manifest_path);
                    return Err(format!("stabilize updated agent: {error}"));
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                rollback_files(manifest)?;
                restart_agent(manifest);
                let _ = fs::remove_file(manifest_path);
                return Err(format!(
                    "updated agent exited before health check: {status}"
                ));
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                rollback_files(manifest)?;
                restart_agent(manifest);
                let _ = fs::remove_file(manifest_path);
                return Err(format!("wait for updated agent: {error}"));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            rollback_files(manifest)?;
            restart_agent(manifest);
            let _ = fs::remove_file(manifest_path);
            return Err("updated agent did not become healthy before timeout".to_string());
        }
        thread::sleep(UPDATE_POLL_INTERVAL);
    }
}

fn spawn_restarted_agent(
    manifest: &UpdateManifest,
    health_check: bool,
) -> std::io::Result<std::process::Child> {
    let mut command = Command::new(&manifest.target);
    command.args(&manifest.restart_args);
    if health_check {
        command.arg("--update-health-file").arg(&manifest.health);
    }
    command
        .arg("--cleanup-update-helper")
        .arg(&manifest.helper)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Hand the verified Agent over to its supervisor.
///
/// The updater starts the candidate directly so it can prove the new binary
/// healthy before committing. Leaving that instance in place would leave the
/// Agent unsupervised and racing its unit for the listening port, so the helper
/// stops it and asks the supervisor to start the installed binary instead.
fn hand_over_to_supervisor(
    manifest: &UpdateManifest,
    child: &mut std::process::Child,
) -> Result<(), String> {
    let Some(unit) = manifest.unit.as_deref() else {
        return Ok(());
    };
    let systemctl = systemctl_path().ok_or("supervised update requires systemctl")?;
    let _ = child.kill();
    let _ = child.wait();
    run_systemctl(&systemctl, &["restart", "--", unit], 10_000)?;
    // `Type=simple` reports the unit active as soon as it forks, so require the
    // restarted Agent to stay active across a short window before the previous
    // binary is discarded: one that cannot bind the port exits again immediately.
    let deadline = Instant::now() + UPDATE_START_TIMEOUT;
    let mut stabilized = 0;
    while stabilized < STABILIZE_CHECKS {
        if Instant::now() >= deadline {
            return Err(format!("{unit} did not stabilize after restart"));
        }
        if unit_is_active(
            &systemctl,
            unit,
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(2)),
        ) {
            stabilized += 1;
        } else {
            stabilized = 0;
        }
        thread::sleep(STABILIZE_POLL_INTERVAL);
    }
    Ok(())
}

/// Restart the restored Agent on rollback paths so it runs under the same
/// supervisor it had before the update attempt.
fn restart_agent(manifest: &UpdateManifest) {
    if let Some(unit) = manifest.unit.as_deref() {
        if let Some(systemctl) = systemctl_path() {
            let _ = run_systemctl(&systemctl, &["restart", "--", unit], 10_000);
        }
        return;
    }
    let _ = spawn_restarted_agent(manifest, false);
}

fn unit_is_active(systemctl: &Path, unit: &str, timeout: Duration) -> bool {
    run_systemctl(
        systemctl,
        &["is-active", "--quiet", "--", unit],
        timeout.as_millis().max(1) as u64,
    )
    .is_ok()
}

fn run_systemctl(program: &Path, args: &[&str], timeout_ms: u64) -> Result<Value, String> {
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let result = command::exec(
        &program.to_string_lossy(),
        &args,
        None,
        &BTreeMap::new(),
        timeout_ms,
    )
    .map_err(|error| error.to_string())?;
    if result["timed_out"] == true || result["exit_code"] != 0 {
        return Err(format!(
            "systemctl failed or timed out: {}",
            result["stderr"].as_str().unwrap_or_default()
        ));
    }
    Ok(result)
}

fn validate_supervisor(unit: &str) -> AgentResult<()> {
    let systemctl = systemctl_path().ok_or_else(|| {
        AgentError::unsupported("systemd-supervised self-update requires systemctl")
    })?;
    let result = run_systemctl(
        &systemctl,
        &[
            "show",
            "--property=KillMode",
            "--property=Restart",
            "--",
            unit,
        ],
        5000,
    )
    .map_err(AgentError::command)?;
    let properties = result["stdout"].as_str().unwrap_or_default();
    if !supervisor_properties_compatible(properties) {
        return Err(AgentError::unsupported(
            "self-update requires KillMode=process and Restart=on-failure on the supervising systemd unit",
        ));
    }
    Ok(())
}

fn supervisor_properties_compatible(properties: &str) -> bool {
    properties.lines().any(|line| line == "KillMode=process")
        && properties.lines().any(|line| line == "Restart=on-failure")
}

fn rollback_files(manifest: &UpdateManifest) -> Result<(), String> {
    if manifest.target.exists() {
        fs::rename(&manifest.target, &manifest.failed)
            .map_err(|error| format!("move failed agent aside: {error}"))?;
    }
    fs::rename(&manifest.backup, &manifest.target)
        .map_err(|error| format!("restore rollback agent: {error}"))?;
    let _ = fs::remove_file(&manifest.failed);
    sync_parent(&manifest.target).map_err(|error| format!("sync rollback directory: {error}"))
}

fn check_candidate(path: &Path) -> AgentResult<Value> {
    let result = command::exec(
        &path.to_string_lossy(),
        &["--self-check".to_string()],
        None,
        &BTreeMap::new(),
        SELF_CHECK_TIMEOUT_MS,
    )?;
    if result["timed_out"] == true || result["exit_code"] != 0 {
        return Err(AgentError::invalid(format!(
            "update candidate self-check failed: {}",
            result["stderr"].as_str().unwrap_or_default()
        )));
    }
    serde_json::from_str(result["stdout"].as_str().unwrap_or_default()).map_err(|error| {
        AgentError::invalid(format!("invalid candidate self-check output: {error}"))
    })
}

fn sha256_file(path: &Path, max_bytes: u64) -> AgentResult<String> {
    let (file, _) = super::files::ensure_regular_file(path)?;
    let mut file = file.take(max_bytes.saturating_add(1));
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| AgentError::io(format!("read {}", path.display()), error))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| AgentError::invalid("update candidate size overflow"))?;
        if total > max_bytes {
            return Err(AgentError::invalid(
                "update candidate exceeds transfer limit",
            ));
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_sha256(value: &str) -> AgentResult<()> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(AgentError::invalid(
            "expected_sha256 must contain exactly 64 hexadecimal characters",
        ))
    }
}

fn update_staging_path_for(target: &Path) -> Option<PathBuf> {
    let name = target.file_name()?.to_string_lossy();
    #[cfg(windows)]
    let staged_name = format!("{}.update-stage.exe", name.trim_end_matches(".exe"));
    #[cfg(not(windows))]
    let staged_name = format!("{name}.update-stage");
    Some(target.with_file_name(staged_name))
}

fn helper_path(target: &Path, suffix: &str) -> PathBuf {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    #[cfg(windows)]
    let helper_name = format!(
        "{}.update-helper-{suffix}.exe",
        name.trim_end_matches(".exe")
    );
    #[cfg(not(windows))]
    let helper_name = format!("{name}.update-helper-{suffix}");
    target.with_file_name(helper_name)
}

fn sibling_with_suffix(target: &Path, suffix: &str) -> PathBuf {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    target.with_file_name(format!("{name}{suffix}"))
}

fn copy_executable_permissions(source: &Path, destination: &Path) -> AgentResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(source)
            .map_err(|error| AgentError::io(format!("stat {}", source.display()), error))?
            .permissions()
            .mode();
        fs::set_permissions(destination, fs::Permissions::from_mode(mode)).map_err(|error| {
            AgentError::io(
                format!("set permissions on {}", destination.display()),
                error,
            )
        })?;
    }
    #[cfg(not(unix))]
    let _ = (source, destination);
    Ok(())
}

fn write_manifest(path: &Path, manifest: &UpdateManifest) -> AgentResult<()> {
    let bytes = serde_json::to_vec(manifest)
        .map_err(|error| AgentError::command(format!("serialize update manifest: {error}")))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(path)
        .map_err(|error| AgentError::io(format!("create {}", path.display()), error))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| AgentError::io(format!("write {}", path.display()), error))
}

fn wait_for_parent_exit(pid: u32) -> Result<(), String> {
    let deadline = Instant::now() + UPDATE_START_TIMEOUT;
    while process_is_alive(pid) {
        if Instant::now() >= deadline {
            return Err(format!(
                "agent process {pid} did not exit before update timeout"
            ));
        }
        thread::sleep(UPDATE_POLL_INTERVAL);
    }
    Ok(())
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // An exited Linux process may remain as a zombie until its original parent
    // reaps it (notably an interactive BusyBox shell). kill(pid, 0) still succeeds
    // for that PID, but it can no longer execute or own the listening socket.
    #[cfg(target_os = "linux")]
    if let Some((contents, false)) =
        super::system::bounded_text(format!("/proc/{pid}/stat"), 16 * 1024)
        && let Some((_, fields)) = contents.rsplit_once(") ")
        && matches!(fields.split_whitespace().next(), Some("Z" | "X" | "x"))
    {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

    let handle = unsafe { OpenProcess(SYNCHRONIZE_ACCESS, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let status = unsafe { WaitForSingleObject(handle, 0) };
    unsafe { CloseHandle(handle) };
    status == WAIT_TIMEOUT
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn sync_parent(target: &Path) -> std::io::Result<()> {
    File::open(target.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_target: &Path) -> std::io::Result<()> {
    Ok(())
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(target_os = "linux")]
fn reboot_supported() -> bool {
    true
}

#[cfg(windows)]
fn reboot_supported() -> bool {
    true
}

#[cfg(not(any(target_os = "linux", windows)))]
fn reboot_supported() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn ensure_reboot_allowed() -> AgentResult<()> {
    if unsafe { libc::geteuid() } == 0 {
        Ok(())
    } else {
        Err(AgentError::command(
            "reboot requires the agent to run as root or with equivalent privilege",
        ))
    }
}

#[cfg(windows)]
fn ensure_reboot_allowed() -> AgentResult<()> {
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn ensure_reboot_allowed() -> AgentResult<()> {
    Err(AgentError::unsupported(
        "reboot is supported only on Linux and Windows",
    ))
}

#[cfg(target_os = "linux")]
fn trigger_reboot() -> std::io::Result<()> {
    unsafe {
        libc::sync();
        if libc::reboot(libc::RB_AUTOBOOT) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(windows)]
fn trigger_reboot() -> std::io::Result<()> {
    let status = Command::new("shutdown.exe")
        .args(["/r", "/t", "0", "/f"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "shutdown.exe exited with {status}"
        )))
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn trigger_reboot() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "reboot is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_policy_requires_both_update_safe_properties() {
        assert!(supervisor_properties_compatible(
            "Restart=on-failure\nKillMode=process\n"
        ));
        assert!(!supervisor_properties_compatible(
            "Restart=always\nKillMode=process\n"
        ));
        assert!(!supervisor_properties_compatible(
            "Restart=on-failure\nKillMode=control-group\n"
        ));
        assert!(!supervisor_properties_compatible(""));
    }

    #[test]
    fn update_receipt_is_bounded_and_correlated_with_the_attempt() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("agent");
        let manifest = UpdateManifest {
            parent_pid: 1,
            target: target.clone(),
            staged: root.path().join("staged"),
            backup: root.path().join("backup"),
            failed: root.path().join("failed"),
            health: root.path().join("health"),
            helper: root.path().join("helper"),
            restart_args: vec![],
            unit: None,
            attempt_id: Some("unique-attempt".into()),
            candidate_sha256: Some("a".repeat(64)),
            previous_instance_id: Some("previous-instance".into()),
        };
        write_update_receipt(&manifest, "updated", Some("detail".repeat(2000))).unwrap();
        let receipt = last_update_receipt(&target).unwrap();
        assert_eq!(receipt["attempt_id"], "unique-attempt");
        assert_eq!(receipt["status"], "updated");
        assert_eq!(receipt["candidate_sha256"], "a".repeat(64));
        assert!(receipt["detail"].as_str().unwrap().len() <= 2048);
        fs::write(update_receipt_path(&target), vec![b'x'; 16 * 1024 + 1]).unwrap();
        assert!(last_update_receipt(&target).is_none());
    }
    #[test]
    fn agent_info_has_stable_identity_capabilities_and_limits() {
        let first = agent_info(1234);
        let second = agent_info(1234);
        assert_eq!(
            first["runtime"]["instance_id"],
            second["runtime"]["instance_id"]
        );
        assert_eq!(first["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(first["limits"]["max_transfer_bytes"], 1234);
        assert_eq!(first["capabilities"]["active_probe"], true);
        assert!(
            first["supported_operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|operation| operation == "agent_info")
        );
    }

    #[test]
    fn runtime_identity_is_initialized_on_demand_with_monotonic_uptime() {
        init_runtime_identity();
        let identity = &*RUNTIME_IDENTITY;
        // The recorded start time must not be later than "now" at any later
        // observation, and uptime must grow monotonically from it.
        assert!(identity.started_at_ms <= unix_time_ms());
        let first = agent_info(1);
        thread::sleep(Duration::from_millis(5));
        let second = agent_info(1);
        assert_eq!(
            first["runtime"]["started_at_ms"],
            second["runtime"]["started_at_ms"]
        );
        assert!(
            second["runtime"]["uptime_ms"].as_u64().unwrap()
                >= first["runtime"]["uptime_ms"].as_u64().unwrap()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn update_parent_exit_accepts_an_unreaped_linux_zombie() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read ignored || :"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(process_is_alive(pid), "running children must remain alive");
        drop(child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_is_alive(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let detected_exit = !process_is_alive(pid);
        let pid_still_exists = unsafe { libc::kill(pid as i32, 0) } == 0;
        let update_wait = detected_exit.then(|| wait_for_parent_exit(pid));
        child.wait().unwrap();
        assert!(
            pid_still_exists,
            "fixture must remain unreaped during detection"
        );
        assert!(detected_exit, "a zombie must not block the update helper");
        assert!(update_wait.unwrap().is_ok());
    }

    #[test]
    fn self_check_is_bounded_machine_readable_metadata() {
        let value = self_check_info();
        assert_eq!(value["name"], "remote-ops-agent");
        assert_eq!(value["protocol_version"], PROTOCOL_VERSION);
        assert!(value["build"]["target"].as_str().is_some());
        assert!(serde_json::to_vec(&value).unwrap().len() < 4096);
    }

    #[test]
    fn invalid_reboot_delay_is_rejected_without_scheduling() {
        let error = schedule_reboot(MIN_REBOOT_DELAY_MS - 1, 1).unwrap_err();
        assert_eq!(error.kind, "invalid_params");
        let error = schedule_reboot(MAX_REBOOT_DELAY_MS + 1, 1).unwrap_err();
        assert_eq!(error.kind, "invalid_params");
    }

    #[test]
    fn update_sha256_requires_exact_hex_digest() {
        for invalid in ["", "abc", &"g".repeat(64)] {
            assert!(validate_sha256(invalid).is_err());
        }
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn supervised_units_are_recognized_from_cgroup_paths() {
        for (cgroup, expected) in [
            (
                "0::/system.slice/remote-ops-agent.service\n",
                Some("remote-ops-agent.service"),
            ),
            (
                "1:name=systemd:/system.slice/remote-ops-agent.service\n",
                Some("remote-ops-agent.service"),
            ),
            ("0::/system.slice/foo.service/nested\n", Some("foo.service")),
            ("0::/\n", None),
            ("0::/user.slice/user-1000.slice/session-3.scope\n", None),
            ("", None),
        ] {
            assert_eq!(
                parse_systemd_unit(cgroup).as_deref(),
                expected,
                "cgroup {cgroup:?}"
            );
        }
    }

    #[test]
    fn update_manifest_without_a_unit_stays_readable() {
        // Manifests written before the unit field existed must still parse so a
        // staged update from an older Agent keeps working.
        let manifest: UpdateManifest = serde_json::from_str(
            r#"{"parent_pid":1,"target":"/a","staged":"/b","backup":"/c",
                "failed":"/d","health":"/e","helper":"/f","restart_args":[]}"#,
        )
        .unwrap();
        assert!(manifest.unit.is_none());
    }
}
