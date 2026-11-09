use remote_ops_protocol::{
    BUILTIN_PSK, DEFAULT_MAX_CONTROL_BYTES, DEFAULT_MAX_TRANSFER_BYTES, FrameType, RemoteError,
    RemoteRequest, RemoteResponse, Session, SessionOptions,
};
use remote_ops_proxy::{
    client::RemoteClient,
    deployment::{SyncOptions, sync_directory},
    mcp::handle_message,
};
use serde_json::{Value, json};
use std::{
    fs,
    net::{SocketAddrV4, TcpListener},
    thread,
    time::Duration,
};

fn connect(listener: &TcpListener) -> Session {
    let (stream, _) = listener.accept().unwrap();
    Session::accept(
        stream,
        BUILTIN_PSK,
        SessionOptions::agent(),
        DEFAULT_MAX_CONTROL_BYTES,
    )
    .unwrap()
}
fn request(session: &mut Session) -> (u64, RemoteRequest) {
    let frame = session.receive().unwrap();
    assert_eq!(frame.kind, FrameType::Request);
    (
        frame.request_id,
        serde_json::from_slice(&frame.payload).unwrap(),
    )
}
fn reply(session: &mut Session, id: u64, value: Value) {
    session
        .send_json(FrameType::Response, id, &RemoteResponse::success(value))
        .unwrap();
}
fn client(address: SocketAddrV4) -> RemoteClient {
    RemoteClient::new(address, Duration::from_secs(1), DEFAULT_MAX_TRANSFER_BYTES)
}

#[test]
fn batch_disconnect_preserves_prior_effects_and_does_not_send_later_commands() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string().parse().unwrap();
    let root = tempfile::tempdir().unwrap();
    let effect = root.path().join("executed");
    let first_effect = effect.clone();
    let server = thread::spawn(move || {
        let mut session = connect(&listener);
        let (id, call) = request(&mut session);
        assert_eq!(call.operation, "exec");
        fs::write(first_effect, b"executed exactly once").unwrap();
        // The first mutation happened, but its response never arrives.
        drop(session);
        listener.set_nonblocking(true).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        id
    });
    let mut client = client(address);
    let response=handle_message(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"batch","arguments":{"calls":[
        {"tool":"exec","arguments":{"program":"first"}},
        {"tool":"exec","arguments":{"program":"must-not-run"}},
        {"tool":"remote_status"}
    ]}}}),&mut client).unwrap();
    let results = &response["result"]["structuredContent"]["results"];
    assert_eq!(results[0]["error"]["outcome"], "unknown");
    for index in [1, 2] {
        assert_eq!(results[index]["error"]["kind"], "not_executed");
        assert_eq!(results[index]["error"]["outcome"], "not_sent");
        assert_eq!(results[index]["error"]["request_started"], false);
        assert_eq!(results[index]["error"]["recovery"]["stopped_after"], 1);
    }
    assert_eq!(fs::read(effect).unwrap(), b"executed exactly once");
    assert_eq!(server.join().unwrap(), 1);
}

#[test]
fn ordinary_remote_batch_error_does_not_block_independent_inspection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string().parse().unwrap();
    let server = thread::spawn(move || {
        let mut session = connect(&listener);
        let (id, _) = request(&mut session);
        session
            .send_json(
                FrameType::Response,
                id,
                &RemoteResponse::failure(RemoteError {
                    kind: "io".into(),
                    message: "missing file".into(),
                }),
            )
            .unwrap();
        let (id, call) = request(&mut session);
        assert_eq!(call.operation, "stat");
        reply(&mut session, id, json!({"size":1}));
    });
    let mut client = client(address);
    let response=handle_message(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"batch","arguments":{"calls":[
        {"tool":"stat","arguments":{"path":"missing"}},
        {"tool":"stat","arguments":{"path":"exists"}}
    ]}}}),&mut client).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["results"][0]["error"]["outcome"],
        "remote_error"
    );
    assert_eq!(
        response["result"]["structuredContent"]["results"][1]["ok"],
        true
    );
    server.join().unwrap();
}

#[test]
fn busy_or_unavailable_results_stop_the_batch_without_a_second_request() {
    for kind in ["manager_busy", "response_too_large"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string().parse().unwrap();
        let server = thread::spawn(move || {
            let mut session = connect(&listener);
            let (id, _) = request(&mut session);
            session
                .send_json(
                    FrameType::Response,
                    id,
                    &RemoteResponse::failure(RemoteError {
                        kind: kind.into(),
                        message: "fixture".into(),
                    }),
                )
                .unwrap();
            thread::sleep(Duration::from_millis(50));
        });
        let mut client = client(address);
        let response=handle_message(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"batch","arguments":{"calls":[
            {"tool":"exec","arguments":{"program":"first"}},
            {"tool":"exec","arguments":{"program":"must-not-run"}}
        ]}}}),&mut client).unwrap();
        let results = &response["result"]["structuredContent"]["results"];
        assert_eq!(results[0]["error"]["kind"], kind);
        assert_eq!(results[1]["error"]["kind"], "not_executed");
        assert_eq!(results[1]["error"]["request_started"], false);
        server.join().unwrap();
    }
}

#[test]
fn lost_sync_commit_reply_never_reconnects_to_delete_recovery_state() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string().parse().unwrap();
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("local");
    fs::create_dir(&local).unwrap();
    let effect = root.path().join("committed");
    let commit_effect = effect.clone();
    let server = thread::spawn(move || {
        let mut session = connect(&listener);
        let (id, call) = request(&mut session);
        assert_eq!(call.operation, "agent_info");
        reply(&mut session, id, json!({"platform":{"family":"unix"}}));
        let (id, call) = request(&mut session);
        assert_eq!(call.operation, "sync_prepare");
        reply(
            &mut session,
            id,
            json!({"staging_path":"/tmp/.remoteops-sync-target-0123456789abcdef01234567","required_uploads":[],"reused_files":0,"reused_bytes":0}),
        );
        let (_, call) = request(&mut session);
        assert_eq!(call.operation, "sync_commit");
        fs::write(commit_effect, b"new tree active, backup retained").unwrap();
        drop(session);
        listener.set_nonblocking(true).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    });
    let mut client = client(address);
    let error = sync_directory(
        &mut client,
        local.to_str().unwrap(),
        "/tmp/target",
        SyncOptions {
            excludes: vec![],
            max_files: 10,
            max_total_bytes: 1024,
            max_depth: 4,
        },
    )
    .unwrap_err();
    assert_eq!(error.details()["outcome"], "unknown");
    assert_eq!(error.details()["recovery"]["phase"], "commit");
    assert_eq!(
        error.details()["recovery"]["cleanup"]["status"],
        "not_attempted"
    );
    assert_eq!(
        fs::read(effect).unwrap(),
        b"new tree active, backup retained"
    );
    server.join().unwrap();
}
