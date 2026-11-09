use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;

use remote_ops_agent::{service::handle_connection, tools::jobs::JobManager};
use remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES;
use remote_ops_proxy::mcp::tool_definitions;
use serde_json::{Value, json};

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

fn run_proxy(args: &[String], requests: Vec<Value>) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_remote-ops-proxy"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain stdout while sending: a full tool catalog can exceed the pipe
    // capacity, otherwise the sender and proxy could block each other.
    let mut stdin = child.stdin.take().unwrap();
    let writer = thread::spawn(move || {
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn validate(name: &str, data: &Value) {
    let tool = tool_definitions()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .unwrap();
    let validator = jsonschema::validator_for(&tool["outputSchema"]).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(data)
        .map(|error| error.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{name} output violates advertised schema: {errors:?}"
    );
}

#[test]
fn compact_and_full_results_match_schemas_over_stdio_and_wire() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let jobs = JobManager::new();
        // A complete remote failure is synchronized; later calls reuse it.
        let (stream, _) = listener.accept().unwrap();
        handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &jobs).unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let sample = root.path().join("sample.txt");
    std::fs::write(&sample, "精确正文\r\nno final newline").unwrap();
    let requests = vec![
        call(0, "remote_probe", json!({})),
        call(1, "agent_info", json!({})),
        call(2, "agent_info", json!({"response_mode":"full"})),
        call(3, "system_info", json!({})),
        call(4, "system_info", json!({"response_mode":"full"})),
        call(5, "remote_status", json!({})),
        call(6, "remote_status", json!({"response_mode":"full"})),
        call(
            7,
            "batch",
            json!({"calls":[
                {"tool":"agent_info"},
                {"tool":"agent_info","arguments":{"response_mode":"full"}},
                {"tool":"read_text","arguments":{"path":sample}},
                {"tool":"stat","arguments":{"path":root.path().join("missing")}}
            ]}),
        ),
        call(
            8,
            "batch",
            json!({"response_mode":"full","calls":[
                {"tool":"agent_info"},
                {"tool":"agent_info","arguments":{"response_mode":"compact"}}
            ]}),
        ),
        call(9, "set_remote", json!({"port":address.port()})),
        call(10, "remote_status", json!({"response_mode":"full"})),
    ];
    let names = requests
        .iter()
        .map(|request| request["params"]["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    let responses = run_proxy(&["--remote".to_string(), address.to_string()], requests);
    server.join().unwrap();
    assert_eq!(responses.len(), names.len());
    for (response, name) in responses.iter().zip(&names) {
        assert_eq!(response["result"]["isError"], false, "{response}");
        let data = &response["result"]["structuredContent"];
        validate(name, data);
        let fallback: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(&fallback, data);
    }
    let data = |index: usize| &responses[index]["result"]["structuredContent"];
    assert_eq!(data(0)["reachable"], true);
    assert!(data(0)["agent_info"].get("build").is_none());
    for (compact_index, full_index) in [(1, 2), (3, 4), (5, 6)] {
        assert!(data(compact_index).get("_response").is_some());
        assert!(data(full_index).get("_response").is_none());
        assert!(
            responses[compact_index].to_string().len() < responses[full_index].to_string().len()
        );
    }
    assert!(data(3).get("network").is_none());
    assert!(data(4).get("network").is_some());
    assert!(data(5)["agent_info"].get("build").is_none());
    assert!(data(6)["agent_info"].get("build").is_some());
    assert!(data(6)["last_probe"]["agent_info"].get("build").is_some());
    let results = &data(7)["results"];
    validate("agent_info", &results[0]["result"]);
    validate("agent_info", &results[1]["result"]);
    assert!(results[0]["result"].get("build").is_none());
    assert!(results[1]["result"].get("build").is_some());
    assert_eq!(results[2]["result"]["text"], "精确正文\r\nno final newline");
    assert_eq!(results[2]["result"]["metadata"]["truncated"], false);
    assert_eq!(results[3]["ok"], false);
    assert!(results[3]["error"]["kind"].is_string());
    assert!(data(8)["results"][0]["result"].get("build").is_some());
    assert!(data(8)["results"][1]["result"].get("build").is_none());
    assert_eq!(data(9)["connected"], false);
    // Presentation cannot mutate the client's complete cached observations.
    assert!(data(10)["agent_info"].get("build").is_some());
    assert_eq!(
        data(1)["runtime"]["instance_id"],
        data(10)["agent_info"]["runtime"]["instance_id"]
    );

    let compact_bytes = responses[3].to_string().len();
    let full_bytes = responses[4].to_string().len();
    eprintln!("system_info MCP response: compact={compact_bytes} bytes, full={full_bytes} bytes");
}

#[test]
fn cli_default_and_per_call_override_are_advertised_and_applied() {
    let responses = run_proxy(
        &["--response-mode".to_string(), "full".to_string()],
        vec![
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            call(2, "remote_status", json!({})),
            call(3, "remote_status", json!({"response_mode":"compact"})),
        ],
    );
    for tool in responses[0]["result"]["tools"].as_array().unwrap() {
        assert_eq!(
            tool["inputSchema"]["properties"]["response_mode"]["default"],
            "full"
        );
    }
    let full = responses[1]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let compact = responses[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(full.contains('\n'));
    assert!(!compact.contains('\n'));
    assert_eq!(
        responses[1]["result"]["structuredContent"],
        responses[2]["result"]["structuredContent"]
    );
}

#[test]
fn schemas_compile_and_partial_results_require_an_omission_report() {
    for tool in tool_definitions() {
        jsonschema::validator_for(&tool["inputSchema"]).unwrap();
        jsonschema::validator_for(&tool["outputSchema"]).unwrap();
        assert_eq!(
            tool["inputSchema"]["properties"]["response_mode"]["default"],
            "compact"
        );
    }
    let info = remote_ops_agent::tools::lifecycle::agent_info(DEFAULT_MAX_TRANSFER_BYTES);
    let tool = tool_definitions()
        .into_iter()
        .find(|tool| tool["name"] == "agent_info")
        .unwrap();
    let validator = jsonschema::validator_for(&tool["outputSchema"]).unwrap();
    assert!(validator.is_valid(&info));
    let mut broken = info;
    broken.as_object_mut().unwrap().remove("build");
    assert!(!validator.is_valid(&broken));
}
