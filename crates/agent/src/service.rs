use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use remote_ops_protocol::{
    BUILTIN_PSK, DEFAULT_CHUNK_BYTES, DEFAULT_MAX_CONTROL_BYTES, DownloadRequest, FrameType,
    ProtocolError, RemoteError, RemoteRequest, RemoteResponse, Session, TransferEnd,
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

pub fn handle_connection(
    stream: TcpStream,
    timeout: Duration,
    max_transfer_bytes: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut session = Session::accept(stream, BUILTIN_PSK, timeout, DEFAULT_MAX_CONTROL_BYTES)?;
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
        match request.operation.as_str() {
            "upload_file" => {
                let args: UploadRequest = match serde_json::from_value(request.arguments) {
                    Ok(args) => args,
                    Err(error) => {
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
                if let Err(error) =
                    receive_upload(&mut session, frame.request_id, args, max_transfer_bytes)
                {
                    session.send_json(
                        FrameType::Error,
                        frame.request_id,
                        &RemoteError::from(error),
                    )?;
                }
            }
            "download_file" => {
                let args: DownloadRequest = match serde_json::from_value(request.arguments) {
                    Ok(args) => args,
                    Err(error) => {
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
                if let Err(error) =
                    send_download(&mut session, frame.request_id, args, max_transfer_bytes)
                {
                    session.send_json(
                        FrameType::Error,
                        frame.request_id,
                        &RemoteError::from(error),
                    )?;
                }
            }
            _ => {
                let response = match dispatch(&request.operation, request.arguments) {
                    Ok(value) => RemoteResponse::success(value),
                    Err(error) => RemoteResponse::failure(error.into()),
                };
                session.send_json(FrameType::Response, frame.request_id, &response)?;
            }
        }
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
) -> Result<(), AgentError> {
    if args.size > max_bytes {
        return Err(AgentError::invalid(format!(
            "file exceeds transfer limit ({max_bytes})"
        )));
    }
    let target = normalized_path(&args.remote_path)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&target) {
        if !metadata.file_type().is_file() {
            return Err(AgentError::invalid(
                "destination must be a regular file and not a symlink",
            ));
        }
        if !args.overwrite {
            return Err(AgentError::invalid(format!(
                "destination already exists: {}",
                target.display()
            )));
        }
    }
    let mut temp = create_transfer_temp(&target)?;
    session
        .send_json(
            FrameType::Response,
            request_id,
            &TransferMetadata { size: args.size },
        )
        .map_err(|error| AgentError::command(error.to_string()))?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    loop {
        let frame = session
            .receive()
            .map_err(|error| AgentError::command(error.to_string()))?;
        if frame.request_id != request_id {
            return Err(AgentError::command("request id changed during upload"));
        }
        match frame.kind {
            FrameType::Chunk => {
                total = total
                    .checked_add(frame.payload.len() as u64)
                    .ok_or_else(|| AgentError::invalid("transfer size overflow"))?;
                if total > args.size || total > max_bytes {
                    return Err(AgentError::invalid("upload exceeded declared size"));
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
                    return Err(AgentError::invalid("upload length or SHA-256 mismatch"));
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
                    .map_err(|error| AgentError::command(error.to_string()))?;
                return Ok(());
            }
            _ => return Err(AgentError::command("unexpected frame during upload")),
        }
    }
}

fn send_download(
    session: &mut Session,
    request_id: u64,
    args: DownloadRequest,
    max_bytes: u64,
) -> Result<(), AgentError> {
    let path = normalized_path(&args.remote_path)?;
    let (mut file, size) = ensure_regular_file(&path)?;
    if size > max_bytes {
        return Err(AgentError::invalid(format!(
            "file exceeds transfer limit ({max_bytes})"
        )));
    }
    session
        .send_json(FrameType::Response, request_id, &TransferMetadata { size })
        .map_err(|error| AgentError::command(error.to_string()))?;
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
        session
            .send(FrameType::Chunk, request_id, &buffer[..read])
            .map_err(|error| AgentError::command(error.to_string()))?;
        digest.update(&buffer[..read]);
        total += read as u64;
    }
    let end = TransferEnd {
        size: total,
        sha256: format!("{:x}", digest.finalize()),
    };
    session
        .send_json(FrameType::End, request_id, &end)
        .map_err(|error| AgentError::command(error.to_string()))
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
