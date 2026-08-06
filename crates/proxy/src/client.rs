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

#[derive(Debug)]
pub struct ClientError {
    pub kind: String,
    pub message: String,
}

impl ClientError {
    pub(crate) fn local(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

impl From<RemoteError> for ClientError {
    fn from(value: RemoteError) -> Self {
        Self {
            kind: value.kind,
            message: value.message,
        }
    }
}

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
        if remote != self.remote {
            self.remote = remote;
            self.session = None;
            self.last_activity = None;
            self.last_success_at_ms = None;
            self.last_error = None;
            self.last_probe = None;
            self.last_agent_info = None;
            self.lifecycle_state = LifecycleState::Ready;
        }
        Ok(self.remote_status())
    }

    pub fn call(&mut self, operation: &str, arguments: Value) -> Result<Value, ClientError> {
        self.prepare_connection()?;
        let request_id = self.allocate_request_id()?;
        let request = RemoteRequest {
            operation: operation.to_string(),
            arguments,
        };
        let result = (|| {
            let session = self.session.as_mut().expect("connected");
            session
                .send_json(FrameType::Request, request_id, &request)
                .map_err(protocol_error)?;
            receive_remote_response(session, request_id)
        })();
        let result = self.finish_operation(result);
        if operation == "agent_info"
            && let Ok(agent_info) = &result
        {
            self.last_agent_info = Some(agent_info.clone());
        }
        result
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
            .map(str::to_string);
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
                Err(error) if error.kind == "connection_uncertain" => {
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
        if previous_agent["capabilities"]["self_update"] != true {
            return Err(ClientError::local(
                "unsupported",
                "remote agent does not support self-update",
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
            Err(error) if error.kind == "connection_uncertain" => (false, None),
            Err(error) => return Err(error),
        };
        self.session = None;
        self.last_activity = None;
        self.lifecycle_state = LifecycleState::Updating {
            previous_instance_id: previous_instance_id.clone(),
        };
        let wait = self.wait_remote(
            Some("offline_then_online"),
            timeout_ms,
            poll_interval_ms,
            probe_timeout_ms,
        );
        let current_agent = wait["agent_info"].clone();
        let reached = wait["reached"].as_bool().unwrap_or(false);
        let candidate = prepared
            .as_ref()
            .and_then(|value| value.get("candidate"))
            .cloned();
        let status = if !reached {
            "timed_out"
        } else if candidate
            .as_ref()
            .is_some_and(|candidate| candidate_matches_agent(candidate, &current_agent))
        {
            "updated"
        } else if restart_acknowledged {
            "rolled_back"
        } else {
            "unconfirmed"
        };
        Ok(json!({
            "status": status,
            "updated": status == "updated",
            "rolled_back": status == "rolled_back",
            "timed_out": status == "timed_out",
            "restart_acknowledged": restart_acknowledged,
            "previous_agent": previous_agent,
            "candidate": candidate,
            "current_agent": current_agent,
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
        self.prepare_connection()?;
        let request_id = self.allocate_request_id()?;
        let result = (|| {
            let session = self.session.as_mut().expect("connected");
            session
                .send_json(
                    FrameType::Request,
                    request_id,
                    &RemoteRequest {
                        operation: "upload_file".to_string(),
                        arguments: serde_json::to_value(UploadRequest {
                            remote_path: remote_path.to_string(),
                            size,
                            sha256: sha256.clone(),
                            overwrite,
                            resume,
                            mode,
                        })
                        .expect("serializable"),
                    },
                )
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
        self.finish_operation(result)
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
        let mut temp = open_download_partial(&partial, resume)?;
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
        self.prepare_connection()?;
        let request_id = self.allocate_request_id()?;
        let result = (|| {
            let session = self.session.as_mut().expect("connected");
            session
                .send_json(
                    FrameType::Request,
                    request_id,
                    &RemoteRequest {
                        operation: "download_file".to_string(),
                        arguments: serde_json::to_value(DownloadRequest {
                            remote_path: remote_path.to_string(),
                            offset: requested_offset,
                            prefix_sha256: (requested_offset > 0).then_some(prefix_sha256.clone()),
                        })
                        .expect("serializable"),
                    },
                )
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
                        preserve_local_mode(target, &partial)?;
                        drop(temp);
                        persist_local(&partial, target, overwrite)?;
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
        let result = self.finish_operation(result);
        if result.is_err() && !resume {
            let _ = fs::remove_file(&partial);
        }
        result
    }

    fn probe(&mut self, timeout: Duration) -> Value {
        let started = Instant::now();
        let connection_reused = self.session.is_some();
        let first = if self.session.is_some() {
            self.ping_current_session(timeout)
        } else {
            self.ensure_connected_with(timeout)
                .and_then(|_| self.ping_current_session(timeout))
        };
        let result = match first {
            Ok(agent_info) => Ok(agent_info),
            Err(first_error) if connection_reused => {
                self.session = None;
                self.last_activity = None;
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    Err(first_error)
                } else {
                    self.ensure_connected_with(remaining)
                        .and_then(|_| self.ping_current_session(remaining))
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
                let error_value = json!({"kind": error.kind, "message": error.message});
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
        if self.session.is_none() {
            let mut options = SessionOptions::proxy(self.timeout);
            let phase_timeout = (connect_timeout / 2).max(Duration::from_millis(1));
            options.connect_timeout = phase_timeout;
            options.handshake_timeout = phase_timeout;
            self.session = Some(
                Session::connect(
                    SocketAddr::V4(self.remote),
                    BUILTIN_PSK,
                    options,
                    DEFAULT_MAX_CONTROL_BYTES,
                )
                .map_err(connection_error)?,
            );
            self.last_activity = Some(Instant::now());
        }
        Ok(())
    }

    fn ping_current_session(&mut self, timeout: Duration) -> Result<Value, ClientError> {
        let request_id = self.allocate_request_id()?;
        let session = self.session.as_mut().expect("connected");
        session
            .send_json(
                FrameType::Request,
                request_id,
                &RemoteRequest {
                    operation: INTERNAL_PING_OPERATION.to_string(),
                    arguments: Value::Null,
                },
            )
            .map_err(health_check_error)?;
        let frame = session
            .receive_with_idle_timeout(Some(timeout))
            .map_err(health_check_error)?;
        check_request_id(&frame, request_id)?;
        let agent_info = match frame.kind {
            FrameType::Response => {
                let response = serde_json::from_slice::<RemoteResponse>(&frame.payload)
                    .map_err(|error| ClientError::local("protocol", error.to_string()))?;
                if response.ok {
                    response.result.unwrap_or(Value::Null)
                } else {
                    return Err(response
                        .error
                        .unwrap_or(RemoteError {
                            kind: "remote".to_string(),
                            message: "health check failed".to_string(),
                        })
                        .into());
                }
            }
            FrameType::Error => {
                return Err(serde_json::from_slice::<RemoteError>(&frame.payload)
                    .map_err(|error| ClientError::local("protocol", error.to_string()))?
                    .into());
            }
            _ => {
                return Err(ClientError::local(
                    "protocol",
                    "unexpected health check response frame",
                ));
            }
        };
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

    fn finish_operation<T>(&mut self, result: Result<T, ClientError>) -> Result<T, ClientError> {
        match &result {
            Ok(_) => {
                self.last_activity = Some(Instant::now());
                self.last_success_at_ms = Some(unix_time_ms());
                self.last_error = None;
            }
            Err(error) => {
                self.session = None;
                self.last_activity = None;
                self.last_error = Some(json!({
                    "kind": error.kind,
                    "message": error.message
                }));
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
    match frame.kind {
        FrameType::Response => {
            let response: RemoteResponse = serde_json::from_slice(&frame.payload)
                .map_err(|error| ClientError::local("protocol", error.to_string()))?;
            if response.ok {
                Ok(response.result.unwrap_or(Value::Null))
            } else {
                Err(response
                    .error
                    .unwrap_or(RemoteError {
                        kind: "remote".to_string(),
                        message: "remote operation failed".to_string(),
                    })
                    .into())
            }
        }
        FrameType::Error => Err(serde_json::from_slice::<RemoteError>(&frame.payload)
            .map_err(|error| ClientError::local("protocol", error.to_string()))?
            .into()),
        _ => Err(ClientError::local("protocol", "unexpected response frame")),
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
        Err(ClientError::local("protocol", "request id mismatch"))
    }
}

fn protocol_error(error: remote_ops_protocol::ProtocolError) -> ClientError {
    ClientError::local(
        "connection_uncertain",
        format!("remote connection failed; request was not replayed: {error}"),
    )
}

fn connection_error(error: remote_ops_protocol::ProtocolError) -> ClientError {
    ClientError::local(
        "connection_unavailable",
        format!("could not establish remote connection; request was not sent: {error}"),
    )
}

fn health_check_error(error: remote_ops_protocol::ProtocolError) -> ClientError {
    ClientError::local(
        "connection_unavailable",
        format!("health check failed: {error}"),
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
}
