use remote_ops_agent::{service::handle_connection, tools::jobs::JobManager};
use remote_ops_protocol::{DEFAULT_MAX_CONTROL_BYTES, DEFAULT_MAX_TRANSFER_BYTES};
use remote_ops_proxy::{client::RemoteClient, mcp::handle_message};
use serde_json::{Value, json};
use std::{fs, io::Write, net::TcpListener, thread, time::Duration};

fn client(address: std::net::SocketAddr) -> RemoteClient {
    RemoteClient::new(
        address.to_string().parse().unwrap(),
        Duration::from_secs(10),
        DEFAULT_MAX_TRANSFER_BYTES,
    )
}

#[test]
fn remote_errors_are_structured_and_keep_a_synchronized_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &JobManager::new()).unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let mut client = client(address);
    // Proxy accepts the parameters; actual filesystem state is only known on
    // the Agent. This must be an MCP tool error with a remote source.
    let response=handle_message(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_text","arguments":{"path":root.path()}}}),&mut client).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], true);
    let error: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(error["error"]["source"], "remote");
    assert_eq!(error["error"]["outcome"], "remote_error");
    assert_eq!(error["error"]["kind"], "invalid_params");
    assert_eq!(error["error"]["request_started"], true);
    assert_eq!(client.remote_status()["connection_state"], "cached");
    assert_eq!(client.remote_status()["last_error"], error["error"]);
    assert!(client.remote_status()["last_success_at_ms"].is_null());
    client.call("agent_info", json!({})).unwrap();
    for (name, args) in [
        ("process_info", json!({"pid":std::process::id()})),
        ("pids", json!({"limit":8})),
    ] {
        let result = client.call(name, args).unwrap();
        let tool = remote_ops_proxy::mcp::tool_definitions()
            .into_iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        let errors: Vec<_> = jsonschema::validator_for(&tool["outputSchema"])
            .unwrap()
            .iter_errors(&result)
            .map(|error| error.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
    assert!(client.remote_status()["last_error"].is_null());
    assert!(client.remote_status()["last_success_at_ms"].is_u64());
    drop(client);
    server.join().unwrap();
}

#[test]
fn encoding_limits_preserve_continuation_and_drain_command_pipes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &JobManager::new()).unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("controls.txt");
    let bytes = vec![0u8; 1024 * 1024];
    fs::write(&path, &bytes).unwrap();
    let mut client = client(address);
    let mut offset = 0;
    while offset < bytes.len() {
        let result = client
            .call("read_text", json!({"path":path,"offset":offset}))
            .unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() < DEFAULT_MAX_CONTROL_BYTES);
        let text = result["text"].as_str().unwrap();
        assert!(!text.is_empty());
        assert!(text.bytes().all(|byte| byte == 0));
        offset += text.len();
        assert_eq!(result["metadata"]["next_offset"], offset);
        assert_eq!(result["metadata"]["truncated"], offset < bytes.len());
    }
    let tail = client
        .call(
            "tail_text",
            json!({"path":path,"lines":1,"max_bytes":1048576}),
        )
        .unwrap();
    assert_eq!(tail["metadata"]["truncated"], true);
    assert!(serde_json::to_vec(&tail).unwrap().len() < DEFAULT_MAX_CONTROL_BYTES);
    fs::write(&path, b"one\r\ntwo\r\n").unwrap();
    let tail = client
        .call("tail_text", json!({"path":path,"lines":1}))
        .unwrap();
    assert_eq!(tail["text"], "two\r\n");
    assert_eq!(tail["metadata"]["lines_returned"], 1);
    let controls = [vec![b'\0'; 998], vec![b'\r', b'\n']].concat();
    fs::write(&path, controls.repeat(900)).unwrap();
    let mut next_line = 1;
    let mut returned = 0;
    loop {
        let result = client
            .call(
                "read_file_lines",
                json!({"path":path,"start_line":next_line,"end_line":900,"max_bytes":1048576}),
            )
            .unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() < DEFAULT_MAX_CONTROL_BYTES);
        let count = result["metadata"]["lines_returned"].as_u64().unwrap();
        assert!(count > 0);
        returned += count;
        if let Some(line) = result["metadata"]["next_line"].as_u64() {
            next_line = line;
        } else {
            break;
        }
    }
    assert_eq!(returned, 900);

    let program = std::env::current_exe().unwrap();
    let args = json!({"program":program,"args":["--exact","control_output_helper","--nocapture"],"env":{"REMOTE_OPS_CONTROL_OUTPUT_HELPER":"1"},"timeout_ms":10000});
    let command = client.call("exec", args.clone()).unwrap();
    assert_eq!(command["exit_code"], 0);
    assert_eq!(command["timed_out"], false);
    assert_eq!(command["stdout_truncated"], true);
    assert_eq!(command["stderr_truncated"], true);
    assert!(command["stdout"].as_str().unwrap().contains('\0'));
    assert!(serde_json::to_vec(&command).unwrap().len() < DEFAULT_MAX_CONTROL_BYTES);

    let started = client.call("process_start", args).unwrap();
    let job_id = started["job_id"].as_u64().unwrap();
    client
        .call("process_wait", json!({"job_id":job_id,"wait_ms":10000}))
        .unwrap();
    let mut stdout_cursor = 0;
    let mut stderr_cursor = 0;
    let mut stderr_bytes = 0;
    for iteration in 0..8 {
        let result=client.call("process_output",json!({"job_id":job_id,"stdout_cursor":stdout_cursor,"stderr_cursor":stderr_cursor,"max_bytes":262144})).unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() < DEFAULT_MAX_CONTROL_BYTES);
        let stdout = result["stdout"].as_str().unwrap();
        let stderr = result["stderr"].as_str().unwrap();
        if stdout.is_empty() && stderr.is_empty() {
            break;
        }
        assert!(iteration < 7, "output cursor did not make progress");
        stdout_cursor = result["next_stdout_cursor"].as_u64().unwrap();
        stderr_cursor = result["next_stderr_cursor"].as_u64().unwrap();
        stderr_bytes += stderr.len();
    }
    assert_eq!(
        stderr_bytes, 262144,
        "encoded output paging must not drop retained bytes"
    );
    client
        .call("process_close", json!({"job_id":job_id}))
        .unwrap();
    client.call("agent_info", json!({})).unwrap();
    drop(client);
    server.join().unwrap();
}

#[test]
fn control_output_helper() {
    if std::env::var_os("REMOTE_OPS_CONTROL_OUTPUT_HELPER").is_none() {
        return;
    }
    let bytes = vec![0u8; 256 * 1024];
    std::io::stdout().write_all(&bytes).unwrap();
    std::io::stderr().write_all(&bytes).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn long_process_command_lines_are_bounded_and_report_their_prefix() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &JobManager::new()).unwrap();
    });
    let mut client = client(address);
    let mut args = vec![
        "-c".to_string(),
        "sleep 30".to_string(),
        "remote-ops-long-command-line-fixture".to_string(),
    ];
    args.extend(std::iter::repeat_n("value".repeat(600), 64));
    let started = client
        .call(
            "process_start",
            json!({"program":"/bin/sh","args":args,"timeout_ms":60000}),
        )
        .unwrap();
    let pid = started["pid"].as_u64().unwrap();
    let job = started["job_id"].as_u64().unwrap();
    let info = client.call("process_info", json!({"pid":pid})).unwrap();
    assert_eq!(info["cmdline_available"], true);
    assert_eq!(info["cmdline_truncated"], true);
    assert!(info["cmdline"].as_str().unwrap().len() <= 131072);
    let page=client.call("pids",json!({"cursor":(pid-1).to_string(),"limit":1,"filter":"remote-ops-long-command-line-fixture"})).unwrap();
    assert_eq!(page["processes"][0]["pid"], pid);
    assert_eq!(page["processes"][0]["cmdline_truncated"], true);
    client
        .call("process_signal", json!({"job_id":job}))
        .unwrap();
    client
        .call("process_wait", json!({"job_id":job,"wait_ms":5000}))
        .unwrap();
    client.call("process_close", json!({"job_id":job})).unwrap();
    drop(client);
    server.join().unwrap();
}

#[test]
fn failed_connection_cleans_nonresumable_download_partial() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut client = client(address);
    let target = root.path().join("output.bin");
    let partial = root.path().join(".remoteops-download-output.bin.part");
    let error = client
        .download("remote.bin", target.to_str().unwrap(), false, false)
        .unwrap_err();
    assert_eq!(error.details()["outcome"], "not_sent");
    assert!(!target.exists());
    assert!(!partial.exists());
    client
        .download("remote.bin", target.to_str().unwrap(), false, true)
        .unwrap_err();
    assert!(partial.is_file());
}

#[cfg(unix)]
#[test]
fn rejected_download_partial_symlink_is_left_intact() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    let partial = root.path().join(".remoteops-download-output.bin.part");
    fs::write(&original, b"must-keep").unwrap();
    symlink(&original, &partial).unwrap();
    let mut client = client("127.0.0.1:1".parse().unwrap());
    let target = root.path().join("output.bin");
    let error = client
        .download("remote.bin", target.to_str().unwrap(), false, false)
        .unwrap_err();
    assert_eq!(error.kind, "invalid_params");
    assert!(
        fs::symlink_metadata(&partial)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&original).unwrap(), b"must-keep");
}
