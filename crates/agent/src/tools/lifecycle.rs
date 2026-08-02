use std::collections::BTreeMap;
use std::fs::{self, File};
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

use super::command;

pub const SELF_CHECK_TIMEOUT_MS: u64 = 10_000;
const UPDATE_START_TIMEOUT: Duration = Duration::from_secs(20);
const UPDATE_POLL_INTERVAL: Duration = Duration::from_millis(50);

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
    "agent_info",
    "reboot",
    "agent_update_prepare",
    "upload_file",
    "download_file",
];

struct RuntimeIdentity {
    instance_id: String,
    started_at_ms: u64,
    started: Instant,
}

static RUNTIME_IDENTITY: LazyLock<RuntimeIdentity> = LazyLock::new(|| {
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
});

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
            "uptime_ms": u64::try_from(RUNTIME_IDENTITY.started.elapsed().as_millis()).unwrap_or(u64::MAX)
        },
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "family": std::env::consts::FAMILY
        },
        "supported_operations": SUPPORTED_OPERATIONS,
        "capabilities": {
            "background_processes": true,
            "incremental_output": true,
            "active_probe": true,
            "wait_remote": true,
            "reboot": reboot_supported(),
            "self_update": executable.is_some() && staging.is_some(),
            "resumable_transfers": true,
            "directory_sync": true,
            "release_deployment": cfg!(unix)
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
            "self_check_timeout_ms": SELF_CHECK_TIMEOUT_MS
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
    copy_executable_permissions(target, &helper)?;

    let manifest = UpdateManifest {
        parent_pid: std::process::id(),
        target: target.to_path_buf(),
        staged: staged.to_path_buf(),
        backup,
        failed,
        health,
        helper: helper.clone(),
        restart_args: restart_args.to_vec(),
    };
    if let Err(error) = write_manifest(&manifest_path, &manifest) {
        let _ = fs::remove_file(&helper);
        return Err(error);
    }
    let spawn = Command::new(&helper)
        .arg("--update-helper")
        .arg(&manifest_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(error) = spawn {
        let _ = fs::remove_file(&manifest_path);
        let _ = fs::remove_file(&helper);
        return Err(AgentError::io("start update helper", error));
    }

    Ok(json!({
        "accepted": true,
        "restart_required": true,
        "previous_instance_id": RUNTIME_IDENTITY.instance_id,
        "candidate": candidate,
        "bytes_staged": metadata.len(),
        "sha256": actual_sha256,
        "staging_path": staged.to_string_lossy(),
        "prepared_at_ms": unix_time_ms()
    }))
}

pub fn run_update_helper(manifest_path: &Path) -> Result<(), String> {
    let manifest_bytes = fs::read(manifest_path)
        .map_err(|error| format!("read update manifest {}: {error}", manifest_path.display()))?;
    let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("parse update manifest: {error}"))?;
    wait_for_parent_exit(manifest.parent_pid)?;
    let result = apply_update(manifest_path, &manifest);
    if result.is_err() {
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
    let _ = fs::remove_file(&manifest.health);
    if let Err(error) = fs::rename(&manifest.target, &manifest.backup) {
        let _ = spawn_restarted_agent(manifest, false);
        return Err(format!("move current agent to rollback path: {error}"));
    }
    if let Err(error) = fs::rename(&manifest.staged, &manifest.target) {
        let _ = fs::rename(&manifest.backup, &manifest.target);
        let _ = spawn_restarted_agent(manifest, false);
        return Err(format!("activate staged agent: {error}"));
    }
    if let Err(error) = sync_parent(&manifest.target) {
        rollback_files(manifest)?;
        let _ = spawn_restarted_agent(manifest, false);
        return Err(format!("sync agent directory: {error}"));
    }

    let mut child = match spawn_restarted_agent(manifest, true) {
        Ok(child) => child,
        Err(error) => {
            rollback_files(manifest)?;
            let _ = spawn_restarted_agent(manifest, false);
            return Err(format!("start updated agent: {error}"));
        }
    };
    let deadline = Instant::now() + UPDATE_START_TIMEOUT;
    loop {
        if manifest.health.is_file() {
            thread::sleep(Duration::from_millis(500));
            match child.try_wait() {
                Ok(None) => {
                    let _ = fs::remove_file(&manifest.backup);
                    let _ = fs::remove_file(&manifest.health);
                    let _ = fs::remove_file(manifest_path);
                    return Ok(());
                }
                Ok(Some(status)) => {
                    rollback_files(manifest)?;
                    let _ = spawn_restarted_agent(manifest, false);
                    let _ = fs::remove_file(manifest_path);
                    return Err(format!(
                        "updated agent exited during health stabilization: {status}"
                    ));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    rollback_files(manifest)?;
                    let _ = spawn_restarted_agent(manifest, false);
                    let _ = fs::remove_file(manifest_path);
                    return Err(format!("stabilize updated agent: {error}"));
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                rollback_files(manifest)?;
                let _ = spawn_restarted_agent(manifest, false);
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
                let _ = spawn_restarted_agent(manifest, false);
                let _ = fs::remove_file(manifest_path);
                return Err(format!("wait for updated agent: {error}"));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            rollback_files(manifest)?;
            spawn_restarted_agent(manifest, false)
                .map_err(|error| format!("restart rollback agent: {error}"))?;
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
    let mut file = File::open(path)
        .map_err(|error| AgentError::io(format!("open {}", path.display()), error))?;
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
}
