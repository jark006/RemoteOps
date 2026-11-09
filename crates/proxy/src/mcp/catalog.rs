//! Canonical tool identities and execution policy. Adding a tool also requires
//! exhaustive input/output schema and argument preparation match arms.
use serde_json::{Value, json};

macro_rules! registry {
    ($( $id:ident => ($name:literal, $batch:literal, ($read:literal,$destructive:literal,$idempotent:literal), $open:literal), )*) => {
        #[derive(Clone,Copy,Debug,PartialEq,Eq)]
        pub(super) enum Tool { $( $id, )* }
        const NAMES: &[&str] = &[$($name,)*];
        impl Tool {
            pub(super) const ALL: &'static [Self] = &[$(Self::$id,)*];
            pub(super) fn parse(name: &str) -> Option<Self> { match name { $($name => Some(Self::$id),)* _ => None } }
            pub(super) fn name(self) -> &'static str { match self { $( Self::$id => $name, )* } }
            pub(super) fn batch_allowed(self) -> bool { match self { $( Self::$id => $batch, )* } }
            pub(super) fn annotations(self) -> Value { match self { $( Self::$id => json!({"readOnlyHint":$read,"destructiveHint":$destructive,"idempotentHint":$idempotent,"openWorldHint":$open}), )* } }
            pub(super) fn batch_names() -> Vec<&'static str> { Self::ALL.iter().filter(|tool| tool.batch_allowed()).map(|tool| tool.name()).collect() }
        }
    }
}

// name, batch eligibility, (read-only, destructive, idempotent), open world.
registry! {
    ReadText => ("read_text", true, (true,false,true), false),
    ReadFileLines => ("read_file_lines", true, (true,false,true), false),
    TailText => ("tail_text", true, (true,false,true), false),
    WriteText => ("write_text", false, (false,true,true), false),
    ApplyPatch => ("apply_patch", false, (false,true,true), false),
    ListFiles => ("list_files", true, (true,false,true), false),
    Grep => ("grep", true, (true,false,true), false),
    Stat => ("stat", true, (true,false,true), false),
    FileHash => ("file_hash", true, (true,false,true), false),
    Mkdir => ("mkdir", false, (false,true,true), false),
    Remove => ("remove", false, (false,true,true), false),
    Move => ("move", false, (false,true,true), false),
    Copy => ("copy", false, (false,true,true), false),
    Chmod => ("chmod", false, (false,true,true), false),
    Symlink => ("symlink", false, (false,true,true), false),
    SyncDirectory => ("sync_directory", false, (false,true,true), false),
    DeployRelease => ("deploy_release", false, (false,true,false), true),
    Pids => ("pids", true, (true,false,true), false),
    ProcessInfo => ("process_info", true, (true,false,true), false),
    Kill => ("kill", false, (false,true,false), false),
    Pkill => ("pkill", false, (false,true,false), false),
    ShExec => ("sh_exec", true, (false,true,false), true),
    Exec => ("exec", true, (false,true,false), true),
    ProcessStart => ("process_start", false, (false,true,false), true),
    ProcessOutput => ("process_output", false, (true,false,true), false),
    ProcessWait => ("process_wait", false, (true,false,true), false),
    ProcessSignal => ("process_signal", false, (false,true,false), false),
    ProcessClose => ("process_close", false, (false,true,false), false),
    SystemInfo => ("system_info", true, (true,false,true), false),
    Environment => ("environment", true, (true,false,true), false),
    UploadFile => ("upload_file", false, (false,true,true), false),
    DownloadFile => ("download_file", false, (false,true,true), false),
    RemoteStatus => ("remote_status", true, (true,false,true), false),
    SetRemote => ("set_remote", false, (false,true,true), false),
    AgentInfo => ("agent_info", true, (true,false,true), false),
    RemoteProbe => ("remote_probe", false, (true,false,true), false),
    WaitRemote => ("wait_remote", false, (true,false,true), false),
    Reboot => ("reboot", false, (false,true,false), true),
    AgentUpdate => ("agent_update", false, (false,true,false), true),
    Batch => ("batch", false, (false,true,false), true),
}

pub fn tool_names() -> &'static [&'static str] {
    NAMES
}
pub fn tool_definitions() -> Vec<Value> {
    Tool::ALL
        .iter()
        .copied()
        .map(super::input::definition)
        .collect()
}
