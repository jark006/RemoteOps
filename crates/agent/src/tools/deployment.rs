use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use remote_ops_protocol::CommandSpec;
use remote_ops_protocol::{
    DeployActivateRequest, DeployPreflightRequest, MAX_SYNC_DEPTH, MAX_SYNC_FILES, MAX_UNIX_MODE,
    SyncEntry, SyncFinishRequest, SyncPrepareRequest,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use tempfile::NamedTempFile;

use crate::error::{AgentError, AgentResult};

#[cfg(unix)]
use super::command;
use super::file_ops::verify_regular_file;

#[cfg(unix)]
const DEPLOY_STEP_OUTPUT_BYTES: usize = 64 * 1024;

pub fn sync_prepare(request: SyncPrepareRequest) -> AgentResult<Value> {
    validate_sync_request(&request)?;
    let target = Path::new(&request.remote_path);
    if let Ok(metadata) = fs::symlink_metadata(target)
        && !metadata.file_type().is_dir()
    {
        return Err(AgentError::invalid(
            "remote_path must be a directory and not a symbolic link",
        ));
    }
    let staging = staging_path_for(target)?;
    fs::create_dir(&staging).map_err(|error| {
        AgentError::io(
            format!("create sync staging directory {}", staging.display()),
            error,
        )
    })?;

    let prepared = (|| {
        let mut required = Vec::new();
        let mut reused_files = 0usize;
        let mut reused_bytes = 0u64;
        for entry in &request.entries {
            let staged_path = join_entry(&staging, &entry.path);
            if entry.kind == "dir" {
                fs::create_dir_all(&staged_path).map_err(|error| {
                    AgentError::io(
                        format!("create staged directory {}", staged_path.display()),
                        error,
                    )
                })?;
                continue;
            }
            let source = join_entry(target, &entry.path);
            let expected_hash = entry.sha256.as_deref().expect("validated file hash");
            if verify_regular_file(&source, entry.size, expected_hash)? {
                let parent = staged_path.parent().expect("entry has staging parent");
                fs::create_dir_all(parent).map_err(|error| {
                    AgentError::io(
                        format!("create staged directory {}", parent.display()),
                        error,
                    )
                })?;
                fs::copy(&source, &staged_path).map_err(|error| {
                    AgentError::io(format!("reuse unchanged file {}", source.display()), error)
                })?;
                apply_mode(&staged_path, entry.mode)?;
                reused_files += 1;
                reused_bytes = reused_bytes
                    .checked_add(entry.size)
                    .ok_or_else(|| AgentError::invalid("reused byte count overflow"))?;
            } else {
                required.push(entry.path.clone());
            }
        }
        write_marker(&staging, &request)?;
        Ok(json!({
            "remote_path": request.remote_path,
            "staging_path": staging.to_string_lossy(),
            "manifest_sha256": request.manifest_sha256,
            "required_uploads": required,
            "reused_files": reused_files,
            "reused_bytes": reused_bytes
        }))
    })();
    if prepared.is_err() {
        let _ = remove_staging(&staging);
    }
    prepared
}

pub fn sync_commit(request: SyncFinishRequest) -> AgentResult<Value> {
    validate_finish_request(&request)?;
    let target = Path::new(&request.remote_path);
    let staging = Path::new(&request.staging_path);
    validate_staging_relationship(target, staging)?;
    let manifest = read_marker(staging)?;
    validate_sync_request(&manifest)?;
    if manifest.remote_path != request.remote_path
        || manifest.manifest_sha256 != request.manifest_sha256
    {
        return Err(AgentError::invalid(
            "staging manifest does not match commit request",
        ));
    }
    validate_staged_tree(staging, &manifest.entries)?;
    apply_directory_modes(staging, manifest.root_mode, &manifest.entries)?;

    let backup = if fs::symlink_metadata(target).is_ok() {
        let backup = backup_path_for(target)?;
        fs::rename(target, &backup).map_err(|error| {
            AgentError::io(
                format!("move previous directory to {}", backup.display()),
                error,
            )
        })?;
        Some(backup)
    } else {
        None
    };
    if let Err(error) = fs::rename(staging, target) {
        if let Some(backup) = &backup {
            let _ = fs::rename(backup, target);
        }
        return Err(AgentError::io(
            format!("activate staged directory {}", target.display()),
            error,
        ));
    }
    sync_parent(target)?;
    let metadata_cleaned = fs::remove_file(marker_path(staging)).is_ok();
    Ok(json!({
        "committed": true,
        "remote_path": request.remote_path,
        "manifest_sha256": request.manifest_sha256,
        "backup_path": backup.map(|path| path.to_string_lossy().into_owned()),
        "files": manifest.entries.iter().filter(|entry| entry.kind == "file").count(),
        "directories": manifest.entries.iter().filter(|entry| entry.kind == "dir").count(),
        "metadata_cleaned": metadata_cleaned
    }))
}

pub fn sync_abort(request: SyncFinishRequest) -> AgentResult<Value> {
    validate_finish_request(&request)?;
    let target = Path::new(&request.remote_path);
    let staging = Path::new(&request.staging_path);
    validate_staging_relationship(target, staging)?;
    let manifest = read_marker(staging)?;
    validate_sync_request(&manifest)?;
    if manifest.remote_path != request.remote_path
        || manifest.manifest_sha256 != request.manifest_sha256
    {
        return Err(AgentError::invalid(
            "staging manifest does not match abort request",
        ));
    }
    make_tree_removable(staging, &manifest.entries)?;
    remove_staging(staging)?;
    fs::remove_file(marker_path(staging)).map_err(|error| {
        AgentError::io(
            format!("remove sync metadata for {}", staging.display()),
            error,
        )
    })?;
    Ok(json!({"aborted": true, "staging_path": request.staging_path}))
}

pub fn deploy_preflight(request: DeployPreflightRequest) -> AgentResult<Value> {
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(AgentError::unsupported(
            "deploy_release requires a Unix Agent",
        ))
    }
    #[cfg(unix)]
    {
        validate_deploy_paths(
            &request.releases_path,
            &request.current_path,
            &request.release_path,
        )?;
        if request.dependencies.len() > remote_ops_protocol::MAX_DEPLOY_DEPENDENCIES {
            return Err(AgentError::invalid(format!(
                "dependencies exceeds {} entries",
                remote_ops_protocol::MAX_DEPLOY_DEPENDENCIES
            )));
        }
        if let Some(expected) = &request.expected_arch
            && expected != std::env::consts::ARCH
        {
            return Err(AgentError::invalid(format!(
                "architecture mismatch: expected {expected}, remote is {}",
                std::env::consts::ARCH
            )));
        }
        let releases = Path::new(&request.releases_path);
        fs::create_dir_all(releases).map_err(|error| {
            AgentError::io(
                format!("create releases directory {}", releases.display()),
                error,
            )
        })?;
        let metadata = fs::symlink_metadata(releases).map_err(|error| {
            AgentError::io(
                format!("stat releases directory {}", releases.display()),
                error,
            )
        })?;
        if !metadata.file_type().is_dir() {
            return Err(AgentError::invalid(
                "releases_path must be a directory and not a symbolic link",
            ));
        }
        if fs::symlink_metadata(&request.release_path).is_ok() {
            return Err(AgentError::invalid("release_path already exists"));
        }
        if let Ok(current) = fs::symlink_metadata(&request.current_path)
            && !current.file_type().is_symlink()
        {
            return Err(AgentError::invalid(
                "current_path must be absent or a symbolic link",
            ));
        }
        let probe = NamedTempFile::new_in(releases).map_err(|error| {
            AgentError::io(
                format!("verify write permission in {}", releases.display()),
                error,
            )
        })?;
        drop(probe);
        let current_parent = Path::new(&request.current_path)
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let current_parent_metadata = fs::symlink_metadata(current_parent).map_err(|error| {
            AgentError::io(
                format!("stat current link parent {}", current_parent.display()),
                error,
            )
        })?;
        if !current_parent_metadata.file_type().is_dir() {
            return Err(AgentError::invalid(
                "current_path parent must be a directory and not a symbolic link",
            ));
        }
        let current_probe = NamedTempFile::new_in(current_parent).map_err(|error| {
            AgentError::io(
                format!("verify write permission in {}", current_parent.display()),
                error,
            )
        })?;
        drop(current_probe);
        let available_bytes = available_bytes(releases)?;
        if available_bytes < request.required_bytes {
            return Err(AgentError::invalid(format!(
                "insufficient disk space: requires {} bytes, {} available",
                request.required_bytes, available_bytes
            )));
        }
        let missing = request
            .dependencies
            .iter()
            .filter(|dependency| !command_available(dependency))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(AgentError::invalid(format!(
                "missing dependencies: {}",
                missing.join(", ")
            )));
        }
        Ok(json!({
            "ready": true,
            "architecture": std::env::consts::ARCH,
            "available_bytes": available_bytes,
            "required_bytes": request.required_bytes,
            "dependencies": request.dependencies,
            "release_path": request.release_path,
            "current_path": request.current_path
        }))
    }
}

pub fn deploy_activate(request: DeployActivateRequest) -> AgentResult<Value> {
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(AgentError::unsupported(
            "deploy_release requires a Unix Agent",
        ))
    }
    #[cfg(unix)]
    {
        validate_command(&request.start)?;
        validate_command(&request.health)?;
        if let Some(command) = &request.stop {
            validate_command(command)?;
        }
        if let Some(command) = &request.rollback_start {
            validate_command(command)?;
        }
        let release = Path::new(&request.release_path);
        let release_metadata = fs::symlink_metadata(release)
            .map_err(|error| AgentError::io(format!("stat {}", release.display()), error))?;
        if !release_metadata.file_type().is_dir() {
            return Err(AgentError::invalid(
                "release_path must be a directory and not a symbolic link",
            ));
        }
        let current = Path::new(&request.current_path);
        if let Ok(metadata) = fs::symlink_metadata(current)
            && !metadata.file_type().is_symlink()
        {
            return Err(AgentError::invalid(
                "current_path must be absent or a symbolic link",
            ));
        }
        let previous_target = fs::read_link(current).ok();
        let mut steps = Vec::new();

        if let Some(stop) = &request.stop {
            let result = run_command("stop", stop)?;
            let succeeded = command_succeeded(&result);
            steps.push(result);
            if !succeeded {
                return Ok(deploy_result(
                    "stop_failed",
                    false,
                    false,
                    previous_target.as_deref(),
                    &request,
                    steps,
                ));
            }
        }

        if let Err(error) = switch_symlink(current, release) {
            if previous_target.is_some() {
                let restart = request.rollback_start.as_ref().unwrap_or(&request.start);
                if let Ok(result) = run_command("restart_previous_after_switch_failure", restart) {
                    steps.push(result);
                }
            }
            return Err(error);
        }
        steps.push(json!({"step":"switch","succeeded":true,"target":request.release_path}));

        let start_result = run_command("start", &request.start)?;
        let start_succeeded = command_succeeded(&start_result);
        steps.push(start_result);
        if !start_succeeded {
            return rollback_deploy(previous_target.as_deref(), &request, steps, "start_failed");
        }

        let health_result = run_command("health", &request.health)?;
        let healthy = command_succeeded(&health_result);
        steps.push(health_result);
        if !healthy {
            return rollback_deploy(previous_target.as_deref(), &request, steps, "health_failed");
        }

        Ok(deploy_result(
            "deployed",
            true,
            false,
            previous_target.as_deref(),
            &request,
            steps,
        ))
    }
}

#[cfg(unix)]
fn rollback_deploy(
    previous_target: Option<&Path>,
    request: &DeployActivateRequest,
    mut steps: Vec<Value>,
    failure_status: &str,
) -> AgentResult<Value> {
    let current = Path::new(&request.current_path);
    match previous_target {
        Some(previous) => switch_symlink(current, previous)?,
        None => fs::remove_file(current).map_err(|error| {
            AgentError::io(
                format!("remove new current link {}", current.display()),
                error,
            )
        })?,
    }
    steps.push(json!({"step":"rollback_switch","succeeded":true,"target":previous_target.map(|path| path.to_string_lossy())}));
    let restarted = if previous_target.is_some() {
        let rollback_start = request.rollback_start.as_ref().unwrap_or(&request.start);
        let restart_result = run_command("rollback_start", rollback_start)?;
        let succeeded = command_succeeded(&restart_result);
        steps.push(restart_result);
        succeeded
    } else {
        true
    };
    let mut result = deploy_result(
        if restarted {
            "rolled_back"
        } else {
            "rollback_failed"
        },
        false,
        true,
        previous_target,
        request,
        steps,
    );
    result
        .as_object_mut()
        .expect("deploy result is object")
        .insert(
            "failure_stage".to_string(),
            Value::String(failure_status.to_string()),
        );
    Ok(result)
}

#[cfg(unix)]
fn deploy_result(
    status: &str,
    deployed: bool,
    rolled_back: bool,
    previous_target: Option<&Path>,
    request: &DeployActivateRequest,
    steps: Vec<Value>,
) -> Value {
    json!({
        "status": status,
        "deployed": deployed,
        "rolled_back": rolled_back,
        "release_path": request.release_path,
        "current_path": request.current_path,
        "previous_target": previous_target.map(|path| path.to_string_lossy()),
        "steps": steps
    })
}

#[cfg(unix)]
fn run_command(step: &str, spec: &CommandSpec) -> AgentResult<Value> {
    let mut result = command::exec(
        &spec.program,
        &spec.args,
        spec.cwd.as_deref(),
        &spec.env,
        spec.timeout_ms,
    )?;
    if let Some(object) = result.as_object_mut() {
        truncate_step_output(object, "stdout", "stdout_truncated");
        truncate_step_output(object, "stderr", "stderr_truncated");
        object.insert("step".to_string(), Value::String(step.to_string()));
        object.insert(
            "succeeded".to_string(),
            Value::Bool(command_succeeded(&Value::Object(object.clone()))),
        );
    }
    Ok(result)
}

#[cfg(unix)]
fn truncate_step_output(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
    truncated_field: &str,
) {
    let Some(text) = object.get(field).and_then(Value::as_str) else {
        return;
    };
    if text.len() <= DEPLOY_STEP_OUTPUT_BYTES {
        return;
    }
    let mut end = DEPLOY_STEP_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    object.insert(field.to_string(), Value::String(text[..end].to_string()));
    object.insert(truncated_field.to_string(), Value::Bool(true));
}

#[cfg(unix)]
fn command_succeeded(result: &Value) -> bool {
    result["exit_code"].as_i64() == Some(0) && result["timed_out"] == false
}

#[cfg(unix)]
fn validate_command(spec: &CommandSpec) -> AgentResult<()> {
    if spec.program.is_empty() || spec.program.contains('\0') {
        return Err(AgentError::invalid(
            "deployment command program must not be empty or contain NUL",
        ));
    }
    if spec.timeout_ms > command::MAX_TIMEOUT_MS {
        return Err(AgentError::invalid(format!(
            "deployment command timeout_ms must be in range 0..={}",
            command::MAX_TIMEOUT_MS
        )));
    }
    Ok(())
}

fn validate_sync_request(request: &SyncPrepareRequest) -> AgentResult<()> {
    validate_path_text(&request.remote_path, "remote_path")?;
    validate_sha256(&request.manifest_sha256, "manifest_sha256")?;
    if request.max_files == 0 || request.max_files > MAX_SYNC_FILES {
        return Err(AgentError::invalid(format!(
            "max_files must be in range 1..={MAX_SYNC_FILES}"
        )));
    }
    if request.max_depth == 0 || request.max_depth > MAX_SYNC_DEPTH {
        return Err(AgentError::invalid(format!(
            "max_depth must be in range 1..={MAX_SYNC_DEPTH}"
        )));
    }
    if request.entries.len() > request.max_files {
        return Err(AgentError::invalid("manifest exceeds max_files"));
    }
    if request.root_mode.is_some_and(|mode| mode > MAX_UNIX_MODE) {
        return Err(AgentError::invalid(format!(
            "root_mode must be in range 0..={MAX_UNIX_MODE}"
        )));
    }
    let actual_manifest = manifest_sha256(request.root_mode, &request.entries)?;
    if !actual_manifest.eq_ignore_ascii_case(&request.manifest_sha256) {
        return Err(AgentError::invalid("manifest SHA-256 mismatch"));
    }
    let mut previous: Option<&str> = None;
    let mut total = 0u64;
    for entry in &request.entries {
        validate_sync_entry(entry, request.max_depth)?;
        if previous.is_some_and(|previous| previous >= entry.path.as_str()) {
            return Err(AgentError::invalid(
                "manifest paths must be unique and strictly sorted",
            ));
        }
        previous = Some(&entry.path);
        if entry.kind == "file" {
            total = total
                .checked_add(entry.size)
                .ok_or_else(|| AgentError::invalid("manifest byte count overflow"))?;
        }
        if total > request.max_total_bytes {
            return Err(AgentError::invalid("manifest exceeds max_total_bytes"));
        }
    }
    Ok(())
}

fn validate_sync_entry(entry: &SyncEntry, max_depth: usize) -> AgentResult<()> {
    if entry.path.is_empty() || entry.path.contains('\0') || entry.path.contains('\\') {
        return Err(AgentError::invalid("manifest contains an invalid path"));
    }
    let components = Path::new(&entry.path).components().collect::<Vec<_>>();
    if components.len() > max_depth
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AgentError::invalid(
            "manifest paths must be relative normal paths within max_depth",
        ));
    }
    if entry.mode.is_some_and(|mode| mode > MAX_UNIX_MODE) {
        return Err(AgentError::invalid(format!(
            "manifest mode must be in range 0..={MAX_UNIX_MODE}"
        )));
    }
    match entry.kind.as_str() {
        "file" => {
            let hash = entry
                .sha256
                .as_deref()
                .ok_or_else(|| AgentError::invalid("file entry requires sha256"))?;
            validate_sha256(hash, "entry sha256")?;
        }
        "dir" if entry.size == 0 && entry.sha256.is_none() => {}
        "dir" => {
            return Err(AgentError::invalid(
                "directory entry must have size zero and no sha256",
            ));
        }
        _ => return Err(AgentError::invalid("entry kind must be file or dir")),
    }
    Ok(())
}

fn validate_staged_tree(staging: &Path, entries: &[SyncEntry]) -> AgentResult<()> {
    let expected = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    let mut stack = vec![staging.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for item in fs::read_dir(&directory).map_err(|error| {
            AgentError::io(
                format!("read staged directory {}", directory.display()),
                error,
            )
        })? {
            let item = item.map_err(|error| AgentError::io("read staged entry", error))?;
            let path = item.path();
            let relative = path
                .strip_prefix(staging)
                .expect("walk remains below staging")
                .to_string_lossy()
                .replace('\\', "/");
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| AgentError::io(format!("stat {}", path.display()), error))?;
            if metadata.file_type().is_dir() {
                stack.push(path);
            } else if !metadata.file_type().is_file() {
                return Err(AgentError::invalid(
                    "staging contains a symbolic link or special file",
                ));
            }
            observed.insert(relative);
        }
    }
    if observed != expected {
        return Err(AgentError::invalid(
            "staging contents do not exactly match manifest",
        ));
    }
    for entry in entries {
        let path = join_entry(staging, &entry.path);
        if entry.kind == "dir" {
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| AgentError::io(format!("stat {}", path.display()), error))?;
            if !metadata.file_type().is_dir() {
                return Err(AgentError::invalid("staged directory type mismatch"));
            }
        } else if !verify_regular_file(
            &path,
            entry.size,
            entry.sha256.as_deref().expect("validated file hash"),
        )? {
            return Err(AgentError::invalid(format!(
                "staged file failed size or SHA-256 verification: {}",
                entry.path
            )));
        }
    }
    Ok(())
}

fn write_marker(staging: &Path, request: &SyncPrepareRequest) -> AgentResult<()> {
    let bytes = serde_json::to_vec(request)
        .map_err(|error| AgentError::command(format!("serialize sync manifest: {error}")))?;
    let marker = marker_path(staging);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .and_then(|mut file| file.write_all(&bytes).and_then(|_| file.sync_all()))
        .map_err(|error| AgentError::io(format!("write sync marker {}", marker.display()), error))
}

fn read_marker(staging: &Path) -> AgentResult<SyncPrepareRequest> {
    let marker = marker_path(staging);
    let file = File::open(&marker)
        .map_err(|error| AgentError::io(format!("open sync marker {}", marker.display()), error))?;
    let mut bytes = Vec::new();
    file.take((remote_ops_protocol::DEFAULT_MAX_CONTROL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| AgentError::io(format!("read sync marker {}", marker.display()), error))?;
    if bytes.len() > remote_ops_protocol::DEFAULT_MAX_CONTROL_BYTES {
        return Err(AgentError::invalid(
            "sync marker exceeds control frame limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| AgentError::invalid(format!("invalid sync marker: {error}")))
}

fn remove_staging(staging: &Path) -> AgentResult<()> {
    validate_removable_tree(staging)?;
    fs::remove_dir_all(staging)
        .map_err(|error| AgentError::io(format!("remove staging {}", staging.display()), error))
}

fn validate_removable_tree(root: &Path) -> AgentResult<()> {
    let mut stack = vec![root.to_path_buf()];
    let mut count = 0usize;
    while let Some(path) = stack.pop() {
        count += 1;
        if count > MAX_SYNC_FILES.saturating_mul(2).saturating_add(2) {
            return Err(AgentError::invalid("staging exceeds bounded entry count"));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| AgentError::io(format!("stat {}", path.display()), error))?;
        if metadata.file_type().is_dir() {
            for item in fs::read_dir(&path).map_err(|error| {
                AgentError::io(format!("read directory {}", path.display()), error)
            })? {
                stack.push(
                    item.map_err(|error| AgentError::io("read directory entry", error))?
                        .path(),
                );
            }
        } else if !metadata.file_type().is_file() {
            return Err(AgentError::invalid(
                "refusing to remove staging containing links or special files",
            ));
        }
    }
    Ok(())
}

fn validate_finish_request(request: &SyncFinishRequest) -> AgentResult<()> {
    validate_path_text(&request.remote_path, "remote_path")?;
    validate_path_text(&request.staging_path, "staging_path")?;
    validate_sha256(&request.manifest_sha256, "manifest_sha256")
}

fn validate_staging_relationship(target: &Path, staging: &Path) -> AgentResult<()> {
    if target.parent() != staging.parent()
        || !staging
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".remoteops-sync-"))
    {
        return Err(AgentError::invalid(
            "staging_path is not a managed sibling of remote_path",
        ));
    }
    Ok(())
}

fn marker_path(staging: &Path) -> PathBuf {
    let mut marker = staging.as_os_str().to_os_string();
    marker.push(".manifest");
    PathBuf::from(marker)
}

fn staging_path_for(target: &Path) -> AgentResult<PathBuf> {
    managed_sibling(target, ".remoteops-sync-")
}

fn backup_path_for(target: &Path) -> AgentResult<PathBuf> {
    managed_sibling(target, ".remoteops-backup-")
}

fn managed_sibling(target: &Path, prefix: &str) -> AgentResult<PathBuf> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AgentError::invalid("path must name a non-root UTF-8 entry"))?;
    let mut random = [0u8; 12];
    getrandom::fill(&mut random)
        .map_err(|error| AgentError::command(format!("generate staging name: {error}")))?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(parent.join(format!("{prefix}{name}-{suffix}")))
}

fn join_entry(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

fn manifest_sha256(root_mode: Option<u32>, entries: &[SyncEntry]) -> AgentResult<String> {
    let encoded = serde_json::to_vec(&(root_mode, entries))
        .map_err(|error| AgentError::command(format!("serialize manifest: {error}")))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn apply_directory_modes(
    staging: &Path,
    root_mode: Option<u32>,
    entries: &[SyncEntry],
) -> AgentResult<()> {
    let mut directories = entries
        .iter()
        .filter(|entry| entry.kind == "dir")
        .collect::<Vec<_>>();
    directories.sort_by_key(|entry| std::cmp::Reverse(entry.path.matches('/').count()));
    for entry in directories {
        apply_mode(&join_entry(staging, &entry.path), entry.mode)?;
    }
    apply_mode(staging, root_mode)
}

fn make_tree_removable(staging: &Path, entries: &[SyncEntry]) -> AgentResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for entry in entries.iter().filter(|entry| entry.kind == "dir") {
            let path = join_entry(staging, &entry.path);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|error| {
                AgentError::io(format!("prepare staging cleanup {}", path.display()), error)
            })?;
        }
        fs::set_permissions(staging, fs::Permissions::from_mode(0o700)).map_err(|error| {
            AgentError::io(
                format!("prepare staging cleanup {}", staging.display()),
                error,
            )
        })?;
    }
    #[cfg(not(unix))]
    let _ = (staging, entries);
    Ok(())
}

fn validate_sha256(value: &str, name: &str) -> AgentResult<()> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(AgentError::invalid(format!(
            "{name} must contain exactly 64 hexadecimal characters"
        )))
    }
}

fn validate_path_text(value: &str, name: &str) -> AgentResult<()> {
    if value.is_empty() || value.contains('\0') {
        Err(AgentError::invalid(format!(
            "{name} must not be empty or contain NUL"
        )))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn validate_deploy_paths(releases: &str, current: &str, release: &str) -> AgentResult<()> {
    validate_path_text(releases, "releases_path")?;
    validate_path_text(current, "current_path")?;
    validate_path_text(release, "release_path")?;
    let releases = Path::new(releases);
    let release = Path::new(release);
    if release.parent() != Some(releases) || release.file_name().is_none() {
        return Err(AgentError::invalid(
            "release_path must be a direct child of releases_path",
        ));
    }
    if Path::new(current) == release {
        return Err(AgentError::invalid(
            "current_path and release_path must differ",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn available_bytes(path: &Path) -> AgentResult<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| AgentError::invalid("releases_path must not contain NUL"))?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(AgentError::io(
            "read filesystem free space",
            std::io::Error::last_os_error(),
        ));
    }
    let stats = unsafe { stats.assume_init() };
    let available = u128::from(stats.f_bavail).saturating_mul(u128::from(stats.f_frsize));
    Ok(u64::try_from(available).unwrap_or(u64::MAX))
}

#[cfg(unix)]
fn command_available(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &Path| {
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    let path = Path::new(program);
    if path.components().count() > 1 {
        return executable(path);
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|directory| executable(&directory.join(path)))
        })
        .unwrap_or(false)
}

#[cfg(unix)]
fn switch_symlink(link: &Path, target: &Path) -> AgentResult<()> {
    let parent = link
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = managed_sibling(link, ".remoteops-current-")?;
    std::os::unix::fs::symlink(target, &temporary).map_err(|error| {
        AgentError::io(
            format!("create temporary deployment link {}", temporary.display()),
            error,
        )
    })?;
    if let Err(error) = fs::rename(&temporary, link) {
        let _ = fs::remove_file(&temporary);
        return Err(AgentError::io(
            format!("switch deployment link {}", link.display()),
            error,
        ));
    }
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| AgentError::io(format!("sync directory {}", parent.display()), error))
}

fn apply_mode(path: &Path, mode: Option<u32>) -> AgentResult<()> {
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|error| AgentError::io(format!("chmod {}", path.display()), error))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> AgentResult<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| AgentError::io(format!("sync directory {}", parent.display()), error))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> AgentResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, contents: &[u8]) -> SyncEntry {
        SyncEntry {
            path: path.to_string(),
            kind: "file".to_string(),
            size: contents.len() as u64,
            sha256: Some(format!("{:x}", Sha256::digest(contents))),
            mode: Some(0o644),
        }
    }

    #[test]
    fn sync_prepare_reuses_matching_files_and_commit_keeps_backup() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("same.txt"), "same").unwrap();
        let entries = vec![entry("new.txt", b"new"), entry("same.txt", b"same")];
        let manifest_sha256 = manifest_sha256(Some(0o755), &entries).unwrap();
        let request = SyncPrepareRequest {
            remote_path: target.to_string_lossy().into_owned(),
            manifest_sha256: manifest_sha256.clone(),
            root_mode: Some(0o755),
            entries,
            max_files: 10,
            max_total_bytes: 1024,
            max_depth: 4,
        };
        let prepared = sync_prepare(request).unwrap();
        assert_eq!(prepared["reused_files"], 1);
        let staging = PathBuf::from(prepared["staging_path"].as_str().unwrap());
        fs::write(staging.join("new.txt"), "new").unwrap();
        let committed = sync_commit(SyncFinishRequest {
            remote_path: target.to_string_lossy().into_owned(),
            staging_path: staging.to_string_lossy().into_owned(),
            manifest_sha256,
        })
        .unwrap();
        assert_eq!(fs::read_to_string(target.join("new.txt")).unwrap(), "new");
        assert!(Path::new(committed["backup_path"].as_str().unwrap()).exists());
    }

    #[test]
    fn sync_rejects_parent_components() {
        let entries = vec![entry("../escape", b"no")];
        let request = SyncPrepareRequest {
            remote_path: "target".to_string(),
            manifest_sha256: manifest_sha256(None, &entries).unwrap(),
            root_mode: None,
            entries,
            max_files: 10,
            max_total_bytes: 1024,
            max_depth: 4,
        };
        assert!(
            sync_prepare(request)
                .unwrap_err()
                .message
                .contains("relative")
        );
    }
}
