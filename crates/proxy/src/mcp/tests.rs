use super::parameters::*;
use super::*;
use remote_ops_protocol::*;
fn output_schema(name: &str) -> Value {
    super::output::output_schema(super::catalog::Tool::parse(name).unwrap())
}

fn valid_arguments(tool: Tool) -> Value {
    match tool {
        Tool::ReadText
        | Tool::ReadFileLines
        | Tool::TailText
        | Tool::ListFiles
        | Tool::Stat
        | Tool::FileHash
        | Tool::Mkdir
        | Tool::Remove => json!({"path":"fixture.txt"}),
        Tool::WriteText => json!({"path":"fixture.txt","content":""}),
        Tool::ApplyPatch => {
            json!({"path":"fixture.txt","patch":"*** Begin Patch\n*** Update File: fixture.txt\n@@\n-old\n+new\n context\n*** End Patch"})
        }
        Tool::Grep => json!({"path":".","pattern":"match"}),
        Tool::Move | Tool::Copy => json!({"source":"source","destination":"destination"}),
        Tool::Chmod => json!({"path":"fixture.txt","mode":384}),
        Tool::Symlink => json!({"target":"target","link_path":"link"}),
        Tool::SyncDirectory => json!({"local_path":".","remote_path":"destination"}),
        Tool::DeployRelease => {
            json!({"local_path":".","releases_path":"releases","current_path":"current","release_id":"r1","start":{"program":"true"},"health":{"program":"true"}})
        }
        Tool::ProcessInfo | Tool::Kill => json!({"pid":1}),
        Tool::Pkill => json!({"name":"fixture"}),
        Tool::ShExec => json!({"command":""}),
        Tool::Exec | Tool::ProcessStart => json!({"program":"fixture"}),
        Tool::ProcessOutput | Tool::ProcessWait | Tool::ProcessSignal | Tool::ProcessClose => {
            json!({"job_id":1})
        }
        Tool::UploadFile | Tool::DownloadFile => {
            json!({"local_path":"local.bin","remote_path":"remote.bin"})
        }
        Tool::SetRemote => json!({"port":9999}),
        Tool::AgentUpdate => json!({"local_path":"candidate.bin"}),
        Tool::Batch => json!({"calls":[{"tool":"remote_status"}]}),
        Tool::Pids
        | Tool::SystemInfo
        | Tool::Environment
        | Tool::RemoteStatus
        | Tool::AgentInfo
        | Tool::RemoteProbe
        | Tool::WaitRemote
        | Tool::Reboot => json!({}),
    }
}

#[test]
fn every_registered_tool_has_strict_preflight_without_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut client = RemoteClient::new(
        listener.local_addr().unwrap().to_string().parse().unwrap(),
        std::time::Duration::from_millis(10),
        DEFAULT_MAX_TRANSFER_BYTES,
    );
    for &tool in Tool::ALL {
        let mut arguments = valid_arguments(tool);
        let definition = super::input::definition(tool);
        assert!(
            jsonschema::is_valid(&definition["inputSchema"], &arguments),
            "invalid test example for {}",
            tool.name()
        );
        if tool == Tool::Batch {
            assert!(super::batch::prepare(&arguments).is_ok());
        } else {
            assert!(
                super::dispatch::prepare(tool, arguments.clone()).is_ok(),
                "valid {} arguments must prepare",
                tool.name()
            );
        }
        arguments["unexpected_argument"] = json!(true);
        let response = call_tool(
            json!(1),
            Some(&json!({"name":tool.name(),"arguments":arguments})),
            &mut client,
        );
        assert_eq!(response["error"]["code"], -32602, "{}", tool.name());
    }
    assert_eq!(
        client.remote_status()["port"],
        listener.local_addr().unwrap().port()
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn batch_validates_all_arguments_before_any_command_can_connect() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut client = RemoteClient::new(
        listener.local_addr().unwrap().to_string().parse().unwrap(),
        std::time::Duration::from_millis(10),
        DEFAULT_MAX_TRANSFER_BYTES,
    );
    for (tool, arguments) in [
        ("read_text", json!({"path":"file","max_bytes":1048577})),
        ("tail_text", json!({"path":"file","lines":10001})),
        ("pids", json!({"limit":0})),
        ("stat", json!({"path":""})),
        ("exec", json!({"program":"fixture","env":{"A=B":"invalid"}})),
        (
            "exec",
            json!({"program":"must-not-run","args":["\u{1}".repeat(400_000)]}),
        ),
    ] {
        let response = call_tool(
            json!(1),
            Some(&json!({"name":"batch","arguments":{"calls":[
                {"tool":"exec","arguments":{"program":"a-command-that-must-not-run"}},
                {"tool":tool,"arguments":arguments}
            ]}})),
            &mut client,
        );
        assert_eq!(response["error"]["code"], -32602, "{response}");
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn basic_tools_reject_invalid_semantics_before_connecting() {
    let mut client = RemoteClient::new(
        "127.0.0.1:1".parse().unwrap(),
        std::time::Duration::from_millis(10),
        DEFAULT_MAX_TRANSFER_BYTES,
    );
    for (name, arguments) in [
        ("read_text", json!({"path":"\u{0}","max_bytes":1})),
        ("tail_text", json!({"path":"file","max_bytes":1048577})),
        ("write_text", json!({"path":"","content":"text"})),
        ("file_hash", json!({"path":"file","max_bytes":67108865})),
        ("pids", json!({"cursor":"0"})),
        ("process_info", json!({"pid":0})),
        ("kill", json!({"pid":1,"signal":0})),
        ("exec", json!({"program":"","timeout_ms":1})),
        ("exec", json!({"program":"fixture","args":["\u{0}"]})),
        ("exec", json!({"program":"fixture","cwd":""})),
        ("exec", json!({"program":"fixture","timeout_ms":300001})),
    ] {
        let response = call_tool(
            json!(1),
            Some(&json!({"name":name,"arguments":arguments})),
            &mut client,
        );
        assert_eq!(response["error"]["code"], -32602, "{name}: {response}");
    }
}

#[test]
fn shell_arguments_are_validated_before_connecting() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut client = RemoteClient::new(
        listener.local_addr().unwrap().to_string().parse().unwrap(),
        std::time::Duration::from_millis(10),
        DEFAULT_MAX_TRANSFER_BYTES,
    );
    for arguments in [
        json!({}),
        json!({"command":"x","shell":"auto"}),
        json!({"command":"x","shell":null}),
        json!({"command":"bad\u{0}text"}),
        json!({"command":"x","cwd":""}),
        json!({"command":"x","env":{"A=B":"bad"}}),
        json!({"command":"x","env":{"X":"x".repeat(remote_ops_protocol::execution::MAX_SHELL_ENV_BYTES)}}),
        json!({"command":"x","timeout_ms":300001}),
        json!({"command":"x".repeat(remote_ops_protocol::execution::MAX_SHELL_COMMAND_BYTES + 1)}),
        json!({"command":"x","extra":true}),
    ] {
        let result = call_tool(
            json!(1),
            Some(&json!({"name":"sh_exec","arguments":arguments})),
            &mut client,
        );
        assert_eq!(result["error"]["code"], -32602);
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let result = call_tool(
        json!(2),
        Some(&json!({"name":"environment","arguments":{"extra":true}})),
        &mut client,
    );
    assert_eq!(result["error"]["code"], -32602);
}

#[test]
fn response_modes_are_validated_before_mutation_or_batch_execution() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let mut client = RemoteClient::new(
        address.to_string().parse().unwrap(),
        std::time::Duration::from_millis(10),
        DEFAULT_MAX_TRANSFER_BYTES,
    );
    for mode in [
        Value::Null,
        json!(true),
        json!(1),
        json!({}),
        json!([]),
        json!("invalid"),
    ] {
        let response = call_tool(
            json!(1),
            Some(&json!({"name":"set_remote","arguments":{"port":9000,"response_mode":mode}})),
            &mut client,
        );
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(client.remote_status()["port"], address.port());
        let response = call_tool(
            json!(2),
            Some(&json!({"name":"batch","arguments":{"calls":[
                {"tool":"agent_info"},
                {"tool":"remote_status","arguments":{"response_mode":mode}}
            ]}})),
            &mut client,
        );
        assert_eq!(response["error"]["code"], -32602);
    }
    // No first batch child may execute before all presentation options have
    // been validated. In particular, invalid options must not connect.
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn command_output_schemas_require_duration_ms() {
    for name in ["sh_exec", "exec"] {
        let schema = output_schema(name);
        assert_eq!(
            schema["properties"]["duration_ms"],
            json!({"type":"integer","minimum":0})
        );
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("duration_ms")),
            "{name} must require duration_ms because every successful command result includes it"
        );
    }
}

#[test]
fn exposes_compatible_tools_plus_transfers() {
    let tools = tool_definitions();
    assert_eq!(tools.len(), 40);
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        tool_names()
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false)
    );
    assert!(tools.iter().all(|tool| {
        ["anyOf", "oneOf", "allOf"]
            .iter()
            .all(|keyword| tool["inputSchema"].get(keyword).is_none())
    }));
    let upload = tools
        .iter()
        .find(|tool| tool["name"] == "upload_file")
        .unwrap();
    assert_eq!(
        upload["inputSchema"]["properties"]["mode"]["maximum"],
        MAX_UNIX_MODE
    );
    assert_eq!(
        upload["inputSchema"]["properties"]["resume"]["default"],
        false
    );
    let sync = tools
        .iter()
        .find(|tool| tool["name"] == "sync_directory")
        .unwrap();
    assert_eq!(
        sync["inputSchema"]["properties"]["max_files"]["maximum"],
        MAX_SYNC_FILES
    );
    assert_eq!(sync["outputSchema"]["additionalProperties"], false);
    let deploy = tools
        .iter()
        .find(|tool| tool["name"] == "deploy_release")
        .unwrap();
    assert_eq!(deploy["annotations"]["openWorldHint"], true);
    assert_eq!(
        deploy["inputSchema"]["properties"]["start"]["additionalProperties"],
        false
    );
    let system = tools
        .iter()
        .find(|tool| tool["name"] == "system_info")
        .unwrap();
    assert_eq!(system["description"], "Read bounded system information");
    assert_eq!(
        system["outputSchema"]["properties"]["hostname"]["type"],
        "string"
    );
    assert!(
        system["outputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .all(|field| system["outputSchema"]["properties"]
                .get(field.as_str().unwrap())
                .is_some())
    );
    assert_eq!(
        system["outputSchema"]["properties"]["cpu"]["properties"]["byte_order"]["enum"],
        json!(["little", "big"])
    );
    assert_eq!(
        system["outputSchema"]["properties"]["network"]["properties"]["routes"]["properties"]["items"]
            ["maxItems"],
        256
    );
    assert_eq!(
        system["outputSchema"]["properties"]["filesystems"]["properties"]["mounts"]["maxItems"],
        256
    );
    assert_eq!(
        system["outputSchema"]["properties"]["toolchains"]["properties"]["items"]["maxItems"],
        24
    );
    let processes = tools.iter().find(|tool| tool["name"] == "pids").unwrap();
    assert_eq!(
        processes["description"],
        "List Linux, Windows, or macOS processes with bounded PID pagination. Follow next_cursor even for an empty filtered page. Command lines report availability/truncation; scan.filter_incomplete warns that hidden or clipped command lines may hide filter matches."
    );
    let process_info = tools
        .iter()
        .find(|tool| tool["name"] == "process_info")
        .unwrap();
    assert_eq!(
        process_info["outputSchema"]["properties"]["state"]["type"],
        json!(["string", "null"])
    );
    assert_eq!(
        process_info["outputSchema"]["properties"]["uid"]["type"],
        json!(["integer", "null"])
    );
    let kill = tools.iter().find(|tool| tool["name"] == "kill").unwrap();
    assert_eq!(
        kill["description"],
        "Send a Unix signal or terminate a Windows process"
    );
    assert_eq!(
        kill["inputSchema"]["properties"]["signal"]["description"],
        "Unix signal number; Windows accepts 9 or 15"
    );
    let pkill = tools.iter().find(|tool| tool["name"] == "pkill").unwrap();
    assert_eq!(
        pkill["description"],
        "Terminate Linux, Windows, or macOS processes whose platform name exactly matches"
    );
    assert_eq!(pkill["inputSchema"]["properties"]["name"]["maxLength"], 260);
    assert_eq!(
        pkill["inputSchema"]["properties"]["signal"]["description"],
        "Unix signal number; Windows accepts 9 or 15"
    );
    assert_eq!(pkill["annotations"]["destructiveHint"], true);
    let shell = tools.iter().find(|tool| tool["name"] == "sh_exec").unwrap();
    assert!(
        shell["description"]
            .as_str()
            .unwrap()
            .contains("environment")
    );
    assert_eq!(
        shell["inputSchema"]["properties"]["shell"]["default"],
        "native"
    );
    assert_eq!(
        shell["inputSchema"]["properties"]["shell"]["enum"],
        json!(remote_ops_protocol::ShellKind::ALL)
    );
    assert!(
        shell["outputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("shell"))
    );
    let process_start = tools
        .iter()
        .find(|tool| tool["name"] == "process_start")
        .unwrap();
    assert_eq!(
        process_start["inputSchema"]["properties"]["timeout_ms"]["maximum"],
        MAX_PROCESS_JOB_TIMEOUT_MS
    );
    assert_eq!(process_start["annotations"]["openWorldHint"], true);
    let process_output = tools
        .iter()
        .find(|tool| tool["name"] == "process_output")
        .unwrap();
    assert_eq!(
        process_output["inputSchema"]["properties"]["max_bytes"]["maximum"],
        MAX_PROCESS_OUTPUT_BYTES
    );
    assert_eq!(
        process_output["outputSchema"]["properties"]["state"]["enum"],
        json!(["running", "exited", "failed"])
    );
    assert_eq!(process_output["annotations"]["readOnlyHint"], true);
    let process_wait = tools
        .iter()
        .find(|tool| tool["name"] == "process_wait")
        .unwrap();
    assert_eq!(
        process_wait["inputSchema"]["properties"]["wait_ms"]["maximum"],
        MAX_PROCESS_WAIT_MS
    );
    let status = tools
        .iter()
        .find(|tool| tool["name"] == "remote_status")
        .unwrap();
    assert_eq!(status["annotations"]["readOnlyHint"], true);
    let setter = tools
        .iter()
        .find(|tool| tool["name"] == "set_remote")
        .unwrap();
    assert_eq!(setter["inputSchema"]["properties"]["ip"]["format"], "ipv4");
    assert_eq!(setter["inputSchema"]["required"], json!([]));
    assert!(
        setter["description"]
            .as_str()
            .unwrap()
            .contains("at least one must be provided")
    );
    assert_eq!(setter["annotations"]["destructiveHint"], true);
    let patch = tools
        .iter()
        .find(|tool| tool["name"] == "apply_patch")
        .unwrap();
    assert_eq!(
        patch["inputSchema"]["properties"]["patch"]["maxLength"],
        APPLY_PATCH_MAX_PATCH_BYTES
    );
    assert!(
        patch["inputSchema"]["properties"]["patch"]["description"]
            .as_str()
            .unwrap()
            .contains("*** Update File: PATH")
    );
    assert_eq!(patch["inputSchema"]["required"], json!(["path", "patch"]));
    assert_eq!(
        patch["outputSchema"]["properties"]["hunks_applied"]["maximum"],
        APPLY_PATCH_MAX_HUNKS
    );
    assert_eq!(patch["annotations"]["idempotentHint"], true);
    assert!(!tools.iter().any(|tool| tool["name"] == "ls"));
    let line_reader = tools
        .iter()
        .find(|tool| tool["name"] == "read_file_lines")
        .unwrap();
    assert_eq!(
        line_reader["inputSchema"]["properties"]["max_bytes"]["maximum"],
        READ_FILE_LINES_MAX_BYTES
    );
    let listing = tools
        .iter()
        .find(|tool| tool["name"] == "list_files")
        .unwrap();
    assert_eq!(
        listing["inputSchema"]["properties"]["recursive"]["default"],
        false
    );
    assert_eq!(
        listing["inputSchema"]["properties"]["max_depth"]["maximum"],
        LIST_FILES_MAX_DEPTH
    );
    let grep = tools.iter().find(|tool| tool["name"] == "grep").unwrap();
    assert_eq!(
        grep["inputSchema"]["properties"]["max_results"]["maximum"],
        GREP_MAX_RESULTS
    );
    assert_eq!(grep["annotations"]["readOnlyHint"], true);
}

#[test]
fn local_remote_tools_validate_and_update_without_connecting() {
    let mut client = RemoteClient::new(
        "127.0.0.1:8022".parse().unwrap(),
        std::time::Duration::from_millis(10),
        remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
    );
    let status = call_tool(
        json!(1),
        Some(&json!({"name":"remote_status","arguments":{}})),
        &mut client,
    );
    assert_eq!(status["result"]["structuredContent"]["connected"], false);

    let updated = call_tool(
        json!(2),
        Some(&json!({"name":"set_remote","arguments":{"port":9000}})),
        &mut client,
    );
    assert_eq!(
        updated["result"]["structuredContent"]["address"],
        "127.0.0.1:9000"
    );
    let updated = call_tool(
        json!(3),
        Some(&json!({"name":"set_remote","arguments":{"ip":"127.0.0.2"}})),
        &mut client,
    );
    assert_eq!(
        updated["result"]["structuredContent"]["address"],
        "127.0.0.2:9000"
    );

    for arguments in [json!({}), json!({"ip":"localhost"}), json!({"port":0})] {
        let response = call_tool(
            json!(4),
            Some(&json!({"name":"set_remote","arguments":arguments})),
            &mut client,
        );
        assert_eq!(response["error"]["code"], -32602);
    }
}

#[test]
fn pkill_arguments_are_validated_before_connecting() {
    let mut client = RemoteClient::new(
        "127.0.0.1:1".parse().unwrap(),
        std::time::Duration::from_millis(10),
        remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
    );
    for arguments in [
        json!({}),
        json!({"name":""}),
        json!({"name":"x".repeat(261)}),
        json!({"name":"has\u{0}nul"}),
        json!({"name":"valid","signal":0}),
        json!({"name":"valid","signal":65}),
        json!({"name":"valid","extra":true}),
    ] {
        let response = call_tool(
            json!(1),
            Some(&json!({"name":"pkill","arguments":arguments})),
            &mut client,
        );
        assert_eq!(response["error"]["code"], -32602);
    }
}

#[test]
fn background_process_arguments_are_validated_before_connecting() {
    let mut client = RemoteClient::new(
        "127.0.0.1:1".parse().unwrap(),
        std::time::Duration::from_millis(10),
        remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
    );
    let invalid_calls = [
        json!({"name":"process_start","arguments":{}}),
        json!({"name":"process_start","arguments":{"program":""}}),
        json!({"name":"process_start","arguments":{"program":"has\u{0}nul"}}),
        json!({"name":"process_start","arguments":{"program":"ok","args":["has\u{0}nul"]}}),
        json!({"name":"process_start","arguments":{"program":"ok","cwd":""}}),
        json!({"name":"process_start","arguments":{"program":"ok","env":{"":"value"}}}),
        json!({"name":"process_start","arguments":{"program":"ok","env":{"A":"has\u{0}nul"}}}),
        json!({"name":"process_start","arguments":{"program":"ok","timeout_ms":0}}),
        json!({"name":"process_start","arguments":{"program":"ok","timeout_ms":MAX_PROCESS_JOB_TIMEOUT_MS + 1}}),
        json!({"name":"process_start","arguments":{"program":"ok","extra":true}}),
        json!({"name":"process_output","arguments":{"job_id":0}}),
        json!({"name":"process_output","arguments":{"job_id":1,"max_bytes":MAX_PROCESS_OUTPUT_BYTES + 1}}),
        json!({"name":"process_output","arguments":{"job_id":1,"extra":true}}),
        json!({"name":"process_wait","arguments":{"job_id":0}}),
        json!({"name":"process_wait","arguments":{"job_id":1,"wait_ms":MAX_PROCESS_WAIT_MS + 1}}),
        json!({"name":"process_signal","arguments":{"job_id":0}}),
        json!({"name":"process_signal","arguments":{"job_id":1,"signal":0}}),
        json!({"name":"process_signal","arguments":{"job_id":1,"signal":65}}),
        json!({"name":"process_close","arguments":{"job_id":0}}),
    ];
    for params in invalid_calls {
        let response = call_tool(json!(1), Some(&params), &mut client);
        assert_eq!(response["error"]["code"], -32602, "{params}");
    }
}

#[test]
fn apply_patch_arguments_are_validated_before_connecting() {
    let mut client = RemoteClient::new(
        "127.0.0.1:1".parse().unwrap(),
        std::time::Duration::from_millis(10),
        remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
    );
    for arguments in [
        json!({}),
        json!({"path":"","patch":"x"}),
        json!({"path":"has\u{0}nul","patch":"x"}),
        json!({"path":"file.txt","patch":""}),
        json!({"path":"file.txt","patch":"invalid"}),
        json!({"path":"file.txt","patch":"*** Begin Patch\n*** Update File: other.txt\n@@\n-old\n+new\n*** End Patch"}),
        json!({"path":"file.txt","patch":"x".repeat(APPLY_PATCH_MAX_PATCH_BYTES + 1)}),
        json!({"path":"file.txt","patch":"x","expected_sha256":"invalid"}),
        json!({"path":"file.txt","patch":"x","extra":true}),
    ] {
        let response = call_tool(
            json!(1),
            Some(&json!({"name":"apply_patch","arguments":arguments})),
            &mut client,
        );
        assert_eq!(response["error"]["code"], -32602);
    }
}

#[test]
fn file_discovery_arguments_are_validated_before_connecting() {
    let mut client = RemoteClient::new(
        "127.0.0.1:1".parse().unwrap(),
        std::time::Duration::from_millis(10),
        remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
    );
    let invalid_calls = [
        json!({"name":"read_file_lines","arguments":{"path":"","start_line":1}}),
        json!({"name":"read_file_lines","arguments":{"path":"file","start_line":0}}),
        json!({"name":"read_file_lines","arguments":{"path":"file","start_line":2,"end_line":1}}),
        json!({"name":"read_file_lines","arguments":{"path":"file","max_bytes":READ_FILE_LINES_MAX_BYTES + 1}}),
        json!({"name":"grep","arguments":{"path":".","pattern":""}}),
        json!({"name":"grep","arguments":{"path":".","pattern":"["}}),
        json!({"name":"grep","arguments":{"path":".","pattern":"x","glob":"["}}),
        json!({"name":"grep","arguments":{"path":".","pattern":"x","max_results":0}}),
        json!({"name":"list_files","arguments":{"path":"","recursive":true}}),
        json!({"name":"list_files","arguments":{"path":".","limit":0}}),
        json!({"name":"list_files","arguments":{"path":".","pattern":"["}}),
        json!({"name":"list_files","arguments":{"path":".","max_depth":0}}),
        json!({"name":"ls","arguments":{"path":"."}}),
    ];
    for params in invalid_calls {
        let response = call_tool(
            json!(1),
            Some(&json!({"name":params["name"],"arguments":params["arguments"]})),
            &mut client,
        );
        assert_eq!(response["error"]["code"], -32602, "{params}");
    }
}

#[test]
fn deployment_arguments_are_validated_before_connecting() {
    let mut client = RemoteClient::new(
        "127.0.0.1:1".parse().unwrap(),
        std::time::Duration::from_millis(10),
        remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES,
    );
    let invalid_calls = [
        json!({"name":"upload_file","arguments":{"local_path":"file","remote_path":"remote","mode":4096}}),
        json!({"name":"download_file","arguments":{"local_path":"","remote_path":"remote"}}),
        json!({"name":"mkdir","arguments":{"path":"","mode":0}}),
        json!({"name":"remove","arguments":{"path":""}}),
        json!({"name":"move","arguments":{"source":"same","destination":"same"}}),
        json!({"name":"copy","arguments":{"source":"same","destination":"same"}}),
        json!({"name":"chmod","arguments":{"path":"file","mode":4096}}),
        json!({"name":"symlink","arguments":{"target":"a","link_path":"b","target_kind":"other"}}),
        json!({"name":"sync_directory","arguments":{"local_path":".","remote_path":"remote","max_files":0}}),
        json!({"name":"sync_directory","arguments":{"local_path":".","remote_path":"remote","excludes":["["]}}),
        json!({"name":"deploy_release","arguments":{
            "local_path":".","releases_path":"releases","current_path":"current",
            "release_id":"../escape","start":{"program":"true"},"health":{"program":"true"}
        }}),
        json!({"name":"deploy_release","arguments":{
            "local_path":".","releases_path":"releases","current_path":"current",
            "release_id":"v1","start":{"program":"","timeout_ms":1},"health":{"program":"true"}
        }}),
    ];
    for params in invalid_calls {
        let response = call_tool(json!(1), Some(&params), &mut client);
        assert_eq!(response["error"]["code"], -32602, "{params}");
    }
}
