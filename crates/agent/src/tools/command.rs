use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{AgentError, AgentResult};

pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const MAX_TIMEOUT_MS: u64 = 300_000;
pub const OUTPUT_LIMIT: usize = 256 * 1024;

#[cfg(windows)]
const GIT_BASH_PATH: &str = r"C:\Program Files\Git\bin\bash.exe";

#[cfg(unix)]
pub fn sh_exec(command: &str, timeout_ms: u64) -> AgentResult<Value> {
    let mut process = Command::new("/bin/sh");
    process.arg("-c").arg(command);
    run(process, timeout_ms)
}

#[cfg(windows)]
pub fn sh_exec(command: &str, timeout_ms: u64) -> AgentResult<Value> {
    match std::fs::metadata(GIT_BASH_PATH) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(AgentError::unsupported(format!(
                "sh_exec requires Git Bash at {GIT_BASH_PATH}"
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AgentError::unsupported(format!(
                "sh_exec requires Git Bash at {GIT_BASH_PATH}"
            )));
        }
        Err(error) => return Err(AgentError::io("inspect Git Bash", error)),
    }

    let mut process = Command::new(GIT_BASH_PATH);
    process
        .args(["--noprofile", "--norc", "-c"])
        .arg(command)
        .env_remove("BASH_ENV");
    run(process, timeout_ms)
}

#[cfg(not(any(unix, windows)))]
pub fn sh_exec(_command: &str, _timeout_ms: u64) -> AgentResult<Value> {
    Err(AgentError::unsupported(
        "sh_exec requires /bin/sh or Git Bash on Windows",
    ))
}

pub fn exec(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
    timeout_ms: u64,
) -> AgentResult<Value> {
    if program.is_empty() {
        return Err(AgentError::invalid("program must not be empty"));
    }
    let mut process = Command::new(program);
    process.args(args).envs(env);
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }
    run(process, timeout_ms)
}

fn run(mut command: Command, timeout_ms: u64) -> AgentResult<Value> {
    let started = Instant::now();
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(AgentError::invalid(format!(
            "timeout_ms must be in range 0..={MAX_TIMEOUT_MS}"
        )));
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(unix)]
    set_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|err| AgentError::io("spawn command", err))?;
    #[cfg(windows)]
    let job = match WindowsJob::new().and_then(|job| {
        job.assign(&child)?;
        Ok(job)
    }) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut status = None;
    let timed_out = loop {
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|err| AgentError::io("wait for command", err))?;
        }
        if status.is_some() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            break false;
        }
        if Instant::now() >= deadline {
            #[cfg(windows)]
            job.terminate()?;
            #[cfg(not(windows))]
            terminate(&mut child)?;
            if status.is_none() {
                status = Some(
                    child
                        .wait()
                        .map_err(|err| AgentError::io("wait for terminated command", err))?,
                );
            }
            break true;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| AgentError::command("stdout reader panicked"))?;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| AgentError::command("stderr reader panicked"))?;
    let status = status.expect("command status available before readers are joined");
    Ok(json!({
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "exit_code": status.code(),
        "timed_out": timed_out,
        "duration_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated
    }))
}

fn read_bounded(mut reader: impl Read) -> (Vec<u8>, bool) {
    let mut output = Vec::with_capacity(8192);
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let remaining = OUTPUT_LIMIT.saturating_sub(output.len());
                output.extend_from_slice(&buffer[..read.min(remaining)]);
                if read > remaining {
                    truncated = true;
                }
            }
        }
    }
    (output, truncated)
}

#[cfg(unix)]
pub(super) fn set_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

#[cfg(unix)]
fn terminate(child: &mut std::process::Child) -> AgentResult<()> {
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn terminate(child: &mut std::process::Child) -> AgentResult<()> {
    child
        .kill()
        .map_err(|error| AgentError::io("terminate command", error))
}

#[cfg(windows)]
pub(super) struct WindowsJob(windows_sys::Win32::Foundation::HANDLE);

// Windows kernel handles may be used from any thread while the handle remains open.
#[cfg(windows)]
unsafe impl Send for WindowsJob {}
#[cfg(windows)]
unsafe impl Sync for WindowsJob {}

#[cfg(windows)]
impl WindowsJob {
    pub(super) fn new() -> AgentResult<Self> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(AgentError::io(
                "create command job object",
                std::io::Error::last_os_error(),
            ));
        }
        let job = Self(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(AgentError::io(
                "configure command job object",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(job)
    }

    pub(super) fn assign(&self, child: &std::process::Child) -> AgentResult<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let process = child.as_raw_handle().cast();
        if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
            return Err(AgentError::io(
                "assign command to job object",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    pub(super) fn terminate(&self) -> AgentResult<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        if unsafe { TerminateJobObject(self.0, 1) } == 0 {
            return Err(AgentError::io(
                "terminate command job object",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OUTPUT_LIMIT, read_bounded};

    #[test]
    fn bounded_reader_reports_truncation() {
        let input = vec![b'x'; OUTPUT_LIMIT + 1];
        let (output, truncated) = read_bounded(input.as_slice());
        assert_eq!(output.len(), OUTPUT_LIMIT);
        assert!(truncated);
    }

    #[cfg(windows)]
    mod windows {
        use std::collections::BTreeMap;
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::process::Command;
        use std::thread;
        use std::time::{Duration, Instant};

        use super::super::{GIT_BASH_PATH, exec, sh_exec};

        const HELPER_ROLE: &str = "REMOTE_OPS_COMMAND_TEST_ROLE";
        const HELPER_PID_FILE: &str = "REMOTE_OPS_COMMAND_TEST_PID_FILE";
        const HELPER_TEST: &str = "tools::command::tests::windows::timeout_process_tree_helper";

        #[test]
        fn successful_command_preserves_result_shape() {
            let args = vec![
                "/D".to_string(),
                "/S".to_string(),
                "/C".to_string(),
                "echo stdout-text & echo stderr-text 1>&2 & exit /b 7".to_string(),
            ];
            let result = exec("cmd.exe", &args, None, &BTreeMap::new(), 10_000).unwrap();

            assert!(result["stdout"].as_str().unwrap().contains("stdout-text"));
            assert!(result["stderr"].as_str().unwrap().contains("stderr-text"));
            assert_eq!(result["exit_code"], 7);
            assert_eq!(result["timed_out"], false);
            assert_eq!(result["stdout_truncated"], false);
            assert_eq!(result["stderr_truncated"], false);
        }

        #[test]
        fn git_bash_executes_shell_command_when_installed() {
            if !std::path::Path::new(GIT_BASH_PATH).is_file() {
                let error = sh_exec("exit 0", 10_000).unwrap_err();
                assert_eq!(error.kind, "unsupported");
                assert!(error.message.contains(GIT_BASH_PATH));
                return;
            }

            let result = sh_exec(
                "printf 'stdout-中文\n'; printf 'stderr-中文\n' >&2; exit 7",
                10_000,
            )
            .unwrap();

            assert_eq!(result["stdout"], "stdout-中文\n");
            assert_eq!(result["stderr"], "stderr-中文\n");
            assert_eq!(result["exit_code"], 7);
            assert_eq!(result["timed_out"], false);
            assert_eq!(result["stdout_truncated"], false);
            assert_eq!(result["stderr_truncated"], false);
        }

        #[test]
        fn timeout_terminates_descendants_holding_output_pipes() {
            let directory = tempfile::tempdir().unwrap();
            let pid_file = directory.path().join("pids.txt");
            let mut env = BTreeMap::new();
            env.insert(HELPER_ROLE.to_string(), "parent".to_string());
            env.insert(
                HELPER_PID_FILE.to_string(),
                pid_file.to_string_lossy().into_owned(),
            );
            let program = std::env::current_exe().unwrap();
            let args = vec!["--exact".to_string(), HELPER_TEST.to_string()];

            let started = Instant::now();
            let result = exec(program.to_str().unwrap(), &args, None, &env, 2_000).unwrap();

            assert_eq!(result["timed_out"], true);
            assert!(started.elapsed() < Duration::from_secs(10));
            assert!(
                result["stdout"]
                    .as_str()
                    .unwrap()
                    .contains("before-timeout-stdout")
            );
            assert!(
                result["stderr"]
                    .as_str()
                    .unwrap()
                    .contains("before-timeout-stderr")
            );

            let pids = std::fs::read_to_string(&pid_file)
                .unwrap()
                .lines()
                .map(|line| line.parse::<u32>().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(pids.len(), 2, "helper should record parent and grandchild");
            // Job Object termination is asynchronous on Windows; under load the
            // kernel may lag a moment before the processes fully disappear.
            let exited = Instant::now() + Duration::from_secs(10);
            for pid in pids {
                while process_is_running(pid) {
                    assert!(Instant::now() < exited, "process {pid} survived timeout");
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }

        #[test]
        fn timeout_process_tree_helper() {
            let Some(role) = std::env::var_os(HELPER_ROLE) else {
                return;
            };
            let pid_file = std::env::var_os(HELPER_PID_FILE).unwrap();
            if role == "grandchild" {
                let mut file = OpenOptions::new().append(true).open(pid_file).unwrap();
                writeln!(file, "{}", std::process::id()).unwrap();
                thread::sleep(Duration::from_secs(30));
                return;
            }

            std::fs::write(&pid_file, format!("{}\n", std::process::id())).unwrap();
            let mut grandchild = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", HELPER_TEST])
                .env(HELPER_ROLE, "grandchild")
                .env(HELPER_PID_FILE, &pid_file)
                .spawn()
                .unwrap();
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "before-timeout-stdout").unwrap();
            stdout.flush().unwrap();
            let mut stderr = std::io::stderr().lock();
            writeln!(stderr, "before-timeout-stderr").unwrap();
            stderr.flush().unwrap();

            let deadline = Instant::now() + Duration::from_secs(5);
            while std::fs::read_to_string(&pid_file)
                .map(|contents| contents.lines().count())
                .unwrap_or(0)
                < 2
            {
                assert!(Instant::now() < deadline, "grandchild did not start");
                thread::sleep(Duration::from_millis(10));
            }
            let _ = grandchild.wait();
        }

        fn process_is_running(pid: u32) -> bool {
            use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
            use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

            const SYNCHRONIZE: u32 = 0x0010_0000;
            let process = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
            if process.is_null() {
                return false;
            }
            let wait = unsafe { WaitForSingleObject(process, 0) };
            unsafe {
                CloseHandle(process);
            }
            wait == WAIT_TIMEOUT
        }
    }
}
