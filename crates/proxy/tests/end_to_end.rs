use std::fs;
use std::io::Write;
use std::net::{SocketAddrV4, TcpListener};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use remote_ops_agent::service::handle_connection;
use remote_ops_agent::tools::jobs::JobManager;
use remote_ops_protocol::{
    BUILTIN_PSK, DEFAULT_MAX_CONTROL_BYTES, DEFAULT_MAX_TRANSFER_BYTES, FrameType,
    INTERNAL_PING_OPERATION, PROTOCOL_VERSION, RemoteRequest, RemoteResponse, Session,
    SessionOptions,
};
use remote_ops_proxy::client::RemoteClient;
use serde_json::{Value, json};

const BACKGROUND_JOB_TEST_ROLE: &str = "REMOTE_OPS_BACKGROUND_E2E_ROLE";

#[test]
fn background_job_output_survives_proxy_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let jobs = JobManager::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &jobs).unwrap();
        }
    });
    let program = std::env::current_exe().unwrap();
    let program = program.to_string_lossy().into_owned();
    let mut first_client = RemoteClient::new(
        address.to_string().parse().unwrap(),
        Duration::from_secs(5),
        DEFAULT_MAX_TRANSFER_BYTES,
    );
    let started = first_client
        .call(
            "process_start",
            json!({
                "program": program,
                "args": ["--exact", "background_job_end_to_end_helper", "--nocapture"],
                "env": {"REMOTE_OPS_BACKGROUND_E2E_ROLE": "1"},
                "timeout_ms": 5_000
            }),
        )
        .unwrap();
    let job_id = started["job_id"].as_u64().unwrap();
    assert_eq!(started["state"], "running");

    let system = first_client.call("system_info", json!({})).unwrap();
    assert!(!system["hostname"].as_str().unwrap().is_empty());

    let deadline = Instant::now() + Duration::from_secs(5);
    let first_output = loop {
        let output = first_client
            .call(
                "process_output",
                json!({"job_id":job_id,"stdout_cursor":0,"stderr_cursor":0}),
            )
            .unwrap();
        if output["stdout"].as_str().unwrap().contains("e2e-first") {
            break output;
        }
        assert!(
            Instant::now() < deadline,
            "first job output was not observed"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(first_output["state"], "running");
    let stdout_cursor = first_output["next_stdout_cursor"].as_u64().unwrap();
    let stderr_cursor = first_output["next_stderr_cursor"].as_u64().unwrap();
    drop(first_client);

    let mut second_client = RemoteClient::new(
        address.to_string().parse().unwrap(),
        Duration::from_secs(5),
        DEFAULT_MAX_TRANSFER_BYTES,
    );
    let finished = second_client
        .call("process_wait", json!({"job_id":job_id,"wait_ms":5_000}))
        .unwrap();
    assert_eq!(finished["state"], "exited");
    assert_eq!(finished["exit_code"], 6);
    let remaining = second_client
        .call(
            "process_output",
            json!({
                "job_id":job_id,
                "stdout_cursor":stdout_cursor,
                "stderr_cursor":stderr_cursor
            }),
        )
        .unwrap();
    assert!(remaining["stdout"].as_str().unwrap().contains("e2e-second"));
    assert!(remaining["stderr"].as_str().unwrap().contains("e2e-error"));
    assert_eq!(remaining["stdout_truncated"], false);
    assert_eq!(
        second_client
            .call("process_close", json!({"job_id":job_id}))
            .unwrap()["closed"],
        true
    );
    drop(second_client);
    server.join().unwrap();
}

#[test]
fn background_job_end_to_end_helper() {
    if std::env::var_os(BACKGROUND_JOB_TEST_ROLE).is_none() {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "e2e-first").unwrap();
    stdout.flush().unwrap();
    thread::sleep(Duration::from_secs(1));
    writeln!(stdout, "e2e-second").unwrap();
    stdout.flush().unwrap();
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "e2e-error").unwrap();
    stderr.flush().unwrap();
    std::process::exit(6);
}

#[test]
fn binary_file_round_trip_crosses_multiple_chunks() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let jobs = JobManager::new();
        handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &jobs).unwrap();
    });

    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let source = local.path().join("source.bin");
    let remote_file = remote.path().join("remote.bin");
    let downloaded = local.path().join("downloaded.bin");
    let bytes: Vec<u8> = (0..150_000).map(|index| (index % 251) as u8).collect();
    fs::write(&source, &bytes).unwrap();

    {
        let mut client = RemoteClient::new(
            address.to_string().parse().unwrap(),
            Duration::from_secs(5),
            DEFAULT_MAX_TRANSFER_BYTES,
        );
        let upload = client
            .upload(
                &source.to_string_lossy(),
                &remote_file.to_string_lossy(),
                true,
            )
            .unwrap();
        assert_eq!(upload["bytes_transferred"], 150_000);
        assert_eq!(fs::read(&remote_file).unwrap(), bytes);

        let stat = client.call("stat", json!({"path": remote_file})).unwrap();
        assert_eq!(stat["size"], 150_000);

        let download = client
            .download(
                &remote_file.to_string_lossy(),
                &downloaded.to_string_lossy(),
                true,
            )
            .unwrap();
        assert_eq!(download["bytes_transferred"], 150_000);
        assert_eq!(upload["sha256"], download["sha256"]);
        assert_eq!(fs::read(downloaded).unwrap(), bytes);
    }
    server.join().unwrap();
}

#[test]
fn file_discovery_tools_cross_the_remote_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let jobs = JobManager::new();
        handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &jobs).unwrap();
    });
    let remote = tempfile::tempdir().unwrap();
    fs::create_dir_all(remote.path().join("src/nested")).unwrap();
    fs::write(
        remote.path().join("src/main.rs"),
        "first\nremote needle\nthird\n",
    )
    .unwrap();
    fs::write(remote.path().join("src/nested/lib.rs"), "no match\n").unwrap();

    {
        let mut client = RemoteClient::new(
            address.to_string().parse().unwrap(),
            Duration::from_secs(5),
            DEFAULT_MAX_TRANSFER_BYTES,
        );
        let lines = client
            .call(
                "read_file_lines",
                json!({
                    "path": remote.path().join("src/main.rs"),
                    "start_line": 2,
                    "end_line": 2
                }),
            )
            .unwrap();
        assert_eq!(lines["text"], "remote needle\n");

        let search = client
            .call(
                "grep",
                json!({"path":remote.path(),"pattern":"needle","glob":"*.rs"}),
            )
            .unwrap();
        assert_eq!(search["matches"].as_array().unwrap().len(), 1);
        assert_eq!(search["matches"][0]["path"], "src/main.rs");

        let listing = client
            .call(
                "list_files",
                json!({"path":remote.path(),"recursive":true,"pattern":"*.rs"}),
            )
            .unwrap();
        assert_eq!(listing["entries"].as_array().unwrap().len(), 2);
        assert_eq!(listing["entries"][0]["name"], "src/main.rs");
        assert_eq!(listing["entries"][1]["name"], "src/nested/lib.rs");
    }
    server.join().unwrap();
}

#[test]
fn proxy_binary_speaks_mcp_stdio_without_connecting_for_discovery() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_remote-ops-proxy"))
        .args(["--timeout-ms", "100"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "protocolVersion":"2025-06-18", "capabilities":{},
                "clientInfo":{"name":"test","version":"1"}
            }
        })
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"remote_status","arguments":{}}})
    )
    .unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 25);
    assert_eq!(
        lines[2]["result"]["structuredContent"]["address"],
        "192.168.43.106:8022"
    );
    assert_eq!(lines[2]["result"]["structuredContent"]["connected"], false);
    assert!(output.stderr.is_empty());
}

#[test]
fn upload_rejection_before_chunks_remains_structured() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let jobs = JobManager::new();
        handle_connection(stream, 64, &jobs).unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("too-large.bin");
    let destination = directory.path().join("must-not-exist.bin");
    fs::write(&source, vec![0xabu8; 128]).unwrap();
    {
        let mut client = RemoteClient::new(
            address.to_string().parse().unwrap(),
            Duration::from_secs(5),
            DEFAULT_MAX_TRANSFER_BYTES,
        );
        let error = client
            .upload(
                &source.to_string_lossy(),
                &destination.to_string_lossy(),
                true,
            )
            .unwrap_err();
        assert_eq!(error.kind, "invalid_params");
        assert!(error.message.contains("transfer limit"));
        assert!(!destination.exists());
    }
    server.join().unwrap();
}

#[test]
fn agent_answers_internal_health_checks_without_exposing_an_mcp_tool() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let jobs = JobManager::new();
        handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &jobs).unwrap();
    });
    {
        let mut session = Session::connect(
            address,
            BUILTIN_PSK,
            SessionOptions::proxy(Duration::from_secs(2)),
            DEFAULT_MAX_CONTROL_BYTES,
        )
        .unwrap();
        session
            .send_json(
                FrameType::Request,
                1,
                &RemoteRequest {
                    operation: INTERNAL_PING_OPERATION.to_string(),
                    arguments: Value::Null,
                },
            )
            .unwrap();
        let response: RemoteResponse = session.receive_json(FrameType::Response, 1).unwrap();
        assert!(response.ok);
        assert_eq!(
            response.result.unwrap()["protocol_version"],
            PROTOCOL_VERSION
        );
    }
    server.join().unwrap();
}

#[test]
fn context_checked_patch_is_applied_over_the_remote_protocol() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let jobs = JobManager::new();
        handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &jobs).unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("remote.txt");
    let path_string = path.to_string_lossy().into_owned();
    fs::write(&path, "name = old\nenabled = true\n").unwrap();

    {
        let mut client = RemoteClient::new(
            address.to_string().parse().unwrap(),
            Duration::from_secs(5),
            DEFAULT_MAX_TRANSFER_BYTES,
        );
        let before = client
            .call("file_hash", json!({"path": path_string}))
            .unwrap()["digest"]
            .as_str()
            .unwrap()
            .to_string();
        let patch = format!(
            "*** Begin Patch\n*** Update File: {path_string}\n@@\n-name = old\n+name = new\n enabled = true\n*** End Patch"
        );
        let result = client
            .call(
                "apply_patch",
                json!({"path":path_string,"patch":patch,"expected_sha256":before}),
            )
            .unwrap();
        assert_eq!(result["hunks_applied"], 1);
        assert_eq!(result["sha256_before"], before);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "name = new\nenabled = true\n"
        );

        let stale = client
            .call(
                "apply_patch",
                json!({"path":path_string,"patch":patch,"expected_sha256":before}),
            )
            .unwrap_err();
        assert_eq!(stale.kind, "invalid_params");
        assert!(stale.message.contains("does not match"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "name = new\nenabled = true\n"
        );
    }
    server.join().unwrap();
}

#[test]
fn remote_can_switch_between_authenticated_endpoints() {
    fn responder(listener: TcpListener, expected_request_id: u64, marker: &'static str) {
        let (stream, _) = listener.accept().unwrap();
        let mut session = Session::accept(
            stream,
            BUILTIN_PSK,
            SessionOptions::agent(),
            DEFAULT_MAX_CONTROL_BYTES,
        )
        .unwrap();
        let request: RemoteRequest = session
            .receive_json(FrameType::Request, expected_request_id)
            .unwrap();
        assert_eq!(request.operation, "marker");
        session
            .send_json(
                FrameType::Response,
                expected_request_id,
                &RemoteResponse::success(json!({"marker": marker})),
            )
            .unwrap();
    }

    let first_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let first_address = first_listener.local_addr().unwrap();
    let first_remote: SocketAddrV4 = first_address.to_string().parse().unwrap();
    let first_server = thread::spawn(move || responder(first_listener, 1, "first"));
    let second_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let second_address = second_listener.local_addr().unwrap();
    let second_remote: SocketAddrV4 = second_address.to_string().parse().unwrap();
    let second_server = thread::spawn(move || responder(second_listener, 2, "second"));

    let mut client = RemoteClient::new(
        first_remote,
        Duration::from_secs(5),
        DEFAULT_MAX_TRANSFER_BYTES,
    );
    assert_eq!(client.remote_status()["connected"], false);
    assert_eq!(client.call("marker", json!({})).unwrap()["marker"], "first");
    assert_eq!(client.remote_status()["connected"], true);

    let unchanged = client
        .set_remote(Some(*first_remote.ip()), Some(first_remote.port()))
        .unwrap();
    assert_eq!(unchanged["connected"], true);

    let switched = client
        .set_remote(Some(*second_remote.ip()), Some(second_remote.port()))
        .unwrap();
    assert_eq!(switched["connected"], false);
    assert_eq!(switched["address"], second_address.to_string());
    assert_eq!(
        client.call("marker", json!({})).unwrap()["marker"],
        "second"
    );

    first_server.join().unwrap();
    second_server.join().unwrap();
}
