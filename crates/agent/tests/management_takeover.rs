use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use remote_ops_protocol::{
    BUILTIN_PSK, DEFAULT_MAX_CONTROL_BYTES, FrameType, RemoteRequest, RemoteResponse, Session,
    SessionOptions,
};
use serde_json::json;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn connect(address: std::net::SocketAddr) -> Session {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut options = SessionOptions::proxy(Duration::from_secs(2));
        options.connect_timeout = Duration::from_millis(100);
        options.handshake_timeout = Duration::from_millis(500);
        match Session::connect(address, BUILTIN_PSK, options, DEFAULT_MAX_CONTROL_BYTES) {
            Ok(session) => return session,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("agent did not accept a connection: {error}"),
        }
    }
}

fn call_agent_info(session: &mut Session, request_id: u64) {
    session
        .send_json(
            FrameType::Request,
            request_id,
            &RemoteRequest {
                operation: "agent_info".to_string(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let response: RemoteResponse = session
        .receive_json(FrameType::Response, request_id)
        .unwrap();
    assert!(response.ok);
    assert_eq!(response.result.unwrap()["name"], "remote-ops-agent");
}

#[test]
fn newer_authenticated_proxy_takes_over_real_agent_listener() {
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);

    let child = Command::new(env!("CARGO_BIN_EXE_remote-ops-agent"))
        .args(["--listen", &address.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _child = ChildGuard(child);

    let mut first = connect(address);
    call_agent_info(&mut first, 1);

    let mut second = connect(address);
    call_agent_info(&mut second, 1);

    thread::sleep(Duration::from_millis(150));
    let old_connection = first
        .send_json(
            FrameType::Request,
            2,
            &RemoteRequest {
                operation: "agent_info".to_string(),
                arguments: json!({}),
            },
        )
        .and_then(|_| first.receive().map(|_| ()));
    assert!(old_connection.is_err());
}
