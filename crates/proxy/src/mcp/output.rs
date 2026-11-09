use super::catalog::Tool;
use super::parameters::*;
use remote_ops_protocol::arguments::{
    MAX_PID_RESULTS, MAX_PROCESS_CMDLINE_BYTES, MAX_PROCESS_SCAN_BYTES, MAX_PROCESS_SCAN_COUNT,
};
use remote_ops_protocol::*;
use serde_json::{Value, json};

pub(super) fn output_schema(tool: Tool) -> Value {
    crate::response::output_schema(tool.name(), full_output_schema(tool))
}

fn full_output_schema(tool: Tool) -> Value {
    match tool {
        Tool::ReadText => strict_output(
            json!({
                "offset":{"type":"integer","minimum":0},
                "bytes_read":{"type":"integer","minimum":0},
                "next_offset":{"type":"integer","minimum":0},
                "truncated":{"type":"boolean"}
            }),
            &["offset", "bytes_read", "next_offset", "truncated"],
        ),
        Tool::ReadFileLines => strict_output(
            json!({
                "start_line":{"type":"integer","minimum":1},
                "end_line":{"type":"integer","minimum":1},
                "lines_returned":{"type":"integer","minimum":0,"maximum":READ_FILE_LINES_MAX_LINES},
                "bytes_returned":{"type":"integer","minimum":0,"maximum":READ_FILE_LINES_MAX_BYTES},
                "next_line":{"type":["integer","null"],"minimum":1},
                "truncated":{"type":"boolean"}
            }),
            &[
                "start_line",
                "end_line",
                "lines_returned",
                "bytes_returned",
                "next_line",
                "truncated",
            ],
        ),
        Tool::TailText => strict_output(
            json!({
                "bytes_scanned":{"type":"integer","minimum":0},
                "lines_returned":{"type":"integer","minimum":0},
                "truncated":{"type":"boolean"}
            }),
            &["bytes_scanned", "lines_returned", "truncated"],
        ),
        Tool::WriteText => strict_output(
            json!({"bytes_written":{"type":"integer","minimum":0}}),
            &["bytes_written"],
        ),
        Tool::ApplyPatch => strict_output(
            json!({
                "bytes_before":{"type":"integer","minimum":0},
                "bytes_after":{"type":"integer","minimum":0},
                "hunks_applied":{"type":"integer","minimum":1,"maximum":APPLY_PATCH_MAX_HUNKS},
                "sha256_before":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                "sha256_after":{"type":"string","pattern":"^[0-9a-f]{64}$"}
            }),
            &[
                "bytes_before",
                "bytes_after",
                "hunks_applied",
                "sha256_before",
                "sha256_after",
            ],
        ),
        Tool::ListFiles => strict_output(
            json!({
                "entries":{"type":"array","items":{
                    "type":"object",
                    "properties":{
                        "name":{"type":"string"},
                        "kind":{"type":"string","enum":["file","dir","symlink","other"]},
                        "size":{"type":"integer","minimum":0},
                        "mtime":{"type":"integer","description":"Modification time as seconds since the Unix epoch"},
                        "mtime_iso":{"type":"string","description":"Modification time as RFC 3339 in the Agent's local timezone"},
                        "mode_str":{"type":"string","description":"lstat-style 10-character type and permission string"}
                    },
                    "required":["name","kind","size","mtime","mtime_iso","mode_str"],"additionalProperties":false
                }},
                "next_cursor":{"type":["string","null"]},
                "truncated":{"type":"boolean"}
            }),
            &["entries", "next_cursor", "truncated"],
        ),
        Tool::Grep => strict_output(
            json!({
                "matches":{"type":"array","maxItems":GREP_MAX_RESULTS,"items":{
                    "type":"object",
                    "properties":{
                        "path":{"type":"string"},
                        "line":{"type":"integer","minimum":1},
                        "column":{"type":"integer","minimum":1},
                        "text":{"type":"string"},
                        "text_truncated":{"type":"boolean"}
                    },
                    "required":["path","line","column","text","text_truncated"],
                    "additionalProperties":false
                }},
                "files_scanned":{"type":"integer","minimum":0,"maximum":10000},
                "files_skipped":{"type":"integer","minimum":0},
                "bytes_scanned":{"type":"integer","minimum":0,"maximum":67108864},
                "truncated":{"type":"boolean"}
            }),
            &[
                "matches",
                "files_scanned",
                "files_skipped",
                "bytes_scanned",
                "truncated",
            ],
        ),
        Tool::Stat => strict_output(
            json!({
                "size":{"type":"integer","minimum":0},
                "mtime":{"type":"integer","description":"Modification time as seconds since the Unix epoch"},
                "mtime_iso":{"type":"string","description":"Modification time as RFC 3339 in the Agent's local timezone"},
                "mode":{"type":"integer","minimum":0,"description":"Raw Unix st_mode (0 when the platform has no Unix mode)"},
                "mode_str":{"type":"string","description":"lstat-style 10-character type and permission string"},
                "kind":{"type":"string","enum":["file","dir","symlink","other"]}
            }),
            &["size", "mtime", "mtime_iso", "mode", "mode_str", "kind"],
        ),
        Tool::Batch => strict_output(
            json!({
                "results":{"type":"array","maxItems":super::batch::MAX_CALLS,"items":{
                    "type":"object",
                    "properties":{
                        "tool":{"type":"string"},
                        "ok":{"type":"boolean"},
                        "result":{"type":"object","description":"Tool result when ok is true"},
                        "error":client_error_schema(false)
                    },
                    "required":["tool","ok"],"additionalProperties":false
                }}
            }),
            &["results"],
        ),
        Tool::FileHash => strict_output(
            json!({
                "algorithm":{"type":"string","const":"sha256"},
                "digest":{"type":"string"},
                "bytes_hashed":{"type":"integer","minimum":0}
            }),
            &["algorithm", "digest", "bytes_hashed"],
        ),
        Tool::Mkdir => strict_output(
            json!({
                "path":{"type":"string"},
                "created":{"type":"boolean"},
                "recursive":{"type":"boolean"},
                "mode":{"type":["integer","null"],"minimum":0,"maximum":MAX_UNIX_MODE}
            }),
            &["path", "created", "recursive", "mode"],
        ),
        Tool::Remove => strict_output(
            json!({
                "path":{"type":"string"},
                "kind":{"type":"string","enum":["file","dir","symlink"]},
                "recursive":{"type":"boolean"},
                "entries_removed":{"type":"integer","minimum":1,"maximum":remote_ops_protocol::MAX_FILE_OPERATION_ENTRIES}
            }),
            &["path", "kind", "recursive", "entries_removed"],
        ),
        Tool::Move => strict_output(
            json!({
                "source":{"type":"string"},
                "destination":{"type":"string"},
                "kind":{"type":"string","enum":["file","dir","symlink"]},
                "overwritten":{"type":"boolean"},
                "cross_filesystem":{"type":"boolean","const":false},
                "atomic":{"type":"boolean"}
            }),
            &[
                "source",
                "destination",
                "kind",
                "overwritten",
                "cross_filesystem",
                "atomic",
            ],
        ),
        Tool::Copy => strict_output(
            json!({
                "source":{"type":"string"},
                "destination":{"type":"string"},
                "kind":{"type":"string","enum":["file","dir"]},
                "recursive":{"type":"boolean"},
                "overwritten":{"type":"boolean"},
                "entries_copied":{"type":"integer","minimum":1,"maximum":remote_ops_protocol::MAX_FILE_OPERATION_ENTRIES},
                "bytes_copied":{"type":"integer","minimum":0}
            }),
            &[
                "source",
                "destination",
                "kind",
                "recursive",
                "overwritten",
                "entries_copied",
                "bytes_copied",
            ],
        ),
        Tool::Chmod => strict_output(
            json!({
                "path":{"type":"string"},
                "kind":{"type":"string","enum":["file","dir"]},
                "mode":{"type":"integer","minimum":0,"maximum":MAX_UNIX_MODE}
            }),
            &["path", "kind", "mode"],
        ),
        Tool::Symlink => strict_output(
            json!({
                "target":{"type":"string"},
                "link_path":{"type":"string"},
                "target_kind":{"type":["string","null"],"enum":["file","dir",null]},
                "overwritten":{"type":"boolean"},
                "atomic":{"type":"boolean"}
            }),
            &[
                "target",
                "link_path",
                "target_kind",
                "overwritten",
                "atomic",
            ],
        ),
        Tool::SyncDirectory => strict_output(
            json!({
                "committed":{"type":"boolean","const":true},
                "local_path":{"type":"string"},
                "remote_path":{"type":"string"},
                "manifest_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                "files":{"type":"integer","minimum":0,"maximum":MAX_SYNC_FILES},
                "directories":{"type":"integer","minimum":0,"maximum":MAX_SYNC_FILES},
                "total_bytes":{"type":"integer","minimum":0,"maximum":DEFAULT_MAX_TRANSFER_BYTES},
                "files_transferred":{"type":"integer","minimum":0,"maximum":MAX_SYNC_FILES},
                "bytes_transferred":{"type":"integer","minimum":0,"maximum":DEFAULT_MAX_TRANSFER_BYTES},
                "files_reused":{"type":"integer","minimum":0,"maximum":MAX_SYNC_FILES},
                "bytes_reused":{"type":"integer","minimum":0,"maximum":DEFAULT_MAX_TRANSFER_BYTES},
                "backup_path":{"type":["string","null"]},
                "staging_path":{"type":"string"},
                "commit":{"type":"object","description":"Agent commit confirmation and durability/cleanup information"}
            }),
            &[
                "committed",
                "local_path",
                "remote_path",
                "manifest_sha256",
                "files",
                "directories",
                "total_bytes",
                "files_transferred",
                "bytes_transferred",
                "files_reused",
                "bytes_reused",
                "backup_path",
                "staging_path",
                "commit",
            ],
        ),
        Tool::DeployRelease => strict_output(
            json!({
                "status":{"type":"string","enum":["deployed","stop_failed","rolled_back","rollback_failed"]},
                "deployed":{"type":"boolean"},
                "rolled_back":{"type":"boolean"},
                "release_id":{"type":"string"},
                "release_path":{"type":"string"},
                "current_path":{"type":"string"},
                "manifest_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                "preflight":{"type":"object"},
                "sync":{"type":"object"},
                "activation":{"type":"object"}
            }),
            &[
                "status",
                "deployed",
                "rolled_back",
                "release_id",
                "release_path",
                "current_path",
                "manifest_sha256",
                "preflight",
                "sync",
                "activation",
            ],
        ),
        Tool::Pids => strict_output(
            json!({
                "processes":{"type":"array","maxItems":MAX_PID_RESULTS,"items":{
                    "type":"object","properties":{"pid":{"type":"integer","minimum":1},"name":{"type":"string"},"cmdline":{"type":"string","maxLength":MAX_PROCESS_CMDLINE_BYTES},"cmdline_available":{"type":"boolean"},"cmdline_truncated":{"type":"boolean"}},
                    "required":["pid","name","cmdline","cmdline_available","cmdline_truncated"],"additionalProperties":false
                }},
                "next_cursor":{"type":["string","null"]},
                "truncated":{"type":"boolean"},
                "scan":strict_output(json!({
                    "processes_scanned":{"type":"integer","minimum":0,"maximum":MAX_PROCESS_SCAN_COUNT},
                    "cmdline_bytes_read":{"type":"integer","minimum":0,"maximum":MAX_PROCESS_SCAN_BYTES},
                    "filter_incomplete":{"type":"boolean"}
                }), &["processes_scanned","cmdline_bytes_read","filter_incomplete"])
            }),
            &["processes", "next_cursor", "truncated", "scan"],
        ),
        Tool::ProcessInfo => strict_output(
            json!({
                "pid":{"type":"integer","minimum":1},"ppid":{"type":"integer","minimum":0},
                "name":{"type":"string"},"state":{"type":["string","null"]},"cmdline":{"type":"string","maxLength":MAX_PROCESS_CMDLINE_BYTES},
                "cmdline_available":{"type":"boolean"},"cmdline_truncated":{"type":"boolean"},
                "uid":{"type":["integer","null"],"minimum":0},
                "resident_bytes":{"type":["integer","null"],"minimum":0},
                "virtual_bytes":{"type":["integer","null"],"minimum":0},
                "start_time_ticks":{"type":"integer","minimum":0},
                "start_time_seconds":{"type":"number","minimum":0},
                "start_time_iso":{"type":["string","null"],"description":"Process start time as RFC 3339 in the Agent's local timezone"}
            }),
            &[
                "pid",
                "ppid",
                "name",
                "state",
                "cmdline",
                "cmdline_available",
                "cmdline_truncated",
                "uid",
                "resident_bytes",
                "virtual_bytes",
                "start_time_ticks",
                "start_time_seconds",
                "start_time_iso",
            ],
        ),
        Tool::Kill => strict_output(
            json!({"pid":{"type":"integer","minimum":1},"signal":{"type":"integer","minimum":1,"maximum":64}}),
            &["pid", "signal"],
        ),
        Tool::Pkill => strict_output(
            json!({
                "name":{"type":"string","maxLength":260},
                "signal":{"type":"integer","minimum":1,"maximum":64},
                "matched":{"type":"integer","minimum":0,"maximum":1024},
                "signaled_pids":{"type":"array","maxItems":1024,"items":{"type":"integer","minimum":1}},
                "failed_pids":{"type":"array","maxItems":1024,"items":{"type":"integer","minimum":1}}
            }),
            &["name", "signal", "matched", "signaled_pids", "failed_pids"],
        ),
        Tool::ShExec => {
            let mut schema = full_output_schema(Tool::Exec);
            schema["properties"]["shell"] = json!({
                "type":"object","properties":{
                    "name":{"type":"string","enum":["sh","bash","busybox","pwsh","powershell","cmd","git_bash"]},
                    "program":{"type":"string","maxLength":remote_ops_protocol::execution::MAX_EXECUTION_PATH_BYTES}
                },
                "required":["name","program"],"additionalProperties":false
            });
            schema["required"]
                .as_array_mut()
                .expect("command schema")
                .push(json!("shell"));
            schema
        }
        Tool::Exec => strict_output(
            json!({
                "stdout":{"type":"string"},"stderr":{"type":"string"},"exit_code":{"type":["integer","null"]},
                "timed_out":{"type":"boolean"},"duration_ms":{"type":"integer","minimum":0},
                "stdout_truncated":{"type":"boolean"},"stderr_truncated":{"type":"boolean"}
            }),
            &[
                "stdout",
                "stderr",
                "exit_code",
                "timed_out",
                "duration_ms",
                "stdout_truncated",
                "stderr_truncated",
            ],
        ),
        Tool::ProcessStart | Tool::ProcessWait => process_status_output(json!({}), &[]),
        Tool::ProcessOutput => process_status_output(
            json!({
                "stdout":{"type":"string"},
                "stderr":{"type":"string"},
                "stdout_start_cursor":{"type":"integer","minimum":0},
                "stderr_start_cursor":{"type":"integer","minimum":0},
                "next_stdout_cursor":{"type":"integer","minimum":0},
                "next_stderr_cursor":{"type":"integer","minimum":0},
                "stdout_truncated":{"type":"boolean"},
                "stderr_truncated":{"type":"boolean"}
            }),
            &[
                "stdout",
                "stderr",
                "stdout_start_cursor",
                "stderr_start_cursor",
                "next_stdout_cursor",
                "next_stderr_cursor",
                "stdout_truncated",
                "stderr_truncated",
            ],
        ),
        Tool::ProcessSignal => process_status_output(
            json!({"signal":{"type":"integer","minimum":1,"maximum":64}}),
            &["signal"],
        ),
        Tool::ProcessClose => strict_output(
            json!({
                "job_id":{"type":"integer","minimum":1},
                "closed":{"type":"boolean","const":true}
            }),
            &["job_id", "closed"],
        ),
        Tool::SystemInfo => system_info_output_schema(),
        Tool::Environment => crate::environment_schema::output_schema(),
        Tool::AgentInfo => agent_info_output_schema(),
        Tool::RemoteProbe => remote_probe_output_schema(),
        Tool::WaitRemote => strict_output(
            json!({
                "address":{"type":"string"},
                "wait_for":{"type":"string","enum":["online","offline","offline_then_online"]},
                "reached":{"type":"boolean"},
                "timed_out":{"type":"boolean"},
                "observed_offline":{"type":"boolean"},
                "attempts":{"type":"integer","minimum":0},
                "elapsed_ms":{"type":"integer","minimum":0},
                "connected":{"type":"boolean"},
                "connection_state":{"type":"string","enum":["cached","disconnected"]},
                "lifecycle_state":{"type":"string","enum":["ready","rebooting","updating"]},
                "last_probe":{"type":["object","null"]},
                "agent_info":{"type":["object","null"]}
            }),
            &[
                "address",
                "wait_for",
                "reached",
                "timed_out",
                "observed_offline",
                "attempts",
                "elapsed_ms",
                "connected",
                "connection_state",
                "lifecycle_state",
                "last_probe",
                "agent_info",
            ],
        ),
        Tool::Reboot => strict_output(
            json!({
                "accepted":{"type":"boolean","const":true},
                "acknowledged":{"type":"boolean"},
                "disconnect_observed":{"type":"boolean"},
                "requested_at_ms":{"type":"integer","minimum":0},
                "delay_ms":{"type":"integer","minimum":MIN_REBOOT_DELAY_MS,"maximum":MAX_REBOOT_DELAY_MS},
                "previous_instance_id":{"type":["string","null"]},
                "lifecycle_state":{"type":"string","const":"rebooting"},
                "agent_response":{"type":["object","null"]}
            }),
            &[
                "accepted",
                "acknowledged",
                "disconnect_observed",
                "requested_at_ms",
                "delay_ms",
                "previous_instance_id",
                "lifecycle_state",
                "agent_response",
            ],
        ),
        Tool::AgentUpdate => strict_output(
            json!({
                "status":{"type":"string","enum":["updated","rolled_back","timed_out","unconfirmed"]},
                "updated":{"type":"boolean"},
                "rolled_back":{"type":"boolean"},
                "timed_out":{"type":"boolean"},
                "restart_acknowledged":{"type":"boolean"},
                "previous_agent":{"type":"object"},
                "candidate":{"type":["object","null"]},
                "current_agent":{"type":["object","null"]},
                "update_receipt":{"type":["object","null"]},
                "recovery":{"type":["object","null"]},
                "staging_path":{"type":"string"},
                "bytes_transferred":{"type":"integer","minimum":0},
                "sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                "wait":{"type":"object"},
                "elapsed_ms":{"type":"integer","minimum":0}
            }),
            &[
                "status",
                "updated",
                "rolled_back",
                "timed_out",
                "restart_acknowledged",
                "previous_agent",
                "candidate",
                "current_agent",
                "update_receipt",
                "recovery",
                "staging_path",
                "bytes_transferred",
                "sha256",
                "wait",
                "elapsed_ms",
            ],
        ),
        Tool::UploadFile => strict_output(
            json!({
                "bytes_transferred":{"type":"integer","minimum":0},
                "size":{"type":"integer","minimum":0},
                "resumed_from":{"type":"integer","minimum":0},
                "sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                "mode":{"type":["integer","null"],"minimum":0,"maximum":MAX_UNIX_MODE},
                "source":{"type":"string"},"destination":{"type":"string"}
            }),
            &[
                "bytes_transferred",
                "size",
                "resumed_from",
                "sha256",
                "mode",
                "source",
                "destination",
            ],
        ),
        Tool::DownloadFile => strict_output(
            json!({
                "bytes_transferred":{"type":"integer","minimum":0},
                "size":{"type":"integer","minimum":0},
                "resumed_from":{"type":"integer","minimum":0},
                "sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                "source":{"type":"string"},"destination":{"type":"string"}
            }),
            &[
                "bytes_transferred",
                "size",
                "resumed_from",
                "sha256",
                "source",
                "destination",
            ],
        ),
        Tool::RemoteStatus | Tool::SetRemote => strict_output(
            json!({
                "ip":{"type":"string","description":"Configured remote IPv4 address"},
                "port":{"type":"integer","minimum":1,"maximum":65535},
                "address":{"type":"string","description":"Configured remote IPv4:PORT"},
                "connected":{"type":"boolean","description":"Whether the proxy holds an authenticated session; no active probe is performed"},
                "connection_state":{"type":"string","enum":["cached","disconnected"]},
                "lifecycle_state":{"type":"string","enum":["ready","rebooting","updating"]},
                "last_success_at_ms":{"type":["integer","null"],"minimum":0},
                "last_error":client_error_schema(true),
                "last_probe":{"type":["object","null"]},
                "agent_info":{"type":["object","null"]}
            }),
            &[
                "ip",
                "port",
                "address",
                "connected",
                "connection_state",
                "lifecycle_state",
                "last_success_at_ms",
                "last_error",
                "last_probe",
                "agent_info",
            ],
        ),
    }
}

fn system_info_output_schema() -> Value {
    let nullable_string = |max_length| json!({"type":["string","null"],"maxLength":max_length});
    let account = || {
        json!({
            "type":"object",
            "properties":{
                "id":{"type":["integer","null"],"minimum":0},
                "name":nullable_string(256)
            },
            "required":["id","name"],
            "additionalProperties":false
        })
    };
    let capability_set = || {
        json!({
            "type":"object",
            "properties":{
                "mask":{"type":"string","pattern":"^[0-9A-Fa-f]{1,16}$"},
                "names":{"type":"array","maxItems":64,"items":{"type":"string","pattern":"^CAP_[A-Z0-9_]+$"}}
            },
            "required":["mask","names"],
            "additionalProperties":false
        })
    };
    let bounded_collection = |items: Value, max_items: usize| {
        json!({
            "type":"object",
            "properties":{
                "available":{"type":"boolean"},
                "items":{"type":"array","maxItems":max_items,"items":items},
                "truncated":{"type":"boolean"}
            },
            "required":["available","items","truncated"],
            "additionalProperties":false
        })
    };
    let interface_address = json!({
        "type":"object",
        "properties":{
            "family":{"type":"string","enum":["ipv4","ipv6"]},
            "address":{"type":"string","maxLength":64},
            "prefix_length":{"type":"integer","minimum":0,"maximum":128},
            "scope":{"type":"string","enum":["host","unspecified","multicast","link","global"]}
        },
        "required":["family","address","prefix_length","scope"],
        "additionalProperties":false
    });
    let network_interface = json!({
        "type":"object",
        "properties":{
            "name":{"type":"string","maxLength":256},
            "index":{"type":["integer","null"],"minimum":0},
            "up":{"type":"boolean"},
            "loopback":{"type":"boolean"},
            "point_to_point":{"type":"boolean"},
            "mac_address":nullable_string(128),
            "mtu":{"type":["integer","null"],"minimum":0},
            "addresses":{"type":"array","maxItems":512,"items":interface_address}
        },
        "required":["name","index","up","loopback","point_to_point","mac_address","mtu","addresses"],
        "additionalProperties":false
    });
    let route = json!({
        "type":"object",
        "properties":{
            "family":{"type":"string","enum":["ipv4","ipv6"]},
            "destination":{"type":"string","maxLength":128},
            "gateway":nullable_string(64),
            "interface":{"type":"string","maxLength":256},
            "metric":{"type":"integer","minimum":0},
            "flags":{"type":"integer","minimum":0}
        },
        "required":["family","destination","gateway","interface","metric","flags"],
        "additionalProperties":false
    });
    let listening_port = json!({
        "type":"object",
        "properties":{
            "protocol":{"type":"string","enum":["tcp","udp"]},
            "family":{"type":"string","enum":["ipv4","ipv6"]},
            "local_address":{"type":"string","maxLength":64},
            "port":{"type":"integer","minimum":1,"maximum":65535}
        },
        "required":["protocol","family","local_address","port"],
        "additionalProperties":false
    });
    let mount = json!({
        "type":"object",
        "properties":{
            "source":{"type":"string","maxLength":1024},
            "mount_point":{"type":"string","maxLength":1024},
            "fs_type":nullable_string(128),
            "total_bytes":{"type":["integer","null"],"minimum":0},
            "available_bytes":{"type":["integer","null"],"minimum":0},
            "total_inodes":{"type":["integer","null"],"minimum":0},
            "available_inodes":{"type":["integer","null"],"minimum":0},
            "read_only":{"type":["boolean","null"]}
        },
        "required":["source","mount_point","fs_type","total_bytes","available_bytes","total_inodes","available_inodes","read_only"],
        "additionalProperties":false
    });
    let toolchain = json!({
        "type":"object",
        "properties":{
            "name":{"type":"string","maxLength":64},
            "path":{"type":"string","maxLength":1024}
        },
        "required":["name","path"],
        "additionalProperties":false
    });
    strict_output(
        json!({
            "hostname":{"type":"string","maxLength":256},
            "kernel":{"type":"object","properties":{"sysname":{"type":"string","maxLength":256},"release":{"type":"string","maxLength":256},"machine":{"type":"string","maxLength":256}},"required":["sysname","release","machine"],"additionalProperties":false},
            "uptime_seconds":{"type":"number","minimum":0},
            "load_average":{"type":"object","properties":{"one":{"type":"number"},"five":{"type":"number"},"fifteen":{"type":"number"}},"required":["one","five","fifteen"],"additionalProperties":false},
            "memory":{"type":"object","properties":{"total_bytes":{"type":"integer","minimum":0},"available_bytes":{"type":"integer","minimum":0}},"required":["total_bytes","available_bytes"],"additionalProperties":false},
            "root_filesystem":{"type":"object","properties":{"total_bytes":{"type":"integer","minimum":0},"available_bytes":{"type":"integer","minimum":0}},"required":["total_bytes","available_bytes"],"additionalProperties":false},
            "temperatures":{"type":"array","maxItems":64,"items":{"type":"object","properties":{"name":{"type":"string","maxLength":256},"celsius":{"type":"number"}},"required":["name","celsius"],"additionalProperties":false}},
            "os":{
                "type":"object",
                "properties":{
                    "id":nullable_string(4096),
                    "id_like":{"type":"array","maxItems":16,"items":{"type":"string","maxLength":128}},
                    "name":nullable_string(4096),
                    "pretty_name":nullable_string(4096),
                    "version":nullable_string(4096),
                    "version_id":nullable_string(4096),
                    "version_codename":nullable_string(4096),
                    "variant":nullable_string(4096),
                    "variant_id":nullable_string(4096),
                    "build_id":nullable_string(4096),
                    "image_id":nullable_string(4096),
                    "image_version":nullable_string(4096)
                },
                "required":["id","id_like","name","pretty_name","version","version_id","version_codename","variant","variant_id","build_id","image_id","image_version"],
                "additionalProperties":false
            },
            "cpu":{
                "type":"object",
                "properties":{
                    "model":nullable_string(512),
                    "logical_cores":{"type":"integer","minimum":1},
                    "physical_cores":{"type":["integer","null"],"minimum":1},
                    "architecture":{"type":"string","maxLength":128},
                    "byte_order":{"type":"string","enum":["little","big"]},
                    "abi":{"type":"string","maxLength":128},
                    "build_target":{"type":"string","maxLength":256},
                    "libc":{"type":"object","properties":{"family":{"type":"string","maxLength":128},"version":nullable_string(256)},"required":["family","version"],"additionalProperties":false}
                },
                "required":["model","logical_cores","physical_cores","architecture","byte_order","abi","build_target","libc"],
                "additionalProperties":false
            },
            "identity":{
                "type":"object",
                "properties":{
                    "real_user":account(),
                    "effective_user":account(),
                    "real_group":account(),
                    "effective_group":account(),
                    "supplementary_groups":bounded_collection(account(), 128),
                    "is_root":{"type":["boolean","null"]},
                    "umask":nullable_string(16),
                    "capabilities":{
                        "type":["object","null"],
                        "properties":{
                            "inheritable":capability_set(),
                            "permitted":capability_set(),
                            "effective":capability_set(),
                            "bounding":capability_set(),
                            "ambient":capability_set(),
                            "last_capability":{"type":"integer","minimum":0,"maximum":63}
                        },
                        "required":["inheritable","permitted","effective","bounding","ambient","last_capability"],
                        "additionalProperties":false
                    }
                },
                "required":["real_user","effective_user","real_group","effective_group","supplementary_groups","is_root","umask","capabilities"],
                "additionalProperties":false
            },
            "network":{
                "type":"object",
                "properties":{
                    "interfaces":bounded_collection(network_interface, 128),
                    "routes":bounded_collection(route, 256),
                    "dns":{
                        "type":"object",
                        "properties":{
                            "available":{"type":"boolean"},
                            "servers":{"type":"array","maxItems":16,"items":{"type":"string","maxLength":256}},
                            "search_domains":{"type":"array","maxItems":16,"items":{"type":"string","maxLength":256}},
                            "truncated":{"type":"boolean"}
                        },
                        "required":["available","servers","search_domains","truncated"],
                        "additionalProperties":false
                    },
                    "listening_ports":bounded_collection(listening_port, 512)
                },
                "required":["interfaces","routes","dns","listening_ports"],
                "additionalProperties":false
            },
            "filesystems":{
                "type":"object",
                "properties":{
                    "available":{"type":"boolean"},
                    "mounts":{"type":"array","maxItems":256,"items":mount},
                    "truncated":{"type":"boolean"}
                },
                "required":["available","mounts","truncated"],
                "additionalProperties":false
            },
            "time":{
                "type":"object",
                "properties":{
                    "unix_seconds":{"type":"integer","minimum":0},
                    "iso":{"type":"string","description":"Current time as RFC 3339 in the Agent's local timezone"},
                    "timezone":nullable_string(256),
                    "utc_offset_seconds":{"type":["integer","null"],"minimum":-86400,"maximum":86400}
                },
                "required":["unix_seconds","iso","timezone","utc_offset_seconds"],
                "additionalProperties":false
            },
            "init_system":{
                "type":"object",
                "properties":{"name":nullable_string(256),"pid1_comm":nullable_string(256)},
                "required":["name","pid1_comm"],
                "additionalProperties":false
            },
            "toolchains":bounded_collection(toolchain, 24)
        }),
        &[
            "hostname",
            "kernel",
            "uptime_seconds",
            "load_average",
            "memory",
            "root_filesystem",
            "temperatures",
            "os",
            "cpu",
            "identity",
            "network",
            "filesystems",
            "time",
            "init_system",
            "toolchains",
        ],
    )
}

fn agent_info_output_schema() -> Value {
    strict_output(
        json!({
            "name":{"type":"string","const":"remote-ops-agent"},
            "version":{"type":"string"},
            "protocol_version":{"type":"integer","minimum":1},
            "build":{
                "type":"object",
                "properties":{
                    "target":{"type":"string"},
                    "profile":{"type":"string"},
                    "git_revision":{"type":"string"}
                },
                "required":["target","profile","git_revision"],
                "additionalProperties":false
            },
            "runtime":{
                "type":"object",
                "properties":{
                    "instance_id":{"type":"string","pattern":"^[0-9a-f]{32}$"},
                    "pid":{"type":"integer","minimum":1},
                    "started_at_ms":{"type":"integer","minimum":0},
                    "started_at_iso":{"type":"string","description":"Agent start time as RFC 3339 in the Agent's local timezone"},
                    "uptime_ms":{"type":"integer","minimum":0}
                },
                "required":["instance_id","pid","started_at_ms","started_at_iso","uptime_ms"],
                "additionalProperties":false
            },
            "platform":{
                "type":"object",
                "properties":{
                    "os":{"type":"string"},
                    "arch":{"type":"string"},
                    "family":{"type":"string"}
                },
                "required":["os","arch","family"],
                "additionalProperties":false
            },
            "supported_operations":{"type":"array","items":{"type":"string"}},
            "capabilities":{
                "type":"object",
                "properties":{
                    "background_processes":{"type":"boolean"},
                    "incremental_output":{"type":"boolean"},
                    "active_probe":{"type":"boolean"},
                    "wait_remote":{"type":"boolean"},
                    "reboot":{"type":"boolean"},
                    "self_update":{"type":"boolean"},
                    "resumable_transfers":{"type":"boolean"},
                    "directory_sync":{"type":"boolean"},
                    "release_deployment":{"type":"boolean"},
                    "unix_mode":{"type":"boolean"},
                    "shell_selection":{"type":"boolean"},
                    "environment_profile":{"type":"boolean"},
                    "update_receipts":{"type":"boolean"}
                },
                "required":[
                    "background_processes","incremental_output","active_probe","wait_remote",
                    "reboot","self_update","resumable_transfers","directory_sync","release_deployment",
                    "unix_mode","shell_selection","environment_profile","update_receipts"
                ],
                "additionalProperties":false
            },
            "limits":{
                "type":"object",
                "properties":{
                    "max_control_bytes":{"type":"integer","minimum":1},
                    "chunk_bytes":{"type":"integer","minimum":1},
                    "max_transfer_bytes":{"type":"integer","minimum":1},
                    "max_process_jobs":{"type":"integer","minimum":1},
                    "default_process_timeout_ms":{"type":"integer","minimum":1},
                    "max_process_timeout_ms":{"type":"integer","minimum":1},
                    "process_output_buffer_bytes":{"type":"integer","minimum":1},
                    "default_process_output_bytes":{"type":"integer","minimum":1},
                    "max_process_output_bytes":{"type":"integer","minimum":1},
                    "default_process_wait_ms":{"type":"integer","minimum":0},
                    "max_process_wait_ms":{"type":"integer","minimum":0},
                    "apply_patch_max_patch_bytes":{"type":"integer","minimum":1},
                    "apply_patch_max_file_bytes":{"type":"integer","minimum":1},
                    "apply_patch_max_hunks":{"type":"integer","minimum":1},
                    "max_file_operation_entries":{"type":"integer","minimum":1},
                    "default_sync_max_files":{"type":"integer","minimum":1},
                    "max_sync_files":{"type":"integer","minimum":1},
                    "default_sync_max_depth":{"type":"integer","minimum":1},
                    "max_sync_depth":{"type":"integer","minimum":1},
                    "max_sync_exclude_patterns":{"type":"integer","minimum":1}
                },
                "required":[
                    "max_control_bytes","chunk_bytes","max_transfer_bytes","max_process_jobs",
                    "default_process_timeout_ms","max_process_timeout_ms",
                    "process_output_buffer_bytes","default_process_output_bytes",
                    "max_process_output_bytes","default_process_wait_ms","max_process_wait_ms",
                    "apply_patch_max_patch_bytes","apply_patch_max_file_bytes",
                    "apply_patch_max_hunks","max_file_operation_entries",
                    "default_sync_max_files","max_sync_files","default_sync_max_depth",
                    "max_sync_depth","max_sync_exclude_patterns"
                ],
                "additionalProperties":false
            },
            "update":{
                "type":"object",
                "properties":{
                    "executable_path":{"type":["string","null"]},
                    "staging_path":{"type":["string","null"]},
                    "self_check_timeout_ms":{"type":"integer","minimum":1},
                    "last_result":{"type":["object","null"],"description":"Bounded helper receipt for the latest update attempt"}
                },
                "required":["executable_path","staging_path","self_check_timeout_ms","last_result"],
                "additionalProperties":false
            }
        }),
        &[
            "name",
            "version",
            "protocol_version",
            "build",
            "runtime",
            "platform",
            "supported_operations",
            "capabilities",
            "limits",
            "update",
        ],
    )
}

fn remote_probe_output_schema() -> Value {
    strict_output(
        json!({
            "address":{"type":"string"},
            "reachable":{"type":"boolean"},
            "connected":{"type":"boolean"},
            "connection_reused":{"type":"boolean"},
            "latency_ms":{"type":"integer","minimum":0},
            "probed_at_ms":{"type":"integer","minimum":0},
            "lifecycle_state":{"type":"string","enum":["ready","rebooting","updating"]},
            "agent_info":{"type":["object","null"]},
            "error":client_error_schema(true)
        }),
        &[
            "address",
            "reachable",
            "connected",
            "connection_reused",
            "latency_ms",
            "probed_at_ms",
            "lifecycle_state",
            "agent_info",
            "error",
        ],
    )
}

fn process_status_output(extra: Value, extra_required: &[&str]) -> Value {
    let mut properties = match json!({
        "job_id":{"type":"integer","minimum":1},
        "pid":{"type":"integer","minimum":1},
        "state":{"type":"string","enum":["running","exited","failed"]},
        "exit_code":{"type":["integer","null"]},
        "timed_out":{"type":"boolean"},
        "error":{"type":["string","null"]},
        "started_at_ms":{"type":"integer","minimum":0},
        "finished_at_ms":{"type":["integer","null"],"minimum":0},
        "timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_PROCESS_JOB_TIMEOUT_MS},
        "stdout_complete":{"type":"boolean"},
        "stderr_complete":{"type":"boolean"}
    }) {
        Value::Object(properties) => properties,
        _ => unreachable!("literal object"),
    };
    if let Value::Object(extra) = extra {
        properties.extend(extra);
    }
    let mut required = vec![
        "job_id",
        "pid",
        "state",
        "exit_code",
        "timed_out",
        "error",
        "started_at_ms",
        "finished_at_ms",
        "timeout_ms",
        "stdout_complete",
        "stderr_complete",
    ];
    required.extend_from_slice(extra_required);
    strict_output(Value::Object(properties), &required)
}

fn strict_output(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn client_error_schema(nullable: bool) -> Value {
    let mut schema = strict_output(
        json!({
            "kind":{"type":"string"},"message":{"type":"string"},
            "source":{"type":"string","enum":["proxy","remote","transport","protocol"]},
            "outcome":{"type":"string","enum":["not_sent","unknown","remote_error","rejected","response_limited","local_error"]},
            "request_started":{"type":"boolean"},
            "prior_requests":{"type":"integer","minimum":0},
            "recovery":{"type":"object","description":"Recovery context; inspect these paths and the failed stage before further mutations"}
        }),
        &[
            "kind",
            "message",
            "source",
            "outcome",
            "request_started",
            "prior_requests",
        ],
    );
    if nullable {
        schema["type"] = json!(["object", "null"]);
    }
    schema
}
