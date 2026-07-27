use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::Instant;

use remote_ops_protocol::{
    BUILTIN_PSK, DEFAULT_CHUNK_BYTES, DEFAULT_MAX_CONTROL_BYTES, DEFAULT_TRANSFER_IDLE_TIMEOUT,
    DownloadRequest, FrameType, INTERNAL_PING_OPERATION, PROTOCOL_VERSION, ProtocolError,
    RemoteError, RemoteRequest, RemoteResponse, Session, SessionOptions, TransferEnd,
    TransferMetadata, UploadRequest,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::dispatch;
use crate::error::AgentError;
use crate::tools::files::{
    create_transfer_temp, ensure_regular_file, normalized_path, persist_replace,
    preserve_existing_mode,
};
use crate::tools::jobs::JobManager;

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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
            Err(ProtocolError::Io(error)) if is_quiet_disconnect(&error) => return Ok(()),
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
                &RemoteResponse::success(json!({"protocol_version": PROTOCOL_VERSION})),
            )?;
            continue;
        }
        let started_at = Instant::now();
        log_operation_started(&peer, frame.request_id, &request.operation);
        match request.operation.as_str() {
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
                let response = match dispatch(&request.operation, request.arguments, jobs) {
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
    let mut temp = create_transfer_temp(&target)?;
    session.send_json(
        FrameType::Response,
        request_id,
        &TransferMetadata { size: args.size },
    )?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
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
                temp.write_all(&frame.payload)
                    .map_err(|error| AgentError::io("write upload temporary file", error))?;
                digest.update(&frame.payload);
            }
            FrameType::End => {
                let end: TransferEnd = serde_json::from_slice(&frame.payload)
                    .map_err(|error| AgentError::invalid(error.to_string()))?;
                let actual = format!("{:x}", digest.finalize());
                if total != args.size || end.size != total || end.sha256 != actual {
                    return Err(AgentError::invalid("upload length or SHA-256 mismatch").into());
                }
                temp.flush()
                    .and_then(|_| temp.as_file().sync_all())
                    .map_err(|error| AgentError::io("flush upload temporary file", error))?;
                preserve_existing_mode(&target, temp.path())?;
                persist_replace(temp, &target, args.overwrite)?;
                let response =
                    RemoteResponse::success(json!({"bytes_transferred": total, "sha256": actual}));
                session
                    .send_json(FrameType::Response, request_id, &response)
                    .map_err(TransferError::from)?;
                return Ok(());
            }
            _ => return Err(AgentError::command("unexpected frame during upload").into()),
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
    session.send_json(FrameType::Response, request_id, &TransferMetadata { size })?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
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
