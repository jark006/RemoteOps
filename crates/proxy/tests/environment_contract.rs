use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;

use remote_ops_agent::{service::handle_connection, tools::jobs::JobManager};
use remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES;
use remote_ops_proxy::mcp::tool_definitions;
use serde_json::{Value, json};

struct McpProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpProcess {
    fn new(address: String) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_remote-ops-proxy"))
            .args(["--remote", &address])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            stdin: child.stdin.take(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child: Some(child),
            next_id: 1,
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(self.stdin.as_mut().unwrap(),"{}",json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).unwrap();
        let mut line = String::new();
        assert!(self.stdout.read_line(&mut line).unwrap() > 0);
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        assert_eq!(response["result"]["isError"], false, "{response}");
        let data = response["result"]["structuredContent"].clone();
        let schema = tool_definitions()
            .into_iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        let validator = jsonschema::validator_for(&schema["outputSchema"]).unwrap();
        let errors: Vec<_> = validator
            .iter_errors(&data)
            .map(|error| error.to_string())
            .collect();
        assert!(errors.is_empty(), "{name} schema errors: {errors:?}");
        assert_eq!(
            serde_json::from_str::<Value>(
                response["result"]["content"][0]["text"].as_str().unwrap()
            )
            .unwrap(),
            data
        );
        data
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        let output = self.child.take().unwrap().wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            drop(self.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Live Unix verification is opt-in, with an explicitly designated workspace.
/// It uses the real PC proxy binary, so MCP framing and local transfer paths
/// are exercised across hosts as well as the remote protocol.
#[test]
#[ignore = "requires REMOTE_OPS_TEST_ADDR and REMOTE_OPS_TEST_ROOT for a live Unix Agent"]
fn live_unix_agent_round_trip() {
    use sha2::{Digest, Sha256};
    use std::time::{SystemTime, UNIX_EPOCH};

    let address = std::env::var("REMOTE_OPS_TEST_ADDR").expect("explicit test address");
    let base = std::env::var("REMOTE_OPS_TEST_ROOT").expect("explicit test workspace");
    assert!(
        base.starts_with('/'),
        "the designated remote workspace must be absolute"
    );
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = format!(
        "{}/wire-{}-{stamp}",
        base.trim_end_matches('/'),
        std::process::id()
    );
    let mut proxy = McpProcess::new(address);
    let environment = proxy.call("environment", json!({}));
    assert_eq!(environment["platform"]["family"], "unix");
    assert_eq!(environment["shells"]["default"], "sh");
    let info = proxy.call("agent_info", json!({"response_mode":"full"}));
    let agent_pid = info["runtime"]["pid"].as_u64().unwrap();
    let detail = proxy.call("process_info", json!({"pid":agent_pid}));
    assert_eq!(detail["cmdline_available"], true);
    assert_eq!(detail["cmdline_truncated"], false);
    let page = proxy.call(
        "pids",
        json!({"cursor":(agent_pid-1).to_string(),"limit":1}),
    );
    assert_eq!(page["processes"][0]["pid"], agent_pid);
    assert!(page["scan"]["processes_scanned"].as_u64().unwrap() >= 1);
    assert_eq!(
        proxy.call("remote_probe", json!({"timeout_ms":1000}))["reachable"],
        true
    );
    assert_eq!(
        info["protocol_version"],
        remote_ops_protocol::PROTOCOL_VERSION
    );
    if environment["platform"]["os"] == "macos" {
        assert!(
            !info["supported_operations"]
                .as_array()
                .unwrap()
                .contains(&json!("reboot"))
        );
        assert_eq!(environment["init_system"]["name"], "launchd");
    }
    proxy.call("mkdir", json!({"path":root,"mode":448}));
    let shell = proxy.call("sh_exec",json!({"command":"printf '%s\\n' \"$REMOTE_OPS_LIVE_VALUE\"; pwd; exit 7","cwd":root,"env":{"REMOTE_OPS_LIVE_VALUE":"跨主机 'quote' $ &"}}));
    assert_eq!(shell["shell"]["name"], "sh");
    assert_eq!(shell["exit_code"], 7);
    assert!(
        shell["stdout"]
            .as_str()
            .unwrap()
            .starts_with("跨主机 'quote' $ &\n")
    );
    let text_path = format!("{root}/config.txt");
    let before = "enabled=false\n名称=设备\n";
    proxy.call("write_text", json!({"path":text_path,"content":before}));
    let patch = format!(
        "*** Begin Patch\n*** Update File: {text_path}\n@@\n-enabled=false\n+enabled=true\n 名称=设备\n*** End Patch"
    );
    let expected = format!("{:x}", Sha256::digest(before.as_bytes()));
    proxy.call(
        "apply_patch",
        json!({"path":text_path,"patch":patch,"expected_sha256":expected}),
    );
    let read = proxy.call("batch",json!({"calls":[{"tool":"read_file_lines","arguments":{"path":text_path}},{"tool":"environment"}]}));
    assert_eq!(
        read["results"][0]["result"]["text"],
        "enabled=true\n名称=设备\n"
    );
    assert_eq!(read["results"][1]["ok"], true);
    let local = tempfile::tempdir().unwrap();
    let upload = local.path().join("payload.bin");
    let download = local.path().join("roundtrip.bin");
    let bytes: Vec<u8> = (0..180_000).map(|index| (index % 239) as u8).collect();
    std::fs::write(&upload, &bytes).unwrap();
    let remote_file = format!("{root}/payload.bin");
    proxy.call(
        "upload_file",
        json!({"local_path":upload,"remote_path":remote_file,"overwrite":false,"mode":384}),
    );
    proxy.call(
        "download_file",
        json!({"remote_path":remote_file,"local_path":download,"overwrite":false}),
    );
    assert_eq!(std::fs::read(&download).unwrap(), bytes);
    let failures = proxy.call(
        "batch",
        json!({"calls":[
            {"tool":"read_text","arguments":{"path":root}},
            {"tool":"environment"}
        ]}),
    );
    assert_eq!(failures["results"][0]["ok"], false);
    assert_eq!(failures["results"][0]["error"]["source"], "remote");
    assert_eq!(failures["results"][0]["error"]["outcome"], "remote_error");
    assert_eq!(failures["results"][1]["ok"], true);
    assert_eq!(
        proxy.call("remote_status", json!({}))["connection_state"],
        "cached"
    );
    proxy.call("remove", json!({"path":root,"recursive":true}));
    eprintln!("verified live Unix Agent: {}", environment["platform"]);
    proxy.finish();
}

#[test]
fn discovered_shells_execute_with_explicit_language_cwd_env_and_schema() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        handle_connection(stream, DEFAULT_MAX_TRANSFER_BYTES, &JobManager::new()).unwrap();
    });
    let mut proxy = McpProcess::new(address.to_string());
    let environment = proxy.call("environment", json!({}));
    assert_eq!(environment["platform"]["os"], std::env::consts::OS);
    assert_eq!(environment["shells"]["detection"], "file_metadata");
    assert!(environment.to_string().len() < 64 * 1024);
    let shells = environment["shells"]["items"].as_array().unwrap();
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("工作 目录 '引号");
    std::fs::create_dir(&cwd).unwrap();
    let default = environment["shells"]["default"]
        .as_str()
        .expect("test host needs its native shell");
    for selected in
        std::iter::once("native").chain(shells.iter().map(|item| item["name"].as_str().unwrap()))
    {
        let name = if selected == "native" {
            default
        } else {
            selected
        };
        let script = match name {
            "cmd" => "echo %REMOTE_OPS_SHELL_VALUE% & cd & echo stderr-text 1>&2 & exit /b 7",
            "powershell" | "pwsh" => {
                "[Console]::WriteLine($env:REMOTE_OPS_SHELL_VALUE); [Console]::WriteLine((Get-Location).Path); [Console]::Error.WriteLine('stderr-text'); exit 7"
            }
            _ => {
                r#"printf '%s\n' "$REMOTE_OPS_SHELL_VALUE"; pwd; printf 'stderr-text\n' >&2; exit 7"#
            }
        };
        let expected_value = if name == "cmd" {
            "plain-value"
        } else {
            "literal 'quote' \"double\" $ &"
        };
        let result = proxy.call("sh_exec",json!({"shell":selected,"command":script,"cwd":cwd,"env":{"REMOTE_OPS_SHELL_VALUE":expected_value},"timeout_ms":15_000}));
        assert_eq!(result["shell"]["name"], name);
        let detected = shells.iter().find(|item| item["name"] == name).unwrap();
        assert_eq!(result["shell"]["program"], detected["program"]);
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains(expected_value), "{name}: {result}");
        // cmd emits the current Windows code page; only its ASCII result is
        // asserted here. PowerShell and POSIX shell scripts exercise UTF-8.
        if name != "cmd" {
            assert!(stdout.contains("工作 目录 '引号"), "{name}: {result}");
        }
        assert!(
            result["stderr"].as_str().unwrap().contains("stderr-text"),
            "{name}: {result}"
        );
        assert_eq!(result["exit_code"], 7, "{name}: {result}");
        assert_eq!(result["timed_out"], false);
        assert_eq!(result["stdout_truncated"], false);
        eprintln!("verified shell {name}: {}", result["shell"]["program"]);

        if matches!(name, "powershell" | "pwsh") {
            let script = format!(
                "# {}\n[Console]::Write('长脚本'); exit 0",
                "界".repeat(40_000)
            );
            let long = proxy.call(
                "sh_exec",
                json!({"shell":name,"command":script,"timeout_ms":15_000}),
            );
            assert_eq!(long["stdout"], "长脚本");
            assert_eq!(long["exit_code"], 0);
            let failure = proxy.call(
                "sh_exec",
                json!({"shell":name,"command":"throw 'expected failure'","timeout_ms":15_000}),
            );
            assert_ne!(failure["exit_code"], 0);
            assert!(
                failure["stderr"]
                    .as_str()
                    .unwrap()
                    .contains("expected failure")
            );
            assert!(!failure["stderr"].as_str().unwrap().contains("#< CLIXML"));
        }
        if matches!(name, "bash" | "git_bash") {
            let hook = root.path().join("startup.sh");
            std::fs::write(&hook, "printf 'unexpected-startup-hook'").unwrap();
            let no_hook = proxy.call(
                "sh_exec",
                json!({"shell":name,"command":"printf ok","env":{"BASH_ENV":hook,"ENV":hook}}),
            );
            assert_eq!(no_hook["stdout"], "ok");
        }
        if matches!(name, "sh" | "bash" | "git_bash" | "busybox") {
            let script = format!("# {}\nprintf '长脚本'; exit 0", "界".repeat(40_000));
            let long = proxy.call(
                "sh_exec",
                json!({"shell":name,"command":script,"timeout_ms":15_000}),
            );
            assert_eq!(long["stdout"], "长脚本");
            assert_eq!(long["exit_code"], 0);
        }
    }
    let batched = proxy.call("batch", json!({"calls":[{"tool":"environment"}]}));
    assert_eq!(batched["results"][0]["ok"], true);
    let info = proxy.call("agent_info", json!({"response_mode":"full"}));
    let operations = info["supported_operations"].as_array().unwrap();
    assert!(operations.contains(&json!("environment")));
    assert_eq!(operations.contains(&json!("chmod")), cfg!(unix));
    assert_eq!(operations.contains(&json!("deploy_activate")), cfg!(unix));
    assert_eq!(
        operations.contains(&json!("reboot")),
        cfg!(any(target_os = "linux", windows))
    );
    assert_eq!(info["capabilities"]["unix_mode"], cfg!(unix));
    proxy.finish();
    server.join().unwrap();
}
