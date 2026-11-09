use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use remote_ops_protocol::{
    CommandSpec, DeployActivateRequest, DeployPreflightRequest, MAX_RELEASE_ID_BYTES, SyncEntry,
    SyncFinishRequest, SyncPrepareRequest,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::client::{ClientError, RemoteClient, agent_supports_unix_mode};

pub struct SyncOptions {
    pub excludes: Vec<String>,
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_depth: usize,
}

pub struct DeployOptions {
    pub local_path: String,
    pub releases_path: String,
    pub current_path: String,
    pub release_id: String,
    pub expected_arch: Option<String>,
    pub min_free_bytes: u64,
    pub dependencies: Vec<String>,
    pub stop: Option<CommandSpec>,
    pub start: CommandSpec,
    pub health: CommandSpec,
    pub rollback_start: Option<CommandSpec>,
    pub sync: SyncOptions,
}

struct LocalManifest {
    root: PathBuf,
    entries: Vec<SyncEntry>,
    sha256: String,
    root_mode: Option<u32>,
    total_bytes: u64,
    files: usize,
    directories: usize,
}

pub fn sync_directory(
    client: &mut RemoteClient,
    local_path: &str,
    remote_path: &str,
    options: SyncOptions,
) -> Result<Value, ClientError> {
    let include_modes = remote_supports_unix_mode(client)?;
    let manifest = build_manifest(local_path, &options, include_modes)?;
    sync_manifest(client, remote_path, &options, &manifest)
}

pub fn deploy_release(
    client: &mut RemoteClient,
    options: DeployOptions,
) -> Result<Value, ClientError> {
    validate_release_id(&options.release_id)?;
    validate_command(&options.start)?;
    validate_command(&options.health)?;
    if let Some(command) = &options.stop {
        validate_command(command)?;
    }
    if let Some(command) = &options.rollback_start {
        validate_command(command)?;
    }
    let include_modes = remote_supports_unix_mode(client)?;
    let manifest = build_manifest(&options.local_path, &options.sync, include_modes)?;
    let release_path = remote_join(&options.releases_path, &options.release_id);
    let required_bytes = manifest
        .total_bytes
        .checked_add(options.min_free_bytes)
        .ok_or_else(|| ClientError::local("invalid_params", "required byte count overflow"))?;
    let preflight_request = DeployPreflightRequest {
        releases_path: options.releases_path.clone(),
        current_path: options.current_path.clone(),
        release_path: release_path.clone(),
        expected_arch: options.expected_arch,
        required_bytes,
        dependencies: options.dependencies,
    };
    let preflight = client.call(
        "deploy_preflight",
        serde_json::to_value(preflight_request).expect("serializable deploy preflight"),
    ).map_err(|error|error.with_recovery(json!({"tool":"deploy_release","phase":"preflight","release_path":release_path,"current_path":options.current_path})))?;
    let sync = sync_manifest(client, &release_path, &options.sync, &manifest)?;
    let activation_request = DeployActivateRequest {
        release_path: release_path.clone(),
        current_path: options.current_path.clone(),
        stop: options.stop,
        start: options.start,
        health: options.health,
        rollback_start: options.rollback_start,
    };
    let activation = client.call(
        "deploy_activate",
        serde_json::to_value(activation_request).expect("serializable deployment activation"),
    ).map_err(|error|error.with_recovery(json!({"tool":"deploy_release","phase":"activation","release_path":release_path,"current_path":options.current_path,"sync":sync})))?;
    Ok(json!({
        "status": activation["status"],
        "deployed": activation["deployed"],
        "rolled_back": activation["rolled_back"],
        "release_id": options.release_id,
        "release_path": release_path,
        "current_path": options.current_path,
        "manifest_sha256": manifest.sha256,
        "preflight": preflight,
        "sync": sync,
        "activation": activation
    }))
}

fn sync_manifest(
    client: &mut RemoteClient,
    remote_path: &str,
    options: &SyncOptions,
    manifest: &LocalManifest,
) -> Result<Value, ClientError> {
    let prepare_request = SyncPrepareRequest {
        remote_path: remote_path.to_string(),
        manifest_sha256: manifest.sha256.clone(),
        root_mode: manifest.root_mode,
        entries: manifest.entries.clone(),
        max_files: options.max_files,
        max_total_bytes: options.max_total_bytes,
        max_depth: options.max_depth,
    };
    let arguments = serde_json::to_value(&prepare_request).expect("serializable sync request");
    crate::client::validate_request("sync_prepare", &arguments)?;
    let prepared=client.call("sync_prepare",arguments).map_err(|error|error.with_recovery(json!({"tool":"sync_directory","phase":"prepare","remote_path":remote_path,"manifest_sha256":manifest.sha256,"cleanup":{"status":"not_attempted"}})))?;
    let staging_path = prepared["staging_path"]
        .as_str()
        .ok_or_else(|| ClientError::local("protocol", "sync_prepare omitted staging_path"))?
        .to_string();
    validate_staging_reply(remote_path, &staging_path)?;
    let finish = SyncFinishRequest {
        remote_path: remote_path.to_string(),
        staging_path: staging_path.clone(),
        manifest_sha256: manifest.sha256.clone(),
    };
    let mut phase = "prepare";
    let result = (|| {
        let required = prepared["required_uploads"].as_array().ok_or_else(|| {
            ClientError::local("protocol", "sync_prepare omitted required_uploads")
        })?;
        let files = manifest
            .entries
            .iter()
            .filter(|entry| entry.kind == "file")
            .map(|entry| (entry.path.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        // Validate the entire upload plan before sending any bytes.
        for item in required {
            let relative = item.as_str().ok_or_else(|| {
                ClientError::local("protocol", "required upload path is not a string")
            })?;
            if !seen.insert(relative) || !files.contains_key(relative) {
                return Err(ClientError::local(
                    "protocol",
                    "sync_prepare returned a duplicate path or a path outside the local manifest",
                ));
            }
        }
        let mut transferred_files = 0usize;
        let mut transferred_bytes = 0u64;
        phase = "upload";
        for item in required {
            let relative = item.as_str().expect("validated upload path");
            let local = join_local_entry(&manifest.root, relative);
            let remote = remote_join(&staging_path, relative);
            let upload = client.upload(&local.to_string_lossy(), &remote, false, None, true)?;
            let bytes = upload["bytes_transferred"].as_u64().ok_or_else(|| {
                ClientError::local("protocol", "upload response omitted byte count")
            })?;
            transferred_bytes = transferred_bytes
                .checked_add(bytes)
                .ok_or_else(|| ClientError::local("protocol", "transferred byte count overflow"))?;
            transferred_files += 1;
        }
        phase = "commit";
        let committed = client.call(
            "sync_commit",
            serde_json::to_value(&finish).expect("serializable sync finish"),
        )?;
        if committed["committed"] != true {
            return Err(ClientError::from(remote_ops_protocol::RemoteError {
                kind: committed["error"]["kind"]
                    .as_str()
                    .unwrap_or("sync_commit_failed")
                    .to_string(),
                message: committed["error"]["message"]
                    .as_str()
                    .unwrap_or("sync_commit did not confirm activation")
                    .to_string(),
            })
            .with_recovery(json!({"commit":committed})));
        }
        Ok(json!({
            "committed":true,"local_path":manifest.root.to_string_lossy(),"remote_path":remote_path,
            "manifest_sha256":manifest.sha256,"files":manifest.files,"directories":manifest.directories,"total_bytes":manifest.total_bytes,
            "files_transferred":transferred_files,"bytes_transferred":transferred_bytes,"files_reused":prepared["reused_files"],"bytes_reused":prepared["reused_bytes"],
            "backup_path":committed["backup_path"],"staging_path":staging_path,
            "commit":committed
        }))
    })();
    result.map_err(|error:ClientError| {
        // An unconfirmed commit or upload may still be executing on the Agent.
        // Keep its staging/marker intact; never reconnect merely to abort it.
        let cleanup=if phase!="commit" && !error.halts_workflow() {
            match client.call("sync_abort",serde_json::to_value(&finish).expect("serializable sync finish")) {
                Ok(value) if value["aborted"]==true=>json!({"status":"removed"}),
                Ok(value)=>json!({"status":"unconfirmed","response":value}),
                Err(cleanup_error)=>json!({"status":"unconfirmed","error":cleanup_error.details()}),
            }
        } else {json!({"status":"not_attempted"})};
        error.with_recovery(json!({"tool":"sync_directory","phase":phase,"remote_path":remote_path,"staging_path":staging_path,"manifest_sha256":manifest.sha256,"cleanup":cleanup}))
    })
}

fn build_manifest(
    local_path: &str,
    options: &SyncOptions,
    include_modes: bool,
) -> Result<LocalManifest, ClientError> {
    validate_local_path(local_path)?;
    let root = PathBuf::from(local_path);
    let metadata = fs::symlink_metadata(&root)
        .map_err(|error| ClientError::local("io", format!("stat {local_path}: {error}")))?;
    if !metadata.file_type().is_dir() {
        return Err(ClientError::local(
            "invalid_params",
            "local_path must be a directory and not a symlink",
        ));
    }
    let excludes = compile_excludes(&options.excludes)?;
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;
    let mut files = 0usize;
    let mut directories = 0usize;
    let mut stack = vec![(root.clone(), 0usize)];
    while let Some((directory, depth)) = stack.pop() {
        let mut children = fs::read_dir(&directory)
            .map_err(|error| {
                ClientError::local(
                    "io",
                    format!("read directory {}: {error}", directory.display()),
                )
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ClientError::local("io", format!("read directory entry: {error}")))?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children.into_iter().rev() {
            let path = child.path();
            let relative = manifest_path(&root, &path)?;
            if excludes.is_match(&relative) {
                continue;
            }
            let child_depth = depth + 1;
            if child_depth > options.max_depth {
                return Err(ClientError::local(
                    "invalid_params",
                    format!("local directory exceeds max_depth at {relative}"),
                ));
            }
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                ClientError::local("io", format!("stat {}: {error}", path.display()))
            })?;
            if metadata.file_type().is_dir() {
                directories += 1;
                entries.push(SyncEntry {
                    path: relative,
                    kind: "dir".to_string(),
                    size: 0,
                    sha256: None,
                    mode: manifest_mode(include_modes, &metadata),
                });
                stack.push((path, child_depth));
            } else if metadata.file_type().is_file() {
                total_bytes = total_bytes.checked_add(metadata.len()).ok_or_else(|| {
                    ClientError::local("invalid_params", "manifest byte count overflow")
                })?;
                if total_bytes > options.max_total_bytes {
                    return Err(ClientError::local(
                        "invalid_params",
                        "local directory exceeds max_total_bytes",
                    ));
                }
                let sha256 = hash_local_file(&path, metadata.len())?;
                files += 1;
                entries.push(SyncEntry {
                    path: relative,
                    kind: "file".to_string(),
                    size: metadata.len(),
                    sha256: Some(sha256),
                    mode: manifest_mode(include_modes, &metadata),
                });
            } else {
                return Err(ClientError::local(
                    "invalid_params",
                    format!(
                        "local directory contains a symbolic link or special file: {}",
                        path.display()
                    ),
                ));
            }
            if entries.len() > options.max_files {
                return Err(ClientError::local(
                    "invalid_params",
                    "local directory exceeds max_files",
                ));
            }
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let root_mode = manifest_mode(include_modes, &metadata);
    let encoded = serde_json::to_vec(&(root_mode, &entries))
        .map_err(|error| ClientError::local("invalid_params", error.to_string()))?;
    let sha256 = format!("{:x}", Sha256::digest(encoded));
    Ok(LocalManifest {
        root,
        entries,
        sha256,
        root_mode,
        total_bytes,
        files,
        directories,
    })
}

fn compile_excludes(patterns: &[String]) -> Result<GlobSet, ClientError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|error| {
            ClientError::local("invalid_params", format!("invalid exclude glob: {error}"))
        })?);
    }
    builder
        .build()
        .map_err(|error| ClientError::local("invalid_params", error.to_string()))
}

fn manifest_path(root: &Path, path: &Path) -> Result<String, ClientError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ClientError::local("invalid_params", "manifest path escaped local root"))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str().ok_or_else(|| {
                ClientError::local("invalid_params", "local paths must contain valid UTF-8")
            })?),
            _ => {
                return Err(ClientError::local(
                    "invalid_params",
                    "manifest paths must contain only normal components",
                ));
            }
        }
    }
    Ok(parts.join("/"))
}

fn join_local_entry(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

fn remote_join(root: &str, relative: &str) -> String {
    format!("{}/{}", root.trim_end_matches(['/', '\\']), relative)
}

fn validate_staging_reply(remote_path: &str, staging: &str) -> Result<(), ClientError> {
    let normalized = remote_path.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    let (parent, name) = normalized.rsplit_once('/').unwrap_or(("", normalized));
    let staged = staging.replace('\\', "/");
    let (stage_parent, stage_name) = staged.rsplit_once('/').unwrap_or(("", staged.as_str()));
    let parent = if parent.is_empty() {
        if normalized.starts_with('/') {
            "/"
        } else {
            "."
        }
    } else {
        parent
    };
    let stage_parent = if stage_parent.is_empty() {
        if staged.starts_with('/') { "/" } else { "." }
    } else {
        stage_parent
    };
    let expected = format!(".remoteops-sync-{name}-");
    let suffix = stage_name.strip_prefix(&expected);
    if parent != stage_parent
        || !suffix.is_some_and(|suffix| {
            suffix.len() == 24 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(ClientError::local(
            "protocol",
            "sync_prepare returned a staging path outside its managed sibling namespace",
        ));
    }
    Ok(())
}

fn hash_local_file(path: &Path, expected_size: u64) -> Result<String, ClientError> {
    let mut file = File::open(path)
        .map_err(|error| ClientError::local("io", format!("open {}: {error}", path.display())))?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            ClientError::local("io", format!("read {}: {error}", path.display()))
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > expected_size {
            return Err(ClientError::local(
                "invalid_params",
                format!("local file changed while hashing: {}", path.display()),
            ));
        }
        digest.update(&buffer[..read]);
    }
    if total != expected_size {
        return Err(ClientError::local(
            "invalid_params",
            format!("local file changed while hashing: {}", path.display()),
        ));
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_local_path(path: &str) -> Result<(), ClientError> {
    if path.is_empty() || path.contains('\0') {
        Err(ClientError::local(
            "invalid_params",
            "local_path must not be empty or contain NUL",
        ))
    } else {
        Ok(())
    }
}

fn remote_supports_unix_mode(client: &mut RemoteClient) -> Result<bool, ClientError> {
    let agent_info = client.call("agent_info", serde_json::json!({}))?;
    Ok(agent_supports_unix_mode(&agent_info))
}

fn manifest_mode(include_modes: bool, metadata: &fs::Metadata) -> Option<u32> {
    if include_modes {
        local_mode(metadata)
    } else {
        None
    }
}

fn validate_release_id(release_id: &str) -> Result<(), ClientError> {
    if release_id.is_empty()
        || release_id.len() > MAX_RELEASE_ID_BYTES
        || matches!(release_id, "." | "..")
        || !release_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Err(ClientError::local(
            "invalid_params",
            "release_id must use 1..=128 ASCII letters, digits, '.', '_', or '-'",
        ))
    } else {
        Ok(())
    }
}

fn validate_command(command: &CommandSpec) -> Result<(), ClientError> {
    if command.program.is_empty() || command.program.contains('\0') {
        return Err(ClientError::local(
            "invalid_params",
            "deployment command program must not be empty or contain NUL",
        ));
    }
    if command.timeout_ms > 300_000 {
        return Err(ClientError::local(
            "invalid_params",
            "deployment command timeout_ms must be in range 0..=300000",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn local_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.mode() & remote_ops_protocol::MAX_UNIX_MODE)
}

#[cfg(not(unix))]
fn local_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_reply_must_use_the_same_parent_and_target_bound_random_name() {
        assert!(
            validate_staging_reply(
                "/target",
                "/.remoteops-sync-target-0123456789abcdef01234567"
            )
            .is_ok()
        );
        assert!(
            validate_staging_reply("/target", ".remoteops-sync-target-0123456789abcdef01234567")
                .is_err()
        );
        assert!(
            validate_staging_reply(
                "target",
                "./.remoteops-sync-target-0123456789abcdef01234567"
            )
            .is_ok()
        );
        assert!(
            validate_staging_reply(
                "C:\\data\\target",
                "C:/data/.remoteops-sync-target-0123456789abcdef01234567"
            )
            .is_ok()
        );
        assert!(
            validate_staging_reply(
                "/tmp/target",
                "/tmp/.remoteops-sync-other-0123456789abcdef01234567"
            )
            .is_err()
        );
    }
    #[test]
    fn manifest_is_sorted_hashed_and_excludes_subtrees() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("src/nested")).unwrap();
        fs::create_dir_all(directory.path().join("target/cache")).unwrap();
        fs::write(directory.path().join("src/main"), "main").unwrap();
        fs::write(directory.path().join("src/nested/data"), "data").unwrap();
        fs::write(directory.path().join("target/cache/file"), "ignored").unwrap();
        let manifest = build_manifest(
            &directory.path().to_string_lossy(),
            &SyncOptions {
                excludes: vec!["target".to_string()],
                max_files: 20,
                max_total_bytes: 1024,
                max_depth: 5,
            },
            true,
        )
        .unwrap();
        assert_eq!(manifest.files, 2);
        assert!(
            manifest
                .entries
                .windows(2)
                .all(|pair| pair[0].path < pair[1].path)
        );
        assert!(
            manifest
                .entries
                .iter()
                .all(|entry| !entry.path.starts_with("target"))
        );
        assert_eq!(manifest.sha256.len(), 64);
    }

    #[test]
    fn manifest_omits_modes_when_remote_does_not_support_unix_mode() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("nested/data"), "data").unwrap();
        fs::write(directory.path().join("main"), "main").unwrap();
        let manifest = build_manifest(
            &directory.path().to_string_lossy(),
            &SyncOptions {
                excludes: Vec::new(),
                max_files: 20,
                max_total_bytes: 1024,
                max_depth: 5,
            },
            false,
        )
        .unwrap();
        assert!(manifest.root_mode.is_none());
        assert!(manifest.entries.iter().all(|entry| entry.mode.is_none()));
    }

    #[test]
    fn unix_mode_support_follows_remote_platform_family() {
        assert!(agent_supports_unix_mode(
            &json!({"platform": {"family": "unix"}})
        ));
        assert!(!agent_supports_unix_mode(
            &json!({"platform": {"family": "windows"}})
        ));
        assert!(!agent_supports_unix_mode(&json!({})));
    }

    #[test]
    fn release_ids_cannot_escape_releases_directory() {
        assert!(validate_release_id("v1.2.3").is_ok());
        assert!(validate_release_id("../escape").is_err());
        assert!(validate_release_id("a/b").is_err());
    }
}
