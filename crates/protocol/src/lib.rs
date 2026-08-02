use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use socket2::{SockRef, TcpKeepalive};

pub const PROTOCOL_VERSION: u8 = 3;
pub const BUILTIN_PSK: &[u8] = b"JARK006_PSK";
pub const DEFAULT_MAX_CONTROL_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_TRANSFER_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const APPLY_PATCH_MAX_PATCH_BYTES: usize = 256 * 1024;
pub const APPLY_PATCH_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const APPLY_PATCH_MAX_HUNKS: usize = 128;
pub const MAX_PROCESS_JOBS: usize = 16;
pub const DEFAULT_PROCESS_JOB_TIMEOUT_MS: u64 = 60 * 60 * 1000;
pub const MAX_PROCESS_JOB_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;
pub const PROCESS_OUTPUT_BUFFER_BYTES: usize = 256 * 1024;
pub const DEFAULT_PROCESS_OUTPUT_BYTES: usize = 64 * 1024;
pub const MAX_PROCESS_OUTPUT_BYTES: usize = 256 * 1024;
pub const DEFAULT_PROCESS_WAIT_MS: u64 = 10_000;
pub const MAX_PROCESS_WAIT_MS: u64 = 30_000;
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_FRAME_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
pub const DEFAULT_HEALTH_CHECK_AFTER: Duration = Duration::from_secs(60);
pub const DEFAULT_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_REMOTE_PROBE_TIMEOUT_MS: u64 = 5_000;
pub const MIN_REMOTE_PROBE_TIMEOUT_MS: u64 = 100;
pub const MAX_REMOTE_PROBE_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_WAIT_REMOTE_TIMEOUT_MS: u64 = 120_000;
pub const MAX_WAIT_REMOTE_TIMEOUT_MS: u64 = 600_000;
pub const DEFAULT_WAIT_REMOTE_POLL_MS: u64 = 1_000;
pub const MIN_WAIT_REMOTE_POLL_MS: u64 = 100;
pub const MAX_WAIT_REMOTE_POLL_MS: u64 = 10_000;
pub const DEFAULT_REBOOT_DELAY_MS: u64 = 1_000;
pub const MIN_REBOOT_DELAY_MS: u64 = 250;
pub const MAX_REBOOT_DELAY_MS: u64 = 10_000;
pub const MAX_UNIX_MODE: u32 = 0o7777;
pub const MAX_FILE_OPERATION_ENTRIES: usize = 100_000;
pub const DEFAULT_SYNC_MAX_FILES: usize = 4_096;
pub const MAX_SYNC_FILES: usize = 10_000;
pub const DEFAULT_SYNC_MAX_DEPTH: usize = 32;
pub const MAX_SYNC_DEPTH: usize = 64;
pub const MAX_SYNC_EXCLUDE_PATTERNS: usize = 64;
pub const MAX_SYNC_GLOB_BYTES: usize = 1_024;
pub const MAX_DEPLOY_DEPENDENCIES: usize = 64;
pub const MAX_RELEASE_ID_BYTES: usize = 128;
pub const INTERNAL_PING_OPERATION: &str = "__remote_ops_ping";

const HELLO_MAGIC: &[u8; 4] = b"ROPS";
const FRAME_HEADER_BYTES: usize = 28;
const MAC_BYTES: usize = 32;

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    Json(serde_json::Error),
    Authentication,
    InvalidFrame(String),
    UnexpectedFrame {
        expected: FrameType,
        actual: FrameType,
    },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Json(err) => write!(f, "JSON error: {err}"),
            Self::Authentication => write!(f, "authentication failed"),
            Self::InvalidFrame(message) => write!(f, "invalid frame: {message}"),
            Self::UnexpectedFrame { expected, actual } => {
                write!(f, "expected {expected:?} frame, received {actual:?}")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<io::Error> for ProtocolError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Request = 1,
    Response = 2,
    Chunk = 3,
    End = 4,
    Error = 5,
}

impl TryFrom<u8> for FrameType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Response),
            3 => Ok(Self::Chunk),
            4 => Ok(Self::End),
            5 => Ok(Self::Error),
            _ => Err(ProtocolError::InvalidFrame(format!(
                "unknown frame type {value}"
            ))),
        }
    }
}

#[derive(Debug)]
pub struct Frame {
    pub kind: FrameType,
    pub request_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteRequest {
    pub operation: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RemoteError>,
}

impl RemoteResponse {
    pub fn success(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(error: RemoteError) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteError {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadRequest {
    pub remote_path: String,
    pub size: u64,
    pub sha256: String,
    pub overwrite: bool,
    pub resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadRequest {
    pub remote_path: String,
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_sha256: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferMetadata {
    pub size: u64,
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_sha256: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferEnd {
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyncEntry {
    pub path: String,
    pub kind: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncPrepareRequest {
    pub remote_path: String,
    pub manifest_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_mode: Option<u32>,
    pub entries: Vec<SyncEntry>,
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncFinishRequest {
    pub remote_path: String,
    pub staging_path: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployPreflightRequest {
    pub releases_path: String,
    pub current_path: String,
    pub release_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_arch: Option<String>,
    pub required_bytes: u64,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployActivateRequest {
    pub release_path: String,
    pub current_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<CommandSpec>,
    pub start: CommandSpec,
    pub health: CommandSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_start: Option<CommandSpec>,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionOptions {
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub idle_read_timeout: Option<Duration>,
    pub frame_timeout: Duration,
    pub write_timeout: Duration,
}

impl SessionOptions {
    pub fn agent() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            idle_read_timeout: None,
            frame_timeout: DEFAULT_FRAME_TIMEOUT,
            write_timeout: DEFAULT_WRITE_TIMEOUT,
        }
    }

    pub fn proxy(operation_timeout: Duration) -> Self {
        Self {
            idle_read_timeout: Some(operation_timeout),
            ..Self::agent()
        }
    }
}

pub struct Session {
    stream: TcpStream,
    key: [u8; 32],
    send_sequence: u64,
    receive_sequence: u64,
    max_control_bytes: usize,
    options: SessionOptions,
}

impl Session {
    pub fn connect(
        address: SocketAddr,
        psk: &[u8],
        options: SessionOptions,
        max_control_bytes: usize,
    ) -> Result<Self, ProtocolError> {
        let mut stream = TcpStream::connect_timeout(&address, options.connect_timeout)?;
        configure_handshake(&stream, options.handshake_timeout)?;
        let key = client_handshake(&mut stream, psk)?;
        configure_established(&stream, options.write_timeout)?;
        Ok(Self::new(stream, key, max_control_bytes, options))
    }

    pub fn accept(
        mut stream: TcpStream,
        psk: &[u8],
        options: SessionOptions,
        max_control_bytes: usize,
    ) -> Result<Self, ProtocolError> {
        configure_handshake(&stream, options.handshake_timeout)?;
        let key = server_handshake(&mut stream, psk)?;
        configure_established(&stream, options.write_timeout)?;
        Ok(Self::new(stream, key, max_control_bytes, options))
    }

    fn new(
        stream: TcpStream,
        key: [u8; 32],
        max_control_bytes: usize,
        options: SessionOptions,
    ) -> Self {
        Self {
            stream,
            key,
            send_sequence: 0,
            receive_sequence: 0,
            max_control_bytes,
            options,
        }
    }

    pub fn send_json<T: Serialize>(
        &mut self,
        kind: FrameType,
        request_id: u64,
        value: &T,
    ) -> Result<(), ProtocolError> {
        let payload = serde_json::to_vec(value)?;
        if payload.len() > self.max_control_bytes {
            return Err(ProtocolError::InvalidFrame(format!(
                "control payload exceeds {} bytes",
                self.max_control_bytes
            )));
        }
        self.send(kind, request_id, &payload)
    }

    pub fn receive_json<T: for<'de> Deserialize<'de>>(
        &mut self,
        expected: FrameType,
        request_id: u64,
    ) -> Result<T, ProtocolError> {
        let frame = self.receive()?;
        if frame.kind != expected {
            return Err(ProtocolError::UnexpectedFrame {
                expected,
                actual: frame.kind,
            });
        }
        if frame.request_id != request_id {
            return Err(ProtocolError::InvalidFrame(
                "request id mismatch".to_string(),
            ));
        }
        Ok(serde_json::from_slice(&frame.payload)?)
    }

    pub fn send(
        &mut self,
        kind: FrameType,
        request_id: u64,
        payload: &[u8],
    ) -> Result<(), ProtocolError> {
        if kind != FrameType::Chunk && payload.len() > self.max_control_bytes {
            return Err(ProtocolError::InvalidFrame(
                "control payload too large".to_string(),
            ));
        }
        let payload_len = u32::try_from(payload.len())
            .map_err(|_| ProtocolError::InvalidFrame("payload too large".to_string()))?;
        let sequence = self.send_sequence;
        let header = make_header(kind, request_id, sequence, payload_len);
        let tag = sign(&self.key, &header, payload);
        let deadline = Instant::now() + self.options.write_timeout;
        write_all_until(&mut self.stream, &header, deadline)?;
        write_all_until(&mut self.stream, &tag, deadline)?;
        write_all_until(&mut self.stream, payload, deadline)?;
        self.stream
            .set_write_timeout(Some(remaining_until(deadline)?))?;
        self.stream.flush()?;
        self.send_sequence = self
            .send_sequence
            .checked_add(1)
            .ok_or_else(|| ProtocolError::InvalidFrame("send sequence exhausted".to_string()))?;
        Ok(())
    }

    pub fn receive(&mut self) -> Result<Frame, ProtocolError> {
        self.receive_with_idle_timeout(self.options.idle_read_timeout)
    }

    pub fn receive_with_idle_timeout(
        &mut self,
        idle_timeout: Option<Duration>,
    ) -> Result<Frame, ProtocolError> {
        let mut header = [0u8; FRAME_HEADER_BYTES];
        self.stream.set_read_timeout(idle_timeout)?;
        self.stream.read_exact(&mut header[..1])?;
        let deadline = Instant::now() + self.options.frame_timeout;
        read_exact_until(&mut self.stream, &mut header[1..], deadline)?;
        if &header[..4] != HELLO_MAGIC || header[4] != PROTOCOL_VERSION || header[6..8] != [0, 0] {
            return Err(ProtocolError::InvalidFrame(
                "magic or version mismatch".to_string(),
            ));
        }
        let kind = FrameType::try_from(header[5])?;
        let request_id = u64::from_be_bytes(header[8..16].try_into().expect("fixed slice"));
        let sequence = u64::from_be_bytes(header[16..24].try_into().expect("fixed slice"));
        if sequence != self.receive_sequence {
            return Err(ProtocolError::InvalidFrame(
                "frame sequence mismatch".to_string(),
            ));
        }
        let payload_len =
            u32::from_be_bytes(header[24..28].try_into().expect("fixed slice")) as usize;
        if kind != FrameType::Chunk && payload_len > self.max_control_bytes {
            return Err(ProtocolError::InvalidFrame(
                "control payload too large".to_string(),
            ));
        }
        if kind == FrameType::Chunk && payload_len > DEFAULT_CHUNK_BYTES {
            return Err(ProtocolError::InvalidFrame(
                "chunk payload too large".to_string(),
            ));
        }
        let mut received_tag = [0u8; MAC_BYTES];
        read_exact_until(&mut self.stream, &mut received_tag, deadline)?;
        let mut payload = vec![0u8; payload_len];
        read_exact_until(&mut self.stream, &mut payload, deadline)?;
        verify(&self.key, &header, &payload, &received_tag)?;
        self.receive_sequence = self
            .receive_sequence
            .checked_add(1)
            .ok_or_else(|| ProtocolError::InvalidFrame("receive sequence exhausted".to_string()))?;
        Ok(Frame {
            kind,
            request_id,
            payload,
        })
    }
}

fn configure_handshake(stream: &TcpStream, timeout: Duration) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))
}

fn configure_established(stream: &TcpStream, write_timeout: Duration) -> io::Result<()> {
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(write_timeout))?;
    let keepalive = TcpKeepalive::new()
        .with_time(DEFAULT_KEEPALIVE_IDLE)
        .with_interval(DEFAULT_KEEPALIVE_INTERVAL);
    SockRef::from(stream).set_tcp_keepalive(&keepalive)
}

fn remaining_until(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "frame I/O deadline exceeded"))
}

fn read_exact_until(
    stream: &mut TcpStream,
    mut buffer: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    while !buffer.is_empty() {
        stream.set_read_timeout(Some(remaining_until(deadline)?))?;
        match stream.read(buffer) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed while receiving frame",
                ));
            }
            Ok(read) => buffer = &mut buffer[read..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_all_until(stream: &mut TcpStream, mut buffer: &[u8], deadline: Instant) -> io::Result<()> {
    while !buffer.is_empty() {
        stream.set_write_timeout(Some(remaining_until(deadline)?))?;
        match stream.write(buffer) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to make progress while sending frame",
                ));
            }
            Ok(written) => buffer = &buffer[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn make_header(
    kind: FrameType,
    request_id: u64,
    sequence: u64,
    payload_len: u32,
) -> [u8; FRAME_HEADER_BYTES] {
    let mut header = [0u8; FRAME_HEADER_BYTES];
    header[..4].copy_from_slice(HELLO_MAGIC);
    header[4] = PROTOCOL_VERSION;
    header[5] = kind as u8;
    header[8..16].copy_from_slice(&request_id.to_be_bytes());
    header[16..24].copy_from_slice(&sequence.to_be_bytes());
    header[24..28].copy_from_slice(&payload_len.to_be_bytes());
    header
}

fn sign(key: &[u8], header: &[u8], payload: &[u8]) -> [u8; MAC_BYTES] {
    hmac_sha256(key, &[header, payload])
}

fn verify(key: &[u8], header: &[u8], payload: &[u8], tag: &[u8]) -> Result<(), ProtocolError> {
    let expected = sign(key, header, payload);
    if constant_time_eq(&expected, tag) {
        Ok(())
    } else {
        Err(ProtocolError::Authentication)
    }
}

fn client_handshake(stream: &mut TcpStream, psk: &[u8]) -> Result<[u8; 32], ProtocolError> {
    validate_psk(psk)?;
    let mut client_nonce = [0u8; 32];
    getrandom::fill(&mut client_nonce)
        .map_err(|error| ProtocolError::Io(io::Error::other(error.to_string())))?;
    stream.write_all(HELLO_MAGIC)?;
    stream.write_all(&[PROTOCOL_VERSION])?;
    stream.write_all(&client_nonce)?;
    stream.flush()?;

    let mut response = [0u8; 64];
    stream.read_exact(&mut response)?;
    let server_nonce: [u8; 32] = response[..32].try_into().expect("fixed slice");
    let expected = handshake_tag(psk, b"server", &client_nonce, &server_nonce);
    if !constant_time_eq(&expected, &response[32..]) {
        return Err(ProtocolError::Authentication);
    }
    let client_tag = handshake_tag(psk, b"client", &client_nonce, &server_nonce);
    stream.write_all(&client_tag)?;
    stream.flush()?;
    Ok(handshake_tag(psk, b"session", &client_nonce, &server_nonce))
}

fn server_handshake(stream: &mut TcpStream, psk: &[u8]) -> Result<[u8; 32], ProtocolError> {
    validate_psk(psk)?;
    let mut hello = [0u8; 37];
    stream.read_exact(&mut hello)?;
    if &hello[..4] != HELLO_MAGIC || hello[4] != PROTOCOL_VERSION {
        return Err(ProtocolError::Authentication);
    }
    let client_nonce: [u8; 32] = hello[5..].try_into().expect("fixed slice");
    let mut server_nonce = [0u8; 32];
    getrandom::fill(&mut server_nonce)
        .map_err(|error| ProtocolError::Io(io::Error::other(error.to_string())))?;
    let server_tag = handshake_tag(psk, b"server", &client_nonce, &server_nonce);
    stream.write_all(&server_nonce)?;
    stream.write_all(&server_tag)?;
    stream.flush()?;
    let mut client_tag = [0u8; 32];
    stream.read_exact(&mut client_tag)?;
    let expected = handshake_tag(psk, b"client", &client_nonce, &server_nonce);
    if !constant_time_eq(&expected, &client_tag) {
        return Err(ProtocolError::Authentication);
    }
    Ok(handshake_tag(psk, b"session", &client_nonce, &server_nonce))
}

fn validate_psk(psk: &[u8]) -> Result<(), ProtocolError> {
    if psk.is_empty() {
        Err(ProtocolError::InvalidFrame(
            "PSK must not be empty".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn handshake_tag(
    psk: &[u8],
    label: &[u8],
    client_nonce: &[u8; 32],
    server_nonce: &[u8; 32],
) -> [u8; 32] {
    hmac_sha256(psk, &[label, client_nonce, server_nonce])
}

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;
    let mut normalized = [0u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; BLOCK_BYTES];
    let mut outer_pad = [0x5cu8; BLOCK_BYTES];
    for index in 0..BLOCK_BYTES {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    for part in parts {
        inner.update(part);
    }
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn constant_time_eq(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .iter()
        .zip(actual)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    #[test]
    fn authenticated_sessions_exchange_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut session = Session::accept(
                stream,
                b"a sufficiently long psk",
                SessionOptions::agent(),
                1024,
            )
            .unwrap();
            let request: RemoteRequest = session.receive_json(FrameType::Request, 7).unwrap();
            assert_eq!(request.operation, "ping");
            session
                .send_json(
                    FrameType::Response,
                    7,
                    &RemoteResponse::success(Value::Null),
                )
                .unwrap();
        });
        let mut client = Session::connect(
            address,
            b"a sufficiently long psk",
            SessionOptions::proxy(Duration::from_secs(2)),
            1024,
        )
        .unwrap();
        client
            .send_json(
                FrameType::Request,
                7,
                &RemoteRequest {
                    operation: "ping".into(),
                    arguments: Value::Null,
                },
            )
            .unwrap();
        let response: RemoteResponse = client.receive_json(FrameType::Response, 7).unwrap();
        assert!(response.ok);
        server.join().unwrap();
    }

    #[test]
    fn empty_psk_is_rejected() {
        assert!(matches!(
            validate_psk(b""),
            Err(ProtocolError::InvalidFrame(_))
        ));
    }

    #[test]
    fn built_in_psk_is_the_deployment_value() {
        assert_eq!(BUILTIN_PSK, b"JARK006_PSK");
        assert!(validate_psk(BUILTIN_PSK).is_ok());
    }

    #[test]
    fn protocol_v3_transfer_requests_require_resume_integrity_fields() {
        assert_eq!(PROTOCOL_VERSION, 3);
        assert!(
            serde_json::from_value::<UploadRequest>(serde_json::json!({
                "remote_path":"file","size":1,"overwrite":true
            }))
            .is_err()
        );
        let request = UploadRequest {
            remote_path: "file".to_string(),
            size: 1,
            sha256: "0".repeat(64),
            overwrite: true,
            resume: true,
            mode: Some(0o755),
        };
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["resume"], true);
        assert_eq!(encoded["mode"], 0o755);
    }

    #[test]
    fn hmac_matches_rfc_4231_test_case_one() {
        let key = [0x0bu8; 20];
        let actual = hmac_sha256(&key, &[b"Hi There"]);
        let actual_hex = actual
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            actual_hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn mismatched_psk_fails_authentication() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            Session::accept(
                stream,
                b"server pre-shared key",
                SessionOptions::agent(),
                1024,
            )
        });
        let client = Session::connect(
            address,
            b"different client key",
            SessionOptions::proxy(Duration::from_secs(2)),
            1024,
        );
        assert!(matches!(client, Err(ProtocolError::Authentication)));
        assert!(server.join().unwrap().is_err());
    }

    #[test]
    fn mismatched_protocol_version_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            Session::accept(
                stream,
                b"a sufficiently long psk",
                SessionOptions::agent(),
                1024,
            )
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(HELLO_MAGIC).unwrap();
        stream
            .write_all(&[PROTOCOL_VERSION.wrapping_sub(1)])
            .unwrap();
        stream.write_all(&[0; 32]).unwrap();
        stream.flush().unwrap();

        assert!(matches!(
            server.join().unwrap(),
            Err(ProtocolError::Authentication)
        ));
    }

    #[test]
    fn replayed_frame_sequence_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut session = Session::accept(
                stream,
                b"a sufficiently long psk",
                SessionOptions::agent(),
                1024,
            )
            .unwrap();
            assert_eq!(session.receive().unwrap().request_id, 9);
            assert!(matches!(
                session.receive(),
                Err(ProtocolError::InvalidFrame(message)) if message.contains("sequence")
            ));
        });
        let mut client = Session::connect(
            address,
            b"a sufficiently long psk",
            SessionOptions::proxy(Duration::from_secs(2)),
            1024,
        )
        .unwrap();
        let header = make_header(FrameType::Request, 9, 0, 0);
        let tag = sign(&client.key, &header, &[]);
        for _ in 0..2 {
            client.stream.write_all(&header).unwrap();
            client.stream.write_all(&tag).unwrap();
            client.stream.flush().unwrap();
        }
        server.join().unwrap();
    }

    #[test]
    fn modified_payload_fails_frame_authentication() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut session = Session::accept(
                stream,
                b"a sufficiently long psk",
                SessionOptions::agent(),
                1024,
            )
            .unwrap();
            assert!(matches!(
                session.receive(),
                Err(ProtocolError::Authentication)
            ));
        });
        let mut client = Session::connect(
            address,
            b"a sufficiently long psk",
            SessionOptions::proxy(Duration::from_secs(2)),
            1024,
        )
        .unwrap();
        let header = make_header(FrameType::Request, 10, 0, 3);
        let tag = sign(&client.key, &header, b"one");
        client.stream.write_all(&header).unwrap();
        client.stream.write_all(&tag).unwrap();
        client.stream.write_all(b"two").unwrap();
        client.stream.flush().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn authenticated_session_can_idle_past_handshake_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut options = SessionOptions::agent();
            options.handshake_timeout = Duration::from_millis(50);
            let mut session =
                Session::accept(stream, b"a sufficiently long psk", options, 1024).unwrap();
            let request: RemoteRequest = session.receive_json(FrameType::Request, 1).unwrap();
            assert_eq!(request.operation, "after-idle");
        });
        let mut client = Session::connect(
            address,
            b"a sufficiently long psk",
            SessionOptions::proxy(Duration::from_secs(2)),
            1024,
        )
        .unwrap();
        thread::sleep(Duration::from_millis(150));
        client
            .send_json(
                FrameType::Request,
                1,
                &RemoteRequest {
                    operation: "after-idle".into(),
                    arguments: Value::Null,
                },
            )
            .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn incomplete_handshake_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut options = SessionOptions::agent();
            options.handshake_timeout = Duration::from_millis(50);
            Session::accept(stream, b"a sufficiently long psk", options, 1024)
        });
        let _silent_client = TcpStream::connect(address).unwrap();
        assert!(matches!(
            server.join().unwrap(),
            Err(ProtocolError::Io(error))
                if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
        ));
    }

    #[test]
    fn partially_started_frame_has_a_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut options = SessionOptions::agent();
            options.frame_timeout = Duration::from_millis(50);
            let mut session =
                Session::accept(stream, b"a sufficiently long psk", options, 1024).unwrap();
            session.receive()
        });
        let mut client = Session::connect(
            address,
            b"a sufficiently long psk",
            SessionOptions::proxy(Duration::from_secs(2)),
            1024,
        )
        .unwrap();
        client.stream.write_all(&HELLO_MAGIC[..1]).unwrap();
        client.stream.flush().unwrap();
        assert!(matches!(
            server.join().unwrap(),
            Err(ProtocolError::Io(error))
                if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
        ));
    }
}
