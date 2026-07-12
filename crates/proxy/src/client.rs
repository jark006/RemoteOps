use std::fs::File;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;
use std::time::Duration;

use remote_ops_protocol::{
    BUILTIN_PSK, DEFAULT_CHUNK_BYTES, DEFAULT_MAX_CONTROL_BYTES, DownloadRequest, FrameType,
    RemoteError, RemoteRequest, RemoteResponse, Session, TransferEnd, TransferMetadata,
    UploadRequest,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

#[derive(Debug)]
pub struct ClientError {
    pub kind: String,
    pub message: String,
}

impl ClientError {
    fn local(kind: &str, message: impl Into<String>) -> Self {
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

pub struct RemoteClient {
    remote: SocketAddrV4,
    timeout: Duration,
    max_transfer_bytes: u64,
    session: Option<Session>,
    next_request_id: u64,
}

impl RemoteClient {
    pub fn new(remote: SocketAddrV4, timeout: Duration, max_transfer_bytes: u64) -> Self {
        Self {
            remote,
            timeout,
            max_transfer_bytes,
            session: None,
            next_request_id: 1,
        }
    }

    pub fn remote_status(&self) -> Value {
        json!({
            "ip": self.remote.ip().to_string(),
            "port": self.remote.port(),
            "address": self.remote.to_string(),
            "connected": self.session.is_some(),
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
        }
        Ok(self.remote_status())
    }

    pub fn call(&mut self, operation: &str, arguments: Value) -> Result<Value, ClientError> {
        let request_id = self.allocate_request_id()?;
        self.ensure_connected()?;
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
        if result.is_err() {
            self.session = None;
        }
        result
    }

    pub fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        overwrite: bool,
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
        let request_id = self.allocate_request_id()?;
        self.ensure_connected()?;
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
                            overwrite,
                        })
                        .expect("serializable"),
                    },
                )
                .map_err(protocol_error)?;
            receive_transfer_metadata(session, request_id, size)?;
            let mut digest = Sha256::new();
            let mut total = 0u64;
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
            let sha256 = format!("{:x}", digest.finalize());
            session
                .send_json(
                    FrameType::End,
                    request_id,
                    &TransferEnd {
                        size: total,
                        sha256: sha256.clone(),
                    },
                )
                .map_err(protocol_error)?;
            let remote = receive_remote_response(session, request_id)?;
            Ok(json!({
                "bytes_transferred": remote["bytes_transferred"],
                "sha256": remote["sha256"],
                "source": local_path,
                "destination": remote_path
            }))
        })();
        if result.is_err() {
            self.session = None;
        }
        result
    }

    pub fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        overwrite: bool,
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
        let parent = target
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temp = NamedTempFile::new_in(parent).map_err(|error| {
            ClientError::local(
                "io",
                format!("create temporary file for {local_path}: {error}"),
            )
        })?;
        let request_id = self.allocate_request_id()?;
        self.ensure_connected()?;
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
            let mut digest = Sha256::new();
            let mut total = 0u64;
            loop {
                let frame = session.receive().map_err(protocol_error)?;
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
                            .and_then(|_| temp.as_file().sync_all())
                            .map_err(|error| {
                                ClientError::local("io", format!("flush {local_path}: {error}"))
                            })?;
                        preserve_local_mode(target, temp.path())?;
                        persist_local(temp, target, overwrite)?;
                        return Ok(
                            json!({"bytes_transferred": total, "sha256": sha256, "source": remote_path, "destination": local_path}),
                        );
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
        if result.is_err() {
            self.session = None;
        }
        result
    }

    fn ensure_connected(&mut self) -> Result<(), ClientError> {
        if self.session.is_none() {
            self.session = Some(
                Session::connect(
                    &self.remote.to_string(),
                    BUILTIN_PSK,
                    self.timeout,
                    DEFAULT_MAX_CONTROL_BYTES,
                )
                .map_err(protocol_error)?,
            );
        }
        Ok(())
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
) -> Result<(), ClientError> {
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
            Ok(())
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

fn persist_local(temp: NamedTempFile, target: &Path, overwrite: bool) -> Result<(), ClientError> {
    if target.exists() && !overwrite {
        return Err(ClientError::local(
            "invalid_params",
            format!("destination already exists: {}", target.display()),
        ));
    }
    platform_persist_local(temp, target)?;
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
fn platform_persist_local(temp: NamedTempFile, target: &Path) -> Result<(), ClientError> {
    temp.persist(target).map(|_| ()).map_err(|error| {
        ClientError::local(
            "io",
            format!("persist {}: {}", target.display(), error.error),
        )
    })
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
fn platform_persist_local(temp: NamedTempFile, target: &Path) -> Result<(), ClientError> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    let temporary = temp.into_temp_path();
    let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
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
