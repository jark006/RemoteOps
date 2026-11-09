//! Prepare every argument before any I/O; execute only a prepared operation.
use serde::Deserialize;
use serde_json::Value;

use super::{catalog::Tool, parameters::*};
use crate::{
    client::{ClientError, RemoteClient},
    deployment,
};

pub(super) struct PreparedCall {
    tool: Tool,
    action: Action,
}

enum Action {
    Remote(Value),
    Upload(UploadArgs),
    Download(DownloadArgs),
    Sync(SyncDirectoryArgs),
    Deploy(Box<DeployReleaseArgs>),
    Status,
    SetRemote(SetRemoteArgs),
    Probe(RemoteProbeArgs),
    Wait(WaitRemoteArgs),
    Reboot(RebootArgs),
    Update(AgentUpdateArgs),
}

pub(super) fn prepare(tool: Tool, arguments: Value) -> Result<PreparedCall, ClientError> {
    let action = match tool {
        Tool::ReadText => return core_remote::<ReadTextArgs>(tool, arguments),
        Tool::TailText => return core_remote::<TailTextArgs>(tool, arguments),
        Tool::WriteText => return core_remote::<WriteTextArgs>(tool, arguments),
        Tool::Stat => return core_remote::<PathArgs>(tool, arguments),
        Tool::FileHash => return core_remote::<HashArgs>(tool, arguments),
        Tool::Pids => return core_remote::<PidsArgs>(tool, arguments),
        Tool::ProcessInfo => return core_remote::<PidArgs>(tool, arguments),
        Tool::Kill => return core_remote::<KillArgs>(tool, arguments),
        Tool::Exec => return core_remote::<ExecArgs>(tool, arguments),
        Tool::ReadFileLines => return remote(tool, arguments, validate_read_file_lines_args),
        Tool::ApplyPatch => return remote(tool, arguments, validate_apply_patch_args),
        Tool::ListFiles => return remote(tool, arguments, validate_list_files_args),
        Tool::Grep => return remote(tool, arguments, validate_grep_args),
        Tool::Mkdir => return remote(tool, arguments, validate_mkdir_args),
        Tool::Remove => return remote(tool, arguments, validate_remove_args),
        Tool::Move => return remote(tool, arguments, validate_move_args),
        Tool::Copy => return remote(tool, arguments, validate_copy_args),
        Tool::Chmod => return remote(tool, arguments, validate_chmod_args),
        Tool::Symlink => return remote(tool, arguments, validate_symlink_args),
        Tool::Pkill => return remote(tool, arguments, validate_pkill_args),
        Tool::ProcessStart => return remote(tool, arguments, validate_process_start_args),
        Tool::ProcessOutput => return remote(tool, arguments, validate_process_output_args),
        Tool::ProcessWait => return remote(tool, arguments, validate_process_wait_args),
        Tool::ProcessSignal => return remote(tool, arguments, validate_process_signal_args),
        Tool::ProcessClose => return remote(tool, arguments, validate_job_id_args),
        Tool::ShExec => {
            return remote(tool, arguments, |args: remote_ops_protocol::ShellArgs| {
                args.validate()
                    .map_err(|message| ClientError::local("invalid_params", message))
            });
        }
        Tool::SystemInfo | Tool::Environment | Tool::AgentInfo => {
            return remote(tool, arguments, |_: EmptyArgs| Ok(()));
        }
        Tool::UploadFile => Action::Upload(validate_upload_args(decode_transfer(&arguments)?)?),
        Tool::DownloadFile => {
            Action::Download(validate_download_args(decode_transfer(&arguments)?)?)
        }
        Tool::SyncDirectory => {
            Action::Sync(validate_sync_directory_args(decode_transfer(&arguments)?)?)
        }
        Tool::DeployRelease => Action::Deploy(Box::new(validate_deploy_release_args(
            decode_transfer(&arguments)?,
        )?)),
        Tool::RemoteStatus => {
            let _: EmptyArgs = decode_transfer(&arguments)?;
            Action::Status
        }
        Tool::SetRemote => {
            let args: SetRemoteArgs = decode_transfer(&arguments)?;
            if (args.ip.is_none() && args.port.is_none()) || args.port == Some(0) {
                return invalid_params(
                    "at least one of ip or port must be provided; port must be in range 1..=65535",
                );
            }
            Action::SetRemote(args)
        }
        Tool::RemoteProbe => {
            Action::Probe(validate_remote_probe_args(decode_transfer(&arguments)?)?)
        }
        Tool::WaitRemote => Action::Wait(validate_wait_remote_args(decode_transfer(&arguments)?)?),
        Tool::Reboot => Action::Reboot(validate_reboot_args(decode_transfer(&arguments)?)?),
        Tool::AgentUpdate => {
            Action::Update(validate_agent_update_args(decode_transfer(&arguments)?)?)
        }
        Tool::Batch => return invalid_params("batch must be prepared as a complete batch"),
    };
    Ok(PreparedCall { tool, action })
}

fn remote<A: for<'de> Deserialize<'de>, R>(
    tool: Tool,
    arguments: Value,
    validate: impl FnOnce(A) -> Result<R, ClientError>,
) -> Result<PreparedCall, ClientError> {
    validate(decode_transfer(&arguments)?)?;
    crate::client::validate_request(tool.name(), &arguments)?;
    Ok(PreparedCall {
        tool,
        action: Action::Remote(arguments),
    })
}

fn core_remote<A: for<'de> Deserialize<'de> + ValidateArguments>(
    tool: Tool,
    arguments: Value,
) -> Result<PreparedCall, ClientError> {
    remote(tool, arguments, |args: A| {
        args.validate()
            .map_err(|message| ClientError::local("invalid_params", message))
    })
}

impl PreparedCall {
    pub(super) fn execute(self, client: &mut RemoteClient) -> Result<Value, ClientError> {
        let before = client.operation_attempts();
        let result = match self.action {
            Action::Remote(arguments) => client.call(self.tool.name(), arguments),
            Action::Upload(args) => client.upload(
                &args.local_path,
                &args.remote_path,
                args.overwrite,
                args.mode,
                args.resume,
            ),
            Action::Download(args) => client.download(
                &args.remote_path,
                &args.local_path,
                args.overwrite,
                args.resume,
            ),
            Action::Sync(args) => deployment::sync_directory(
                client,
                &args.local_path,
                &args.remote_path,
                args.sync_options(),
            ),
            Action::Deploy(args) => deployment::deploy_release(client, (*args).into_options()),
            Action::Status => Ok(client.remote_status()),
            Action::SetRemote(args) => client.set_remote(args.ip, args.port),
            Action::Probe(args) => Ok(client.remote_probe(args.timeout_ms)),
            Action::Wait(args) => Ok(client.wait_remote(
                args.wait_for.as_deref(),
                args.timeout_ms,
                args.poll_interval_ms,
                args.probe_timeout_ms,
            )),
            Action::Reboot(args) => client.reboot(args.delay_ms),
            Action::Update(args) => client.agent_update(
                &args.local_path,
                args.timeout_ms,
                args.poll_interval_ms,
                args.probe_timeout_ms,
            ),
        };
        result.map_err(|error| {
            error.workflow_context(client.operation_attempts().saturating_sub(before))
        })
    }
}
