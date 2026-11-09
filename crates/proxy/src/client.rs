use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use remote_ops_protocol::{
    BUILTIN_PSK, DEFAULT_CHUNK_BYTES, DEFAULT_CONNECT_TIMEOUT, DEFAULT_HEALTH_CHECK_AFTER,
    DEFAULT_HEALTH_CHECK_TIMEOUT, DEFAULT_MAX_CONTROL_BYTES, DEFAULT_TRANSFER_IDLE_TIMEOUT,
    DownloadRequest, FrameType, INTERNAL_PING_OPERATION, RemoteError, RemoteRequest,
    RemoteResponse, Session, SessionOptions, TransferEnd, TransferMetadata, UploadRequest,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod error;
pub use error::ClientError;

#[derive(Debug, Clone)]
enum LifecycleState {
    Ready,
    Rebooting {
        previous_instance_id: Option<String>,
    },
    Updating {
        previous_instance_id: Option<String>,
    },
}

impl LifecycleState {
    fn name(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Rebooting { .. } => "rebooting",
            Self::Updating { .. } => "updating",
        }
    }

    fn previous_instance_id(&self) -> Option<&str> {
        match self {
            Self::Ready => None,
            Self::Rebooting {
                previous_instance_id,
            }
            | Self::Updating {
                previous_instance_id,
            } => previous_instance_id.as_deref(),
        }
    }
}

pub struct RemoteClient {
    remote: SocketAddrV4,
    timeout: Duration,
    max_transfer_bytes: u64,
    session: Option<Session>,
    next_request_id: u64,
    operation_attempts: u64,
    last_activity: Option<Instant>,
    health_check_after: Duration,
    last_success_at_ms: Option<u64>,
    last_error: Option<Value>,
    last_probe: Option<Value>,
    last_agent_info: Option<Value>,
    lifecycle_state: LifecycleState,
}

impl RemoteClient {
    pub fn new(remote: SocketAddrV4, timeout: Duration, max_transfer_bytes: u64) -> Self {
        Self {
            remote,
            timeout,
            max_transfer_bytes,
            session: None,
            next_request_id: 1,
            operation_attempts: 0,
            last_activity: None,
            health_check_after: DEFAULT_HEALTH_CHECK_AFTER,
            last_success_at_ms: None,
            last_error: None,
            last_probe: None,
            last_agent_info: None,
            lifecycle_state: LifecycleState::Ready,
        }
    }

    pub fn remote_status(&self) -> Value {
        json!({
            "ip": self.remote.ip().to_string(),
            "port": self.remote.port(),
            "address": self.remote.to_string(),
            "connected": self.session.is_some(),
            "connection_state": if self.session.is_some() { "cached" } else { "disconnected" },
            "lifecycle_state": self.lifecycle_state.name(),
            "last_success_at_ms": self.last_success_at_ms,
            "last_error": self.last_error,
            "last_probe": self.last_probe,
            "agent_info": self.last_agent_info,
        })
    }

    pub fn set_remote(
        &mut self,
        ip: Option<Ipv4Addr>,
        port: Option<u16>,
    ) -> Result<Value, ClientError> {
        if ip.is_none() && port.is_none() {
            return Err(ClientError::local(
                "invalid_params",
                "at least one of ip or port must be provided",
            ));
        }
        if port == Some(0) {
            return Err(ClientError::local(
                "invalid_params",
                "port must be in range 1..=65535",
            ));
        }
        let remote = SocketAddrV4::new(
            ip.unwrap_or(*self.remote.ip()),
            port.unwrap_or(self.remote.port()),
        );
        let address_changed = remote != self.remote;
        self.remote = remote;
        // Re-pointing always drops the cached session so the next operation dials
        // a fresh connection. This is the deterministic recovery lever for a
        // wedged or half-open session, so it must not depend on the address
        // actually changing. Observation and lifecycle state are preserved when
        // the address is unchanged, so in-flight reboot/update orchestration and
        // instance-change detection survive the reset.
        self.session = None;
        self.last_activity = None;
        if address_changed {
            self.last_success_at_ms = None;
            self.last_error = None;
            self.last_probe = None;
            self.last_agent_info = None;
            self.lifecycle_state = LifecycleState::Ready;
        }
        Ok(self.remote_status())
    }

    pub fn call(&mut self, operation: &str, arguments: Value) -> Result<Value, ClientError> {
        let payload = encode_request(operation, &arguments)?;
        self.prepare_connection()?;
        let request_id = self.allocate_request_id()?;
        self.operation_attempts = self.operation_attempts.saturating_add(1);
        let result = (|| {
            let session = self.session.as_mut().expect("connected");
            session
                .send(FrameType::Request, request_id, &payload)
                .map_err(protocol_error)?;
            receive_remote_response(session, request_id)
        })();
        let result = self.finish_operation(result.map_err(ClientError::during_request), true);
        if operation == "agent_info"
            && let Ok(agent_info) = &result
        {
            self.last_agent_info = Some(agent_info.clone());
        }
        result
    }

    pub(crate) fn operation_attempts(&self) -> u64 {
        self.operation_attempts
    }

    pub fn remote_probe(&mut self, timeout_ms: u64) -> Value {
        self.probe(Duration::from_millis(timeout_ms))
    }

    pub fn wait_remote(
        &mut self,
        wait_for: Option<&str>,
        timeout_ms: u64,
        poll_interval_ms: u64,
        probe_timeout_ms: u64,
    ) -> Value {
        let effective_wait = wait_for.unwrap_or(match self.lifecycle_state {
            LifecycleState::Ready => "online",
            LifecycleState::Rebooting { .. } | LifecycleState::Updating { .. } => {
                "offline_then_online"
            }
        });
        let previous_instance_id = self
            .lifecycle_state
            .previous_instance_id()
            .map(str::to_string)
            .or_else(|| {
                self.last_agent_info
                    .as_ref()
                    .and_then(agent_instance_id)
                    .map(str::to_string)
            });
        let started = Instant::now();
        let deadline = started + Duration::from_millis(timeout_ms);
        let mut attempts = 0u64;
        let mut observed_offline = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let attempt_timeout = Duration::from_millis(probe_timeout_ms).min(remaining);
            if attempt_timeout.is_zero() {
                return self.wait_remote_result(
                    effective_wait,
                    false,
                    true,
                    observed_offline,
                    attempts,
                    started,
                );
            }
            attempts += 1;
            let probe = self.probe(attempt_timeout);
            let reachable = probe["reachable"].as_bool().unwrap_or(false);
            if !reachable {
                observed_offline = true;
            }
            let current_instance_id = probe["agent_info"]["runtime"]["instance_id"].as_str();
            let instance_changed = previous_instance_id
                .as_deref()
                .zip(current_instance_id)
                .is_some_and(|(previous, current)| previous != current);
            let reached = match effective_wait {
                "online" => reachable,
                "offline" => !reachable,
                "offline_then_online" => reachable && (observed_offline || instance_changed),
                _ => false,
            };
            if reached {
                if effective_wait == "offline_then_online"
                    || instance_changed
                    || matches!(self.lifecycle_state, LifecycleState::Ready)
                {
                    self.lifecycle_state = LifecycleState::Ready;
                }
                return self.wait_remote_result(
                    effective_wait,
                    true,
                    false,
                    observed_offline,
                    attempts,
                    started,
                );
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return self.wait_remote_result(
                    effective_wait,
                    false,
                    true,
                    observed_offline,
                    attempts,
                    started,
                );
            }
            thread::sleep(Duration::from_millis(poll_interval_ms).min(remaining));
        }
    }

    pub fn reboot(&mut self, delay_ms: u64) -> Result<Value, ClientError> {
        let requested_at_ms = unix_time_ms();
        let fallback_instance = self
            .last_agent_info
            .as_ref()
            .and_then(agent_instance_id)
            .map(str::to_string);
        let response = self.call("reboot", json!({"delay_ms": delay_ms}));
        let (acknowledged, disconnect_observed, agent_response, previous_instance_id) =
            match response {
                Ok(response) => {
                    let previous = response["previous_instance_id"]
                        .as_str()
                        .map(str::to_string)
                        .or(fallback_instance);
                    if let Some(agent) = response.get("agent") {
                        self.last_agent_info = Some(agent.clone());
                    }
                    (true, false, Some(response), previous)
                }
                Err(error) if error.disconnected_during_request() => {
                    (false, true, None, fallback_instance)
                }
                Err(error) => return Err(error),
            };
        self.session = None;
        self.last_activity = None;
        self.lifecycle_state = LifecycleState::Rebooting {
            previous_instance_id: previous_instance_id.clone(),
        };
        Ok(json!({
            "accepted": true,
            "acknowledged": acknowledged,
            "disconnect_observed": disconnect_observed,
            "requested_at_ms": requested_at_ms,
            "delay_ms": delay_ms,
            "previous_instance_id": previous_instance_id,
            "lifecycle_state": self.lifecycle_state.name(),
            "agent_response": agent_response
        }))
    }

    pub fn agent_update(
        &mut self,
        local_path: &str,
        timeout_ms: u64,
        poll_interval_ms: u64,
        probe_timeout_ms: u64,
    ) -> Result<Value, ClientError> {
        let started = Instant::now();
        let probe = self.remote_probe(probe_timeout_ms);
        if !probe["reachable"].as_bool().unwrap_or(false) {
            return Err(probe["error"]
                .as_object()
                .map(|error| {
                    ClientError::local(
                        error
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or("connection_unavailable"),
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("remote probe failed"),
                    )
                })
                .unwrap_or_else(|| {
                    ClientError::local("connection_unavailable", "remote probe failed")
                }));
        }
        let previous_agent = probe["agent_info"].clone();
        let previous_instance_id = agent_instance_id(&previous_agent).map(str::to_string);
        let staging_path = previous_agent["update"]["staging_path"]
            .as_str()
            .ok_or_else(|| {
                ClientError::local(
                    "unsupported",
                    "remote agent did not provide an update staging path",
                )
            })?
            .to_string();
        if previous_agent["capabilities"]["self_update"] != true
            || previous_agent["capabilities"]["update_receipts"] != true
        {
            return Err(ClientError::local(
                "unsupported",
                "remote agent does not support verified self-update receipts; deploy a matching Agent before using agent_update",
            ));
        }
        let upload_mode = agent_supports_unix_mode(&previous_agent).then_some(0o755);
        let upload = self.upload(local_path, &staging_path, true, upload_mode, true)?;
        let sha256 = upload["sha256"]
            .as_str()
            .ok_or_else(|| ClientError::local("protocol", "upload response omitted SHA-256"))?
            .to_string();
        let prepare = self.call("agent_update_prepare", json!({"expected_sha256": sha256}));
        let (restart_acknowledged, prepared) = match prepare {
            Ok(value) => (true, Some(value)),
            Err(error) if error.disconnected_during_request() => (false, None),
            Err(error) => return Err(error),
        };
        self.session = None;
        self.last_activity = None;
        self.lifecycle_state = LifecycleState::Updating {
            previous_instance_id: previous_instance_id.clone(),
        };
        let wait_started = Instant::now();
        let mut wait = self.wait_remote(
            Some("offline_then_online"),
            timeout_ms,
            poll_interval_ms,
            probe_timeout_ms,
        );
        let candidate = prepared
            .as_ref()
            .and_then(|value| value.get("candidate"))
            .cloned();
        let expected_attempt = prepared
            .as_ref()
            .and_then(|prepared| prepared["attempt_id"].as_str());
        self.lifecycle_state = LifecycleState::Updating {
            previous_instance_id: previous_instance_id.clone(),
        };
        // Being online may be the helper's temporary candidate instance. Only
        // its final receipt proves stabilization and supervisor handover.
        while wait["reached"] == true {
            let receipt = &wait["agent_info"]["update"]["last_result"];
            if update_receipt_matches(
                receipt,
                &sha256,
                previous_instance_id.as_deref(),
                expected_attempt,
            ) && receipt["status"] != "pending"
            {
                break;
            }
            let remaining =
                Duration::from_millis(timeout_ms).saturating_sub(wait_started.elapsed());
            if remaining.is_zero() {
                wait["timed_out"] = json!(true);
                break;
            }
            thread::sleep(Duration::from_millis(poll_interval_ms).min(remaining));
            let remaining =
                Duration::from_millis(timeout_ms).saturating_sub(wait_started.elapsed());
            if remaining.is_zero() {
                wait["timed_out"] = json!(true);
                break;
            }
            let probe = self.probe(Duration::from_millis(probe_timeout_ms).min(remaining));
            wait["last_probe"] = probe.clone();
            if probe["reachable"] == true {
                wait["agent_info"] = probe["agent_info"].clone();
            }
        }
        let current_agent = wait["agent_info"].clone();
        let receipt = current_agent["update"]["last_result"].clone();
        let matched = update_receipt_matches(
            &receipt,
            &sha256,
            previous_instance_id.as_deref(),
            expected_attempt,
        );
        let status = if wait["timed_out"] == true || wait["reached"] != true {
            "timed_out"
        } else if matched
            && receipt["status"] == "updated"
            && candidate
                .as_ref()
                .is_none_or(|candidate| candidate_matches_agent(candidate, &current_agent))
        {
            "updated"
        } else if matched
            && receipt["status"] == "rolled_back"
            && candidate_matches_agent(&previous_agent, &current_agent)
        {
            "rolled_back"
        } else {
            "unconfirmed"
        };
        if matches!(status, "updated" | "rolled_back") {
            self.lifecycle_state = LifecycleState::Ready;
        }
        wait["elapsed_ms"] = json!(duration_ms(wait_started.elapsed()));
        wait["lifecycle_state"] = json!(self.lifecycle_state.name());
        wait["connected"] = json!(self.session.is_some());
        wait["connection_state"] = json!(if self.session.is_some() {
            "cached"
        } else {
            "disconnected"
        });
        Ok(json!({
            "status": status,
            "updated": status == "updated",
            "rolled_back": status == "rolled_back",
            "timed_out": status == "timed_out",
            "restart_acknowledged": restart_acknowledged,
            "previous_agent": previous_agent,
            "candidate": candidate,
            "current_agent": current_agent,
            "update_receipt":receipt,
            "recovery":prepared.as_ref().and_then(|prepared|prepared.get("recovery")).cloned(),
            "staging_path": staging_path,
            "bytes_transferred": upload["bytes_transferred"],
            "sha256": sha256,
            "wait": wait,
            "elapsed_ms": duration_ms(started.elapsed())
        }))
    }

    pub fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        overwrite: bool,
        mode: Option<u32>,
        resume: bool,
    ) -> Result<Value, ClientError> {
        if local_path.is_empty() || remote_path.is_empty() {
            return Err(ClientError::local(
                "invalid_params",
                "local_path and remote_path must not be empty",
            ));
        }
        let path_metadata = std::fs::symlink_metadata(local_path)
            .map_err(|error| ClientError::local("io", format!("stat {local_path}: {error}")))?;
        if !path_metadata.file_type().is_file() {
            return Err(ClientError::local(
                "invalid_params",
                "local_path must be a regular file and not a symlink",
            ));
        }
        let mut file = File::open(local_path)
            .map_err(|error| ClientError::local("io", format!("open {local_path}: {error}")))?;
        let metadata = file
            .metadata()
            .map_err(|error| ClientError::local("io", format!("stat {local_path}: {error}")))?;
        let size = metadata.len();
        if size > self.max_transfer_bytes {
            return Err(ClientError::local(
                "invalid_params",
                format!("file exceeds transfer limit ({})", self.max_transfer_bytes),
            ));
        }
        if mode.is_some_and(|mode| mode > remote_ops_protocol::MAX_UNIX_MODE) {
            return Err(ClientError::local(
                "invalid_params",
                "mode must be in range 0..=4095",
            ));
        }
        let sha256 = hash_file(&mut file, local_path, size)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| ClientError::local("io", format!("seek {local_path}: {error}")))?;
        let payload = encode_request(
            "upload_file",
            &serde_json::to_value(UploadRequest {
                remote_path: remote_path.to_string(),
                size,
                sha256: sha256.clone(),
                overwrite,
                resume,
                mode,
            })
            .expect("serializable"),
        )?;
        self.prepare_connection()?;
        let request_id = self.allocate_request_id()?;
        self.operation_attempts = self.operation_attempts.saturating_add(1);
        let result = (|| {
            let session = self.session.as_mut().expect("connected");
            session
                .send(FrameType::Request, request_id, &payload)
                .map_err(protocol_error)?;
            let metadata = receive_transfer_metadata(session, request_id, size)?;
            if metadata.offset > size {
                return Err(ClientError::local(
                    "protocol",
                    "remote upload offset exceeds local file size",
                ));
            }
            let (mut digest, prefix_sha256) =
                hash_local_prefix(&mut file, metadata.offset, local_path)?;
            if metadata.offset > 0
                && !metadata
                    .prefix_sha256
                    .as_deref()
                    .is_some_and(|remote| remote.eq_ignore_ascii_case(&prefix_sha256))
            {
                return Err(ClientError::local(
                    "protocol",
                    "remote upload prefix SHA-256 does not match local file",
                ));
            }
            file.seek(SeekFrom::Start(metadata.offset))
                .map_err(|error| ClientError::local("io", format!("seek {local_path}: {error}")))?;
            let mut total = metadata.offset;
            let mut buffer = vec![0u8; DEFAULT_CHUNK_BYTES];
            loop {
                let read = file.read(&mut buffer).map_err(|error| {
                    ClientError::local("io", format!("read {local_path}: {error}"))
                })?;
                if read == 0 {
                    break;
                }
                session
                    .send(FrameType::Chunk, request_id, &buffer[..read])
                    .map_err(protocol_error)?;
                digest.update(&buffer[..read]);
                total += read as u64;
            }
            let actual_sha256 = format!("{:x}", digest.finalize());
            if !actual_sha256.eq_ignore_ascii_case(&sha256) {
                return Err(ClientError::local(
                    "invalid_params",
                    "local file changed while it was being uploaded",
                ));
            }
            session
                .send_json(
                    FrameType::End,
                    request_id,
                    &TransferEnd {
                        size: total,
                        sha256: actual_sha256.clone(),
                    },
                )
                .map_err(protocol_error)?;
            let remote = receive_remote_response(session, request_id)?;
            Ok(json!({
                "bytes_transferred": remote["bytes_transferred"],
                "size": remote["size"],
                "resumed_from": remote["resumed_from"],
                "sha256": remote["sha256"],
                "mode": remote["mode"],
                "source": local_path,
                "destination": remote_path
            }))
        })();
        self.finish_operation(result.map_err(ClientError::during_request), false)
    }

    pub fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        overwrite: bool,
        resume: bool,
    ) -> Result<Value, ClientError> {
        if local_path.is_empty() || remote_path.is_empty() {
            return Err(ClientError::local(
                "invalid_params",
                "local_path and remote_path must not be empty",
            ));
        }
        let target = Path::new(local_path);
        if target.exists() && !overwrite {
            return Err(ClientError::local(
                "invalid_params",
                format!("destination already exists: {local_path}"),
            ));
        }
        if let Ok(metadata) = std::fs::symlink_metadata(target)
            && !metadata.file_type().is_file()
        {
            return Err(ClientError::local(
                "invalid_params",
                "local_path must be a regular file and not a symlink",
            ));
        }
        let partial = download_partial_path(target)?;
        let mut cleanup = DownloadPartialCleanup {
            path: partial.clone(),
            keep: true,
            committed: false,
        };
        let mut temp = open_download_partial(&partial, resume)?;
        cleanup.keep = resume;
        let requested_offset = temp
            .metadata()
            .map_err(|error| {
                ClientError::local("io", format!("stat {}: {error}", partial.display()))
            })?
            .len();
        if requested_offset > self.max_transfer_bytes {
            return Err(ClientError::local(
                "invalid_params",
                "download partial file exceeds transfer limit",
            ));
        }
        let (prefix_digest, prefix_sha256) =
            hash_local_prefix(&mut temp, requested_offset, &partial.to_string_lossy())?;
        let payload = encode_request(
            "download_file",
            &serde_json::to_value(DownloadRequest {
                remote_path: remote_path.to_string(),
                offset: requested_offset,
                prefix_sha256: (requested_offset > 0).then_some(prefix_sha256.clone()),
            })
            .expect("serializable"),
        )?;
        self.prepare_connection()?;
        let request_id = self.allocate_request_id()?;
        self.operation_attempts = self.operation_attempts.saturating_add(1);
        let result = (|| {
            let session = self.session.as_mut().expect("connected");
            session
                .send(FrameType::Request, request_id, &payload)
                .map_err(protocol_error)?;
            let first = session.receive().map_err(protocol_error)?;
            check_request_id(&first, request_id)?;
            if first.kind == FrameType::Error {
                return Err(serde_json::from_slice::<RemoteError>(&first.payload)
                    .map_err(|error| ClientError::local("protocol", error.to_string()))?
                    .into());
            }
            if first.kind != FrameType::Response {
                return Err(ClientError::local("protocol", "expected download metadata"));
            }
            let metadata: TransferMetadata = serde_json::from_slice(&first.payload)
                .map_err(|error| ClientError::local("protocol", error.to_string()))?;
            if metadata.size > self.max_transfer_bytes {
                return Err(ClientError::local(
                    "invalid_params",
                    format!("file exceeds transfer limit ({})", self.max_transfer_bytes),
                ));
            }
            if metadata.offset != 0 && metadata.offset != requested_offset {
                return Err(ClientError::local(
                    "protocol",
                    "remote returned an unexpected download offset",
                ));
            }
            if metadata.offset > 0
                && !metadata
                    .prefix_sha256
                    .as_deref()
                    .is_some_and(|remote| remote.eq_ignore_ascii_case(&prefix_sha256))
            {
                return Err(ClientError::local(
                    "protocol",
                    "remote download prefix SHA-256 acknowledgement mismatch",
                ));
            }
            if metadata.offset == 0 {
                temp.set_len(0).map_err(|error| {
                    ClientError::local("io", format!("truncate {}: {error}", partial.display()))
                })?;
            }
            temp.seek(SeekFrom::Start(metadata.offset))
                .map_err(|error| {
                    ClientError::local("io", format!("seek {}: {error}", partial.display()))
                })?;
            let mut digest = if metadata.offset == requested_offset {
                prefix_digest
            } else {
                Sha256::new()
            };
            let mut total = metadata.offset;
            loop {
                let frame = session
                    .receive_with_idle_timeout(Some(DEFAULT_TRANSFER_IDLE_TIMEOUT))
                    .map_err(protocol_error)?;
                check_request_id(&frame, request_id)?;
                match frame.kind {
                    FrameType::Chunk => {
                        total = total
                            .checked_add(frame.payload.len() as u64)
                            .ok_or_else(|| {
                                ClientError::local("protocol", "transfer size overflow")
                            })?;
                        if total > metadata.size || total > self.max_transfer_bytes {
                            return Err(ClientError::local(
                                "protocol",
                                "download exceeded declared size",
                            ));
                        }
                        temp.write_all(&frame.payload).map_err(|error| {
                            ClientError::local("io", format!("write {local_path}: {error}"))
                        })?;
                        digest.update(&frame.payload);
                    }
                    FrameType::End => {
                        let end: TransferEnd = serde_json::from_slice(&frame.payload)
                            .map_err(|error| ClientError::local("protocol", error.to_string()))?;
                        let sha256 = format!("{:x}", digest.finalize());
                        if total != metadata.size || end.size != total || end.sha256 != sha256 {
                            return Err(ClientError::local(
                                "protocol",
                                "download length or SHA-256 mismatch",
                            ));
                        }
                        temp.flush()
                            .and_then(|_| temp.sync_all())
                            .map_err(|error| {
                                ClientError::local("io", format!("flush {local_path}: {error}"))
                            })?;
                        preserve_local_mode(target, &partial)
                            .map_err(ClientError::after_response)?;
                        drop(temp);
                        persist_local(&partial, target, overwrite)
                            .map_err(ClientError::after_response)?;
                        cleanup.committed = true;
                        return Ok(json!({
                            "bytes_transferred": total - metadata.offset,
                            "size": total,
                            "resumed_from": metadata.offset,
                            "sha256": sha256,
                            "source": remote_path,
                            "destination": local_path
                        }));
                    }
                    FrameType::Error => {
                        return Err(serde_json::from_slice::<RemoteError>(&frame.payload)
                            .map_err(|error| ClientError::local("protocol", error.to_string()))?
                            .into());
                    }
                    _ => {
                        return Err(ClientError::local(
                            "protocol",
                            "unexpected frame during download",
                        ));
                    }
                }
            }
        })();
        self.finish_operation(result.map_err(ClientError::during_request), false)
    }

    fn probe(&mut self, timeout: Duration) -> Value {
        let started = Instant::now();
        let deadline = started + timeout;
        let mut connection_reused = self.session.is_some();
        // A reused session only spends half the budget on its health check. A
        // stale or half-open connection must never be able to consume the whole
        // probe timeout, because that would leave nothing for the reconnect that
        // recovers it and report the remote unreachable while it is healthy.
        let first = if self.session.is_some() {
            self.ping_current_session_until(started + timeout / 2)
        } else {
            self.connect_and_ping_until(deadline)
        };
        let result = match first {
            Ok(agent_info) => Ok(agent_info),
            Err(first_error) if connection_reused => {
                connection_reused = false;
                self.session = None;
                self.last_activity = None;
                if Instant::now() >= deadline {
                    Err(first_error)
                } else {
                    self.connect_and_ping_until(deadline)
                }
            }
            Err(error) => Err(error),
        };
        let probed_at_ms = unix_time_ms();
        let latency_ms = duration_ms(started.elapsed());
        let probe = match result {
            Ok(agent_info) => {
                self.last_success_at_ms = Some(probed_at_ms);
                self.last_error = None;
                self.last_agent_info = Some(agent_info.clone());
                json!({
                    "address": self.remote.to_string(),
                    "reachable": true,
                    "connected": true,
                    "connection_reused": connection_reused,
                    "latency_ms": latency_ms,
                    "probed_at_ms": probed_at_ms,
                    "lifecycle_state": self.lifecycle_state.name(),
                    "agent_info": agent_info,
                    "error": null
                })
            }
            Err(error) => {
                self.session = None;
                self.last_activity = None;
                let error_value = error.details();
                self.last_error = Some(error_value.clone());
                json!({
                    "address": self.remote.to_string(),
                    "reachable": false,
                    "connected": false,
                    "connection_reused": connection_reused,
                    "latency_ms": latency_ms,
                    "probed_at_ms": probed_at_ms,
                    "lifecycle_state": self.lifecycle_state.name(),
                    "agent_info": null,
                    "error": error_value
                })
            }
        };
        self.last_probe = Some(probe.clone());
        probe
    }

    fn wait_remote_result(
        &self,
        wait_for: &str,
        reached: bool,
        timed_out: bool,
        observed_offline: bool,
        attempts: u64,
        started: Instant,
    ) -> Value {
        json!({
            "address": self.remote.to_string(),
            "wait_for": wait_for,
            "reached": reached,
            "timed_out": timed_out,
            "observed_offline": observed_offline,
            "attempts": attempts,
            "elapsed_ms": duration_ms(started.elapsed()),
            "connected": self.session.is_some(),
            "connection_state": if self.session.is_some() { "cached" } else { "disconnected" },
            "lifecycle_state": self.lifecycle_state.name(),
            "last_probe": self.last_probe,
            "agent_info": self.last_agent_info
        })
    }

    fn prepare_connection(&mut self) -> Result<(), ClientError> {
        self.ensure_connected()?;
        let needs_health_check = self
            .last_activity
            .is_some_and(|last_activity| last_activity.elapsed() >= self.health_check_after);
        if needs_health_check
            && self
                .ping_current_session(DEFAULT_HEALTH_CHECK_TIMEOUT)
                .is_err()
        {
            self.session = None;
            self.last_activity = None;
            self.ensure_connected()?;
        }
        Ok(())
    }

    fn ensure_connected(&mut self) -> Result<(), ClientError> {
        self.ensure_connected_with(DEFAULT_CONNECT_TIMEOUT)
    }

    fn ensure_connected_with(&mut self, connect_timeout: Duration) -> Result<(), ClientError> {
        self.ensure_connected_until(Instant::now() + connect_timeout)
    }

    fn connect_and_ping_until(&mut self, deadline: Instant) -> Result<Value, ClientError> {
        self.ensure_connected_until(deadline)?;
        self.ping_current_session_until(deadline)
    }

    fn ensure_connected_until(&mut self, deadline: Instant) -> Result<(), ClientError> {
        if self.session.is_none() {
            let mut options = SessionOptions::proxy(self.timeout);
            options.connect_timeout = DEFAULT_CONNECT_TIMEOUT;
            let connection = Session::connect_until(
                SocketAddr::V4(self.remote),
                BUILTIN_PSK,
                options,
                DEFAULT_MAX_CONTROL_BYTES,
                deadline,
            )
            .map_err(connection_error);
            match connection {
                Ok(session) => self.session = Some(session),
                Err(error) => {
                    self.last_error = Some(error.details());
                    self.last_activity = None;
                    return Err(error);
                }
            }
            self.last_activity = Some(Instant::now());
        }
        Ok(())
    }

    fn ping_current_session(&mut self, timeout: Duration) -> Result<Value, ClientError> {
        self.ping_current_session_until(Instant::now() + timeout)
    }

    fn ping_current_session_until(&mut self, deadline: Instant) -> Result<Value, ClientError> {
        let request_id = self.allocate_request_id()?;
        let session = self.session.as_mut().expect("connected");
        session
            .send_json_until(
                FrameType::Request,
                request_id,
                &RemoteRequest {
                    operation: INTERNAL_PING_OPERATION.to_string(),
                    arguments: Value::Null,
                },
                deadline,
            )
            .map_err(health_check_error)?;
        let frame = session
            .receive_until(deadline)
            .map_err(health_check_error)?;
        check_request_id(&frame, request_id)?;
        let agent_info = decode_remote_response(frame)?;
        if agent_info["name"] != "remote-ops-agent"
            || agent_info["runtime"]["instance_id"].as_str().is_none()
            || agent_info["supported_operations"].as_array().is_none()
        {
            return Err(ClientError::local(
                "incompatible_agent",
                "health check did not return Agent identity and capabilities; upgrade the remote Agent",
            ));
        }
        self.last_activity = Some(Instant::now());
        self.last_agent_info = Some(agent_info.clone());
        Ok(agent_info)
    }

    fn finish_operation<T>(
        &mut self,
        result: Result<T, ClientError>,
        reuse_remote_error: bool,
    ) -> Result<T, ClientError> {
        match &result {
            Ok(_) => {
                self.last_activity = Some(Instant::now());
                self.last_success_at_ms = Some(unix_time_ms());
                self.last_error = None;
            }
            Err(error) => {
                if reuse_remote_error && error.reusable_reply() {
                    self.last_activity = Some(Instant::now());
                } else {
                    self.session = None;
                    self.last_activity = None;
                }
                self.last_error = Some(error.details());
            }
        }
        result
    }

    fn allocate_request_id(&mut self) -> Result<u64, ClientError> {
        let id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| ClientError::local("protocol", "request id exhausted"))?;
        Ok(id)
    }
}

fn agent_instance_id(agent_info: &Value) -> Option<&str> {
    agent_info["runtime"]["instance_id"].as_str()
}

pub(crate) fn agent_supports_unix_mode(agent_info: &Value) -> bool {
    agent_info["platform"]["family"] == "unix"
}

fn candidate_matches_agent(candidate: &Value, agent_info: &Value) -> bool {
    candidate["name"] == agent_info["name"]
        && candidate["version"] == agent_info["version"]
        && candidate["protocol_version"] == agent_info["protocol_version"]
        && candidate["build"]["target"] == agent_info["build"]["target"]
        && candidate["build"]["git_revision"] == agent_info["build"]["git_revision"]
}

fn update_receipt_matches(
    receipt: &Value,
    sha256: &str,
    previous: Option<&str>,
    attempt: Option<&str>,
) -> bool {
    receipt["candidate_sha256"]
        .as_str()
        .is_some_and(|hash| hash.eq_ignore_ascii_case(sha256))
        && previous.is_some_and(|previous| receipt["previous_instance_id"] == previous)
        && attempt.is_none_or(|attempt| receipt["attempt_id"] == attempt)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn receive_remote_response(session: &mut Session, request_id: u64) -> Result<Value, ClientError> {
    let frame = session.receive().map_err(protocol_error)?;
    check_request_id(&frame, request_id)?;
    decode_remote_response(frame)
}

fn decode_remote_response(frame: remote_ops_protocol::Frame) -> Result<Value, ClientError> {
    match frame.kind {
        FrameType::Response => {
            let response: RemoteResponse = serde_json::from_slice(&frame.payload)
                .map_err(|error| ClientError::protocol(error.to_string()))?;
            match (response.ok, response.result, response.error) {
                (true, Some(value), None) if value.is_object() => Ok(value),
                (false, None, Some(error)) if !error.kind.is_empty() => Err(error.into()),
                _ => Err(ClientError::protocol(
                    "inconsistent remote response envelope",
                )),
            }
        }
        FrameType::Error => Err(serde_json::from_slice::<RemoteError>(&frame.payload)
            .map_err(|error| ClientError::protocol(error.to_string()))?
            .into()),
        _ => Err(ClientError::protocol("unexpected response frame")),
    }
}

fn receive_transfer_metadata(
    session: &mut Session,
    request_id: u64,
    expected_size: u64,
) -> Result<TransferMetadata, ClientError> {
    let frame = session.receive().map_err(protocol_error)?;
    check_request_id(&frame, request_id)?;
    match frame.kind {
        FrameType::Response => {
            let metadata: TransferMetadata = serde_json::from_slice(&frame.payload)
                .map_err(|error| ClientError::local("protocol", error.to_string()))?;
            if metadata.size != expected_size {
                return Err(ClientError::local(
                    "protocol",
                    "remote upload size acknowledgement mismatch",
                ));
            }
            Ok(metadata)
        }
        FrameType::Error => Err(serde_json::from_slice::<RemoteError>(&frame.payload)
            .map_err(|error| ClientError::local("protocol", error.to_string()))?
            .into()),
        _ => Err(ClientError::local(
            "protocol",
            "expected upload acknowledgement",
        )),
    }
}

fn check_request_id(frame: &remote_ops_protocol::Frame, expected: u64) -> Result<(), ClientError> {
    if frame.request_id == expected {
        Ok(())
    } else {
        Err(ClientError::protocol("request id mismatch"))
    }
}

pub(crate) fn validate_request(operation: &str, arguments: &Value) -> Result<(), ClientError> {
    encode_request(operation, arguments).map(|_| ())
}

fn encode_request(operation: &str, arguments: &Value) -> Result<Vec<u8>, ClientError> {
    #[derive(serde::Serialize)]
    struct Request<'a> {
        operation: &'a str,
        arguments: &'a Value,
    }
    remote_ops_protocol::encoding::to_vec_bounded(
        &Request {
            operation,
            arguments,
        },
        DEFAULT_MAX_CONTROL_BYTES,
    )
    .map_err(|error| ClientError::local("invalid_params", error.to_string()))
}

fn protocol_error(error: remote_ops_protocol::ProtocolError) -> ClientError {
    if let remote_ops_protocol::ProtocolError::Io(io)
    | remote_ops_protocol::ProtocolError::IncompleteFrame(io) = &error
    {
        let disconnected = matches!(
            io.kind(),
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::NotConnected
        );
        ClientError::transport(
            "connection_uncertain",
            format!("remote connection failed; request was not replayed: {error}"),
            true,
        )
        .with_disconnect(disconnected)
    } else {
        ClientError::protocol(format!(
            "remote protocol failed; request was not replayed: {error}"
        ))
    }
}

fn connection_error(error: remote_ops_protocol::ProtocolError) -> ClientError {
    ClientError::transport(
        "connection_unavailable",
        format!("could not establish remote connection; request was not sent: {error}"),
        false,
    )
}

fn health_check_error(error: remote_ops_protocol::ProtocolError) -> ClientError {
    ClientError::transport(
        "connection_unavailable",
        format!("health check failed: {error}"),
        false,
    )
}

fn persist_local(source: &Path, target: &Path, overwrite: bool) -> Result<(), ClientError> {
    if target.exists() && !overwrite {
        return Err(ClientError::local(
            "invalid_params",
            format!("destination already exists: {}", target.display()),
        ));
    }
    platform_persist_local(source, target)?;
    sync_local_parent(target)
}

fn preserve_local_mode(target: &Path, temporary: &Path) -> Result<(), ClientError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if let Ok(metadata) = target.metadata() {
            std::fs::set_permissions(temporary, std::fs::Permissions::from_mode(metadata.mode()))
                .map_err(|error| {
                ClientError::local("io", format!("preserve {} mode: {error}", target.display()))
            })?;
        }
    }
    #[cfg(not(unix))]
    let _ = (target, temporary);
    Ok(())
}

#[cfg(not(windows))]
fn platform_persist_local(source: &Path, target: &Path) -> Result<(), ClientError> {
    fs::rename(source, target)
        .map_err(|error| ClientError::local("io", format!("persist {}: {error}", target.display())))
}

#[cfg(unix)]
fn sync_local_parent(target: &Path) -> Result<(), ClientError> {
    let parent = target.parent().unwrap_or(Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            ClientError::local(
                "io",
                format!("sync directory {}: {error}", parent.display()),
            )
        })
}

#[cfg(not(unix))]
fn sync_local_parent(_target: &Path) -> Result<(), ClientError> {
    Ok(())
}

#[cfg(windows)]
fn platform_persist_local(source: &Path, target: &Path) -> Result<(), ClientError> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(ClientError::local(
            "io",
            format!(
                "persist {}: {}",
                target.display(),
                std::io::Error::last_os_error()
            ),
        ))
    } else {
        Ok(())
    }
}

fn download_partial_path(target: &Path) -> Result<PathBuf, ClientError> {
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ClientError::local("invalid_params", "local_path must name a UTF-8 file"))?;
    Ok(parent.join(format!(".remoteops-download-{name}.part")))
}

fn open_download_partial(path: &Path, resume: bool) -> Result<File, ClientError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_file() {
            return Err(ClientError::local(
                "invalid_params",
                "download partial path is not a regular file",
            ));
        }
        if !resume {
            fs::remove_file(path).map_err(|error| {
                ClientError::local(
                    "io",
                    format!("remove stale partial {}: {error}", path.display()),
                )
            })?;
        }
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| ClientError::local("io", format!("open {}: {error}", path.display())))
}

struct DownloadPartialCleanup {
    path: PathBuf,
    keep: bool,
    committed: bool,
}
impl Drop for DownloadPartialCleanup {
    fn drop(&mut self) {
        if !self.keep && !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn hash_file(file: &mut File, display: &str, expected_size: u64) -> Result<String, ClientError> {
    let (digest, _) = hash_local_prefix(file, expected_size, display)?;
    Ok(format!("{:x}", digest.finalize()))
}

fn hash_local_prefix(
    file: &mut File,
    length: u64,
    display: &str,
) -> Result<(Sha256, String), ClientError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| ClientError::local("io", format!("seek {display}: {error}")))?;
    let mut digest = Sha256::new();
    let mut remaining = length;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let limit = buffer.len().min(remaining as usize);
        let read = file
            .read(&mut buffer[..limit])
            .map_err(|error| ClientError::local("io", format!("read {display}: {error}")))?;
        if read == 0 {
            return Err(ClientError::local(
                "io",
                format!("{display} became shorter while hashing"),
            ));
        }
        digest.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let sha256 = format!("{:x}", digest.clone().finalize());
    Ok((digest, sha256))
}

#[cfg(test)]
mod tests {
    use super::*;
    use remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES;
    use std::net::TcpListener;
    use std::thread;

    fn accept_session(listener: &TcpListener) -> Session {
        let (stream, _) = listener.accept().unwrap();
        Session::accept(
            stream,
            BUILTIN_PSK,
            SessionOptions::agent(),
            DEFAULT_MAX_CONTROL_BYTES,
        )
        .unwrap()
    }

    #[test]
    fn update_receipts_must_match_the_hash_old_instance_and_attempt() {
        let receipt = json!({"attempt_id":"attempt","candidate_sha256":"a".repeat(64),"previous_instance_id":"old","status":"updated"});
        assert!(update_receipt_matches(
            &receipt,
            &"a".repeat(64),
            Some("old"),
            Some("attempt")
        ));
        assert!(!update_receipt_matches(
            &receipt,
            &"b".repeat(64),
            Some("old"),
            Some("attempt")
        ));
        assert!(!update_receipt_matches(
            &receipt,
            &"a".repeat(64),
            Some("other"),
            Some("attempt")
        ));
        assert!(!update_receipt_matches(
            &receipt,
            &"a".repeat(64),
            Some("old"),
            Some("stale")
        ));
        assert!(!update_receipt_matches(
            &Value::Null,
            &"a".repeat(64),
            Some("old"),
            None
        ));
    }

    #[test]
    fn probe_budget_is_shared_by_delayed_handshake_and_health_check() {
        use std::io::{Read, Write};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut hello = [0; 37];
            stream.read_exact(&mut hello).unwrap();
            thread::sleep(Duration::from_millis(250));
            // Use a second protocol Session as the handshake endpoint through a
            // transparent TCP relay, so this test does not duplicate auth code.
            let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
            let upstream_address = upstream.local_addr().unwrap();
            let responder = thread::spawn(move || {
                let mut session = accept_session(&upstream);
                session.receive().unwrap();
                thread::sleep(Duration::from_millis(700));
            });
            let mut connection = std::net::TcpStream::connect(upstream_address).unwrap();
            connection.write_all(&hello).unwrap();
            let mut reply = [0; 64];
            connection.read_exact(&mut reply).unwrap();
            stream.write_all(&reply).unwrap();
            let mut tag = [0; 32];
            stream.read_exact(&mut tag).unwrap();
            connection.write_all(&tag).unwrap();
            let mut outgoing = connection.try_clone().unwrap();
            let mut incoming = stream.try_clone().unwrap();
            let forward = thread::spawn(move || {
                let _ = std::io::copy(&mut incoming, &mut outgoing);
            });
            let _ = std::io::copy(&mut connection, &mut stream);
            let _ = stream.shutdown(std::net::Shutdown::Both);
            forward.join().unwrap();
            responder.join().unwrap();
        });
        let mut client =
            RemoteClient::new(remote, Duration::from_secs(10), DEFAULT_MAX_TRANSFER_BYTES);
        let started = Instant::now();
        let probe = client.remote_probe(600);
        assert_eq!(probe["reachable"], false);
        assert!(
            started.elapsed() >= Duration::from_millis(500)
                && started.elapsed() < Duration::from_millis(800),
            "{probe}"
        );
        assert_eq!(client.remote_status()["connection_state"], "disconnected");
        server.join().unwrap();
    }

    #[test]
    fn probe_reconnects_after_a_partial_cached_health_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut raw = stream.try_clone().unwrap();
            let mut first = Session::accept(
                stream,
                BUILTIN_PSK,
                SessionOptions::agent(),
                DEFAULT_MAX_CONTROL_BYTES,
            )
            .unwrap();
            answer_ping(&mut first, "old-instance");
            first.receive().unwrap();
            raw.write_all(b"R").unwrap();
            let mut replacement = accept_session(&listener);
            answer_ping(&mut replacement, "new-instance");
        });
        let mut client =
            RemoteClient::new(remote, Duration::from_secs(5), DEFAULT_MAX_TRANSFER_BYTES);
        assert_eq!(client.remote_probe(1000)["reachable"], true);
        let started = Instant::now();
        let result = client.remote_probe(400);
        assert_eq!(result["reachable"], true, "{result}");
        assert_eq!(result["connection_reused"], false);
        assert_eq!(
            result["agent_info"]["runtime"]["instance_id"],
            "new-instance"
        );
        assert!(started.elapsed() < Duration::from_millis(700), "{result}");
        server.join().unwrap();
    }

    #[test]
    fn oversized_user_request_is_rejected_before_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut client = RemoteClient::new(
            listener.local_addr().unwrap().to_string().parse().unwrap(),
            Duration::from_secs(1),
            DEFAULT_MAX_TRANSFER_BYTES,
        );
        let error = client
            .call(
                "write_text",
                json!({"path":"must-not-write","content":"\0".repeat(400_000)}),
            )
            .unwrap_err();
        assert_eq!(error.details()["outcome"], "not_sent");
        assert_eq!(error.details()["source"], "proxy");
        assert_eq!(client.operation_attempts(), 0);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn invalid_response_envelopes_are_uncertain_and_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let mut session = accept_session(&listener);
            let frame = session.receive().unwrap();
            session.send_json(FrameType::Response,frame.request_id,&json!({"ok":true,"result":{"value":1},"error":{"kind":"io","message":"contradictory"}})).unwrap();
        });
        let mut client =
            RemoteClient::new(remote, Duration::from_secs(1), DEFAULT_MAX_TRANSFER_BYTES);
        let error = client.call("exec", json!({})).unwrap_err();
        assert_eq!(error.details()["outcome"], "unknown");
        assert_eq!(error.details()["source"], "protocol");
        assert_eq!(client.remote_status()["connected"], false);
        server.join().unwrap();
    }

    #[test]
    fn reboot_timeout_is_not_acknowledged_as_a_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let mut session = accept_session(&listener);
            session.receive().unwrap();
            thread::sleep(Duration::from_millis(150));
        });
        let mut client = RemoteClient::new(
            remote,
            Duration::from_millis(30),
            DEFAULT_MAX_TRANSFER_BYTES,
        );
        let error = client.reboot(1000).unwrap_err();
        assert_eq!(error.kind, "connection_uncertain");
        assert_eq!(client.remote_status()["lifecycle_state"], "ready");
        server.join().unwrap();
    }

    #[test]
    fn remote_error_text_cannot_impersonate_a_transport_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let mut session = accept_session(&listener);
            let frame = session.receive().unwrap();
            session
                .send_json(
                    FrameType::Response,
                    frame.request_id,
                    &RemoteResponse::failure(RemoteError {
                        kind: "connection_uncertain".into(),
                        message: "remote internal failure".into(),
                    }),
                )
                .unwrap();
        });
        let mut client =
            RemoteClient::new(remote, Duration::from_secs(1), DEFAULT_MAX_TRANSFER_BYTES);
        let error = client.reboot(1000).unwrap_err();
        assert_eq!(error.details()["source"], "remote");
        assert_eq!(error.details()["outcome"], "remote_error");
        assert_eq!(client.remote_status()["lifecycle_state"], "ready");
        server.join().unwrap();
    }

    #[test]
    fn unix_mode_support_follows_remote_platform_family() {
        use serde_json::json;
        assert!(agent_supports_unix_mode(
            &json!({"platform": {"family": "unix"}})
        ));
        assert!(!agent_supports_unix_mode(
            &json!({"platform": {"family": "windows"}})
        ));
        assert!(!agent_supports_unix_mode(&json!({})));
    }

    #[test]
    fn stale_session_is_checked_and_reconnected_before_user_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let mut first = accept_session(&listener);
            let request: RemoteRequest = first.receive_json(FrameType::Request, 1).unwrap();
            assert_eq!(request.operation, "marker");
            first
                .send_json(
                    FrameType::Response,
                    1,
                    &RemoteResponse::success(json!({"connection": 1})),
                )
                .unwrap();
            let ping: RemoteRequest = first.receive_json(FrameType::Request, 2).unwrap();
            assert_eq!(ping.operation, INTERNAL_PING_OPERATION);
            drop(first);

            let mut second = accept_session(&listener);
            let request: RemoteRequest = second.receive_json(FrameType::Request, 3).unwrap();
            assert_eq!(request.operation, "marker");
            second
                .send_json(
                    FrameType::Response,
                    3,
                    &RemoteResponse::success(json!({"connection": 2})),
                )
                .unwrap();
        });

        let mut client =
            RemoteClient::new(remote, Duration::from_secs(2), DEFAULT_MAX_TRANSFER_BYTES);
        assert_eq!(client.call("marker", json!({})).unwrap()["connection"], 1);
        client.health_check_after = Duration::ZERO;
        assert_eq!(client.call("marker", json!({})).unwrap()["connection"], 2);
        server.join().unwrap();
    }

    #[test]
    fn user_request_is_not_replayed_after_connection_loss() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let mut session = accept_session(&listener);
            let request: RemoteRequest = session.receive_json(FrameType::Request, 1).unwrap();
            assert_eq!(request.operation, "mutating-operation");
            drop(session);

            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_millis(200);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok(_) => panic!("user request was replayed on a new connection"),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
        });

        let mut client =
            RemoteClient::new(remote, Duration::from_secs(2), DEFAULT_MAX_TRANSFER_BYTES);
        let error = client.call("mutating-operation", json!({})).unwrap_err();
        assert_eq!(error.kind, "connection_uncertain");
        assert!(!client.remote_status()["connected"].as_bool().unwrap());
        server.join().unwrap();
    }

    fn agent_identity(instance_id: &str) -> Value {
        json!({
            "name": "remote-ops-agent",
            "runtime": {"instance_id": instance_id},
            "supported_operations": []
        })
    }

    fn answer_ping(session: &mut Session, instance_id: &str) {
        let frame = session.receive().unwrap();
        assert_eq!(frame.kind, FrameType::Request);
        let ping: RemoteRequest = serde_json::from_slice(&frame.payload).unwrap();
        assert_eq!(ping.operation, INTERNAL_PING_OPERATION);
        session
            .send_json(
                FrameType::Response,
                frame.request_id,
                &RemoteResponse::success(agent_identity(instance_id)),
            )
            .unwrap();
    }

    #[test]
    fn offline_then_online_detects_a_replaced_agent_while_ready() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let mut first = accept_session(&listener);
            answer_ping(&mut first, "instance-1");
            drop(first);

            let mut second = accept_session(&listener);
            answer_ping(&mut second, "instance-2");
            // Stay alive so the probe reporting the new instance never observes
            // the remote going offline.
            thread::sleep(Duration::from_millis(500));
        });

        let mut client =
            RemoteClient::new(remote, Duration::from_secs(2), DEFAULT_MAX_TRANSFER_BYTES);
        let probe = client.remote_probe(1_000);
        assert_eq!(probe["agent_info"]["runtime"]["instance_id"], "instance-1");

        // The cached session is gone; the next probe reaches a different instance.
        client.session = None;
        client.last_activity = None;
        let waited = client.wait_remote(Some("offline_then_online"), 2_000, 100, 1_000);
        assert!(waited["reached"].as_bool().unwrap(), "{waited}");
        assert!(!waited["observed_offline"].as_bool().unwrap(), "{waited}");
        server.join().unwrap();
    }

    #[test]
    fn probe_reconnects_when_a_reused_session_goes_half_open() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let mut first = accept_session(&listener);
            answer_ping(&mut first, "instance-1");

            // Never answer again on the first session: the peer is half open, so
            // only a fresh connection can make progress.
            let mut second = accept_session(&listener);
            answer_ping(&mut second, "instance-1");
            drop(first);
        });

        let mut client =
            RemoteClient::new(remote, Duration::from_secs(2), DEFAULT_MAX_TRANSFER_BYTES);
        assert!(client.remote_probe(1_000)["reachable"].as_bool().unwrap());

        let probe = client.remote_probe(1_000);
        assert!(probe["reachable"].as_bool().unwrap(), "{probe}");
        assert!(!probe["connection_reused"].as_bool().unwrap(), "{probe}");
        server.join().unwrap();
    }

    #[test]
    fn set_remote_resets_the_cached_session_for_the_same_address() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = match listener.local_addr().unwrap() {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => unreachable!("bound an IPv4 listener"),
        };
        let server = thread::spawn(move || {
            let mut session = accept_session(&listener);
            answer_ping(&mut session, "instance-1");
            thread::sleep(Duration::from_millis(500));
        });

        let mut client =
            RemoteClient::new(address, Duration::from_secs(2), DEFAULT_MAX_TRANSFER_BYTES);
        assert!(client.remote_probe(1_000)["reachable"].as_bool().unwrap());
        assert_eq!(client.remote_status()["connection_state"], "cached");

        let status = client
            .set_remote(Some(*address.ip()), Some(address.port()))
            .unwrap();
        assert_eq!(status["connection_state"], "disconnected");
        assert!(!status["connected"].as_bool().unwrap());
        // Observation state survives a same-address re-point so in-flight
        // orchestration and instance-change detection keep working.
        assert_eq!(status["agent_info"]["runtime"]["instance_id"], "instance-1");
        assert!(status["last_success_at_ms"].as_u64().is_some());
        server.join().unwrap();
    }
}
