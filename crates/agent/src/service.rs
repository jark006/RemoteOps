use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Instant;

use remote_ops_protocol::{
    BUILTIN_PSK, DEFAULT_CHUNK_BYTES, DEFAULT_MAX_CONTROL_BYTES, DEFAULT_TRANSFER_IDLE_TIMEOUT,
    DownloadRequest, FrameType, INTERNAL_PING_OPERATION, ProtocolError, RemoteError, RemoteRequest,
    RemoteResponse, Session, SessionOptions, TransferEnd, TransferMetadata, UploadRequest,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::dispatch;
use crate::error::AgentError;
use crate::tools::file_ops::set_optional_mode;
use crate::tools::files::{
    ensure_regular_file, normalized_path, persist_path_replace, preserve_existing_mode,
};
use crate::tools::jobs::JobManager;
use crate::tools::lifecycle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionAction {
    Continue,
    RestartAgent,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentUpdatePrepareArgs {
    expected_sha256: String,
}

enum TransferError {
    Agent(AgentError),
    Protocol(ProtocolError),
}

impl From<AgentError> for TransferError {
    fn from(value: AgentError) -> Self {
        Self::Agent(value)
    }
}

impl From<ProtocolError> for TransferError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

pub fn handle_connection(
    stream: TcpStream,
    max_transfer_bytes: u64,
    jobs: &JobManager,
) -> Result<ConnectionAction, Box<dyn std::error::Error + Send + Sync>> {
    let peer = stream
        .peer_addr()
        .map(|address| address.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let mut session = Session::accept(
        stream,
        BUILTIN_PSK,
        SessionOptions::agent(),
        DEFAULT_MAX_CONTROL_BYTES,
    )?;
    loop {
        let frame = match session.receive() {
            Ok(frame) => frame,
            Err(ProtocolError::Io(error)) if is_quiet_disconnect(&error) => {
                return Ok(ConnectionAction::Continue);
            }
            Err(error) => return Err(error.into()),
        };
        if frame.kind != FrameType::Request {
            return Err(format!("expected request, received {:?}", frame.kind).into());
        }
        let request: RemoteRequest = serde_json::from_slice(&frame.payload)?;
        if request.operation == INTERNAL_PING_OPERATION {
            session.send_json(
                FrameType::Response,
                frame.request_id,
                &RemoteResponse::success(lifecycle::agent_info(max_transfer_bytes)),
            )?;
            continue;
        }
        let started_at = Instant::now();
        log_operation_started(&peer, frame.request_id, &request.operation);
        match request.operation.as_str() {
            "agent_update_prepare" => {
                let result = serde_json::from_value::<AgentUpdatePrepareArgs>(request.arguments)
                    .map_err(|error| AgentError::invalid(error.to_string()))
                    .and_then(|args| {
                        lifecycle::prepare_agent_update(
                            &args.expected_sha256,
                            max_transfer_bytes,
                            lifecycle::restart_args(),
                        )
                    });
                let (response, action, error) = match result {
                    Ok(value) => (
                        RemoteResponse::success(value),
                        ConnectionAction::RestartAgent,
                        None,
                    ),
                    Err(error) => {
                        let message = error.to_string();
                        (
                            RemoteResponse::failure(error.into()),
                            ConnectionAction::Continue,
                            Some(message),
                        )
                    }
                };
                log_operation_finished(
                    &peer,
                    frame.request_id,
                    &request.operation,
                    started_at,
                    error.as_deref(),
                );
                session.send_json(FrameType::Response, frame.request_id, &response)?;
                if action == ConnectionAction::RestartAgent {
                    return Ok(action);
                }
            }
            "upload_file" => {
                let args: UploadRequest = match serde_json::from_value(request.arguments) {
                    Ok(args) => args,
                    Err(error) => {
                        log_operation_finished(
                            &peer,
                            frame.request_id,
                            &request.operation,
                            started_at,
                            Some(&error.to_string()),
                        );
                        session.send_json(
                            FrameType::Error,
                            frame.request_id,
                            &RemoteError {
                                kind: "invalid_params".to_string(),
                                message: error.to_string(),
                            },
                        )?;
                        continue;
                    }
                };
                let result =
                    receive_upload(&mut session, frame.request_id, args, max_transfer_bytes);
                complete_transfer(
                    &mut session,
                    &peer,
                    frame.request_id,
                    &request.operation,
                    started_at,
                    result,
                )?;
            }
            "download_file" => {
                let args: DownloadRequest = match serde_json::from_value(request.arguments) {
                    Ok(args) => args,
                    Err(error) => {
                        log_operation_finished(
                            &peer,
                            frame.request_id,
                            &request.operation,
                            started_at,
                            Some(&error.to_string()),
                        );
                        session.send_json(
                            FrameType::Error,
                            frame.request_id,
                            &RemoteError {
                                kind: "invalid_params".to_string(),
                                message: error.to_string(),
                            },
                        )?;
                        continue;
                    }
                };
                let result =
                    send_download(&mut session, frame.request_id, args, max_transfer_bytes);
                complete_transfer(
                    &mut session,
                    &peer,
                    frame.request_id,
                    &request.operation,
                    started_at,
                    result,
                )?;
            }
            _ => {
                let response = match dispatch(
                    &request.operation,
                    request.arguments,
                    jobs,
                    max_transfer_bytes,
                ) {
                    Ok(value) => {
                        log_operation_finished(
                            &peer,
                            frame.request_id,
                            &request.operation,
                            started_at,
                            None,
                        );
                        RemoteResponse::success(value)
                    }
                    Err(error) => {
                        log_operation_finished(
                            &peer,
                            frame.request_id,
                            &request.operation,
                            started_at,
                            Some(&error.to_string()),
                        );
                        RemoteResponse::failure(error.into())
                    }
                };
                session.send_json(FrameType::Response, frame.request_id, &response)?;
            }
        }
    }
}

fn complete_transfer(
    session: &mut Session,
    peer: &str,
    request_id: u64,
    operation: &str,
    started_at: Instant,
    result: Result<(), TransferError>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match result {
        Ok(()) => {
            log_operation_finished(peer, request_id, operation, started_at, None);
            Ok(())
        }
        Err(TransferError::Agent(error)) => {
            log_operation_finished(
                peer,
                request_id,
                operation,
                started_at,
                Some(&error.to_string()),
            );
            session.send_json(FrameType::Error, request_id, &RemoteError::from(error))?;
            Ok(())
        }
        Err(TransferError::Protocol(error)) => {
            log_operation_finished(
                peer,
                request_id,
                operation,
                started_at,
                Some(&error.to_string()),
            );
            Err(error.into())
        }
    }
}

fn log_operation_started(peer: &str, request_id: u64, operation: &str) {
    eprintln!(
        "[operation] peer={peer} request_id={request_id} operation={operation} status=started"
    );
}

fn log_operation_finished(
    peer: &str,
    request_id: u64,
    operation: &str,
    started_at: Instant,
    error: Option<&str>,
) {
    let elapsed_ms = started_at.elapsed().as_millis();
    match error {
        Some(error) => eprintln!(
            "[operation] peer={peer} request_id={request_id} operation={operation} status=failed elapsed_ms={elapsed_ms} error={error}"
        ),
        None => eprintln!(
            "[operation] peer={peer} request_id={request_id} operation={operation} status=succeeded elapsed_ms={elapsed_ms}"
        ),
    }
}

fn is_quiet_disconnect(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::WouldBlock
    )
}

fn receive_upload(
    session: &mut Session,
    request_id: u64,
    args: UploadRequest,
    max_bytes: u64,
) -> Result<(), TransferError> {
    if args.size > max_bytes {
        return Err(
            AgentError::invalid(format!("file exceeds transfer limit ({max_bytes})")).into(),
        );
    }
    validate_sha256(&args.sha256)?;
    if args
        .mode
        .is_some_and(|mode| mode > remote_ops_protocol::MAX_UNIX_MODE)
    {
        return Err(AgentError::invalid("mode must be in range 0..=4095").into());
    }
    let target = normalized_path(&args.remote_path)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&target) {
        if !metadata.file_type().is_file() {
            return Err(AgentError::invalid(
                "destination must be a regular file and not a symlink",
            )
            .into());
        }
        if !args.overwrite {
            return Err(AgentError::invalid(format!(
                "destination already exists: {}",
                target.display()
            ))
            .into());
        }
    }
    let partial = upload_partial_path(&target, &args.sha256)?;
    let mut file = open_upload_partial(&partial, args.resume)?;
    let mut cleanup = UploadPartialCleanup::new(partial.clone(), args.resume);
    let mut offset = file
        .metadata()
        .map_err(|error| AgentError::io("stat upload partial file", error))?
        .len();
    if offset > args.size {
        file.set_len(0)
            .map_err(|error| AgentError::io("truncate upload partial file", error))?;
        offset = 0;
    }
    let (mut digest, prefix_sha256) = hash_prefix(&mut file, offset)?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| AgentError::io("seek upload partial file", error))?;
    session.send_json(
        FrameType::Response,
        request_id,
        &TransferMetadata {
            size: args.size,
            offset,
            prefix_sha256: (offset > 0).then_some(prefix_sha256),
        },
    )?;
    let mut total = offset;
    loop {
        let frame = session.receive_with_idle_timeout(Some(DEFAULT_TRANSFER_IDLE_TIMEOUT))?;
        if frame.request_id != request_id {
            return Err(AgentError::command("request id changed during upload").into());
        }
        match frame.kind {
            FrameType::Chunk => {
                total = total
                    .checked_add(frame.payload.len() as u64)
                    .ok_or_else(|| AgentError::invalid("transfer size overflow"))?;
                if total > args.size || total > max_bytes {
                    return Err(AgentError::invalid("upload exceeded declared size").into());
                }
                file.write_all(&frame.payload)
                    .map_err(|error| AgentError::io("write upload temporary file", error))?;
                digest.update(&frame.payload);
            }
            FrameType::End => {
                let end: TransferEnd = serde_json::from_slice(&frame.payload)
                    .map_err(|error| AgentError::invalid(error.to_string()))?;
                let actual = format!("{:x}", digest.finalize());
                if total != args.size
                    || end.size != total
                    || !end.sha256.eq_ignore_ascii_case(&actual)
                    || !args.sha256.eq_ignore_ascii_case(&actual)
                {
                    return Err(AgentError::invalid("upload length or SHA-256 mismatch").into());
                }
                file.flush()
                    .and_then(|_| file.sync_all())
                    .map_err(|error| AgentError::io("flush upload temporary file", error))?;
                if args.mode.is_some() {
                    set_optional_mode(&partial, args.mode)?;
                } else {
                    preserve_existing_mode(&target, &partial)?;
                }
                drop(file);
                persist_path_replace(&partial, &target, args.overwrite)?;
                cleanup.committed = true;
                let response = RemoteResponse::success(json!({
                    "bytes_transferred": total - offset,
                    "size": total,
                    "resumed_from": offset,
                    "sha256": actual,
                    "mode": args.mode
                }));
                session
                    .send_json(FrameType::Response, request_id, &response)
                    .map_err(TransferError::from)?;
                return Ok(());
            }
            _ => return Err(AgentError::command("unexpected frame during upload").into()),
        }
    }
}

struct UploadPartialCleanup {
    path: PathBuf,
    keep_on_failure: bool,
    committed: bool,
}

impl UploadPartialCleanup {
    fn new(path: PathBuf, keep_on_failure: bool) -> Self {
        Self {
            path,
            keep_on_failure,
            committed: false,
        }
    }
}

impl Drop for UploadPartialCleanup {
    fn drop(&mut self) {
        if !self.keep_on_failure && !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn send_download(
    session: &mut Session,
    request_id: u64,
    args: DownloadRequest,
    max_bytes: u64,
) -> Result<(), TransferError> {
    let path = normalized_path(&args.remote_path)?;
    let (mut file, size) = ensure_regular_file(&path)?;
    if size > max_bytes {
        return Err(
            AgentError::invalid(format!("file exceeds transfer limit ({max_bytes})")).into(),
        );
    }
    if args.offset > max_bytes {
        return Err(AgentError::invalid("download offset exceeds transfer limit").into());
    }
    if let Some(prefix) = &args.prefix_sha256 {
        validate_sha256(prefix)?;
    }
    let requested_offset = args.offset.min(size);
    let (prefix_digest, prefix_sha256) = hash_prefix(&mut file, requested_offset)?;
    let accepted_offset = if requested_offset == args.offset
        && (args.offset == 0
            || args
                .prefix_sha256
                .as_deref()
                .is_some_and(|expected| expected.eq_ignore_ascii_case(&prefix_sha256)))
    {
        args.offset
    } else {
        0
    };
    let mut digest = if accepted_offset == requested_offset {
        prefix_digest
    } else {
        Sha256::new()
    };
    file.seek(SeekFrom::Start(accepted_offset))
        .map_err(|error| AgentError::io(format!("seek {}", path.display()), error))?;
    session.send_json(
        FrameType::Response,
        request_id,
        &TransferMetadata {
            size,
            offset: accepted_offset,
            prefix_sha256: (accepted_offset > 0).then_some(prefix_sha256),
        },
    )?;
    let mut total = accepted_offset;
    let mut buffer = vec![0u8; DEFAULT_CHUNK_BYTES];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            AgentError::io(
                format!("read {}", Path::new(&args.remote_path).display()),
                error,
            )
        })?;
        if read == 0 {
            break;
        }
        session.send(FrameType::Chunk, request_id, &buffer[..read])?;
        digest.update(&buffer[..read]);
        total += read as u64;
    }
    let end = TransferEnd {
        size: total,
        sha256: format!("{:x}", digest.finalize()),
    };
    session
        .send_json(FrameType::End, request_id, &end)
        .map_err(TransferError::from)
}

fn upload_partial_path(target: &Path, sha256: &str) -> Result<PathBuf, AgentError> {
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AgentError::invalid("remote_path must name a UTF-8 file"))?;
    Ok(parent.join(format!(".remoteops-upload-{name}-{}.part", &sha256[..16])))
}

fn open_upload_partial(path: &Path, resume: bool) -> Result<File, AgentError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_file() {
            return Err(AgentError::invalid(
                "upload partial path is not a regular file",
            ));
        }
        if !resume {
            fs::remove_file(path).map_err(|error| {
                AgentError::io(
                    format!("remove stale upload partial {}", path.display()),
                    error,
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
        .map_err(|error| AgentError::io(format!("open upload partial {}", path.display()), error))
}

fn hash_prefix(file: &mut File, length: u64) -> Result<(Sha256, String), AgentError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| AgentError::io("seek transfer file", error))?;
    let mut digest = Sha256::new();
    let mut remaining = length;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let limit = buffer.len().min(remaining as usize);
        let read = file
            .read(&mut buffer[..limit])
            .map_err(|error| AgentError::io("read transfer prefix", error))?;
        if read == 0 {
            return Err(AgentError::invalid(
                "transfer prefix is shorter than declared",
            ));
        }
        digest.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let prefix = format!("{:x}", digest.clone().finalize());
    Ok((digest, prefix))
}

fn validate_sha256(value: &str) -> Result<(), AgentError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(AgentError::invalid(
            "SHA-256 must contain exactly 64 hexadecimal characters",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::is_quiet_disconnect;
    use std::io::{Error, ErrorKind};

    #[test]
    fn quiet_disconnects_only_include_normal_close_and_timeouts() {
        for kind in [
            ErrorKind::UnexpectedEof,
            ErrorKind::TimedOut,
            ErrorKind::WouldBlock,
        ] {
            assert!(is_quiet_disconnect(&Error::from(kind)), "{kind:?}");
        }

        for kind in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::BrokenPipe,
            ErrorKind::NotConnected,
        ] {
            assert!(!is_quiet_disconnect(&Error::from(kind)), "{kind:?}");
        }
    }
}
