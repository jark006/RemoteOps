use remote_ops_protocol::execution::MAX_EXECUTION_PATH_BYTES;
use serde_json::{Value, json};

pub(crate) fn output_schema() -> Value {
    let path = json!({"type":"string","minLength":1,"maxLength":MAX_EXECUTION_PATH_BYTES});
    let names = json!([
        "sh",
        "bash",
        "busybox",
        "pwsh",
        "powershell",
        "cmd",
        "git_bash"
    ]);
    let platform = object(
        json!({"os":{"type":"string"},"arch":{"type":"string"},"family":{"type":"string"}}),
        &["os", "arch", "family"],
    );
    let item = |name: Value| object(json!({"name":name,"program":path}), &["name", "program"]);
    object(
        json!({
            "platform":platform,
            "cwd":{"type":["string","null"],"maxLength":MAX_EXECUTION_PATH_BYTES},
            "shells":object(json!({
                "default":{"type":["string","null"],"enum":[null,"sh","pwsh","powershell"]},
                "items":{"type":"array","maxItems":7,"items":item(json!({"type":"string","enum":names}))},
                "detection":{"type":"string","const":"file_metadata"},
                "truncated":{"type":"boolean"}
            }), &["default","items","detection","truncated"]),
            "init_system":object(json!({"name":{"type":["string","null"],"maxLength":256},"pid1_comm":{"type":["string","null"],"maxLength":256}}), &["name","pid1_comm"]),
            "utilities":object(json!({
                "available":{"type":"boolean"},
                "items":{"type":"array","maxItems":24,"items":item(json!({"type":"string"}))},
                "truncated":{"type":"boolean"}
            }), &["available","items","truncated"]),
            "search":object(json!({
                "path_directories_scanned":{"type":"integer","minimum":0,"maximum":128},
                "path_truncated":{"type":"boolean"},
                "ignored_relative_paths":{"type":"boolean"}
            }), &["path_directories_scanned","path_truncated","ignored_relative_paths"]),
            "execution":object(json!({
                "stdin":{"type":"string","const":"closed"},
                "output_encoding":{"type":"string","const":"utf-8-lossy"},
                "unix_mode":{"type":"boolean"},
                "release_deployment":{"type":"boolean"},
                "process_tree":{"type":"string","enum":["job_object","process_group"]}
            }), &["stdin","output_encoding","unix_mode","release_deployment","process_tree"])
        }),
        &[
            "platform",
            "cwd",
            "shells",
            "init_system",
            "utilities",
            "search",
            "execution",
        ],
    )
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
