use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use remote_ops_protocol::{
    MAX_PROCESS_JOB_TIMEOUT_MS, MAX_PROCESS_JOBS, MAX_PROCESS_OUTPUT_BYTES, MAX_PROCESS_WAIT_MS,
    PROCESS_OUTPUT_BUFFER_BYTES,
};
use serde_json::{Map, Value, json};

use crate::error::{AgentError, AgentResult};

#[cfg(windows)]
use super::command::WindowsJob;
#[cfg(unix)]
use super::command::set_process_group;

pub struct JobManager {
    inner: Mutex<ManagerData>,
}

struct ManagerData {
    jobs: HashMap<u64, Arc<Job>>,
    next_job_id: u64,
}

struct Job {
    id: u64,
    pid: u32,
    started_at_ms: u64,
    timeout_ms: u64,
    control: JobControl,
    data: Mutex<JobData>,
    changed: Condvar,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct JobData {
    state: JobState,
    exit_code: Option<i32>,
    timed_out: bool,
    error: Option<String>,
    finished_at_ms: Option<u64>,
    stdout: OutputBuffer,
    stderr: OutputBuffer,
    stdout_complete: bool,
    stderr_complete: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
    Exited,
    Failed,
}

struct OutputBuffer {
    bytes: VecDeque<u8>,
    start_cursor: u64,
    end_cursor: u64,
}

struct OutputSlice {
    bytes: Vec<u8>,
    start_cursor: u64,
    next_cursor: u64,
    truncated: bool,
}

impl JobManager {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(ManagerData {
                jobs: HashMap::new(),
                next_job_id: 1,
            }),
        }
    }

    fn start(&self, mut command: Command, timeout_ms: u64) -> AgentResult<Value> {
        validate_job_timeout(timeout_ms)?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        set_process_group(&mut command);

        let mut manager = lock(&self.inner);
        evict_completed_job_if_full(&mut manager);
        if manager.jobs.len() >= MAX_PROCESS_JOBS {
            return Err(AgentError::command(format!(
                "background job limit reached ({MAX_PROCESS_JOBS})"
            )));
        }
        let job_id = manager.next_job_id;
        manager.next_job_id = manager
            .next_job_id
            .checked_add(1)
            .ok_or_else(|| AgentError::command("background job ID space exhausted"))?;

        let mut child = command
            .spawn()
            .map_err(|error| AgentError::io("spawn background process", error))?;
        let control = match JobControl::new(&child) {
            Ok(control) => control,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let job = Arc::new(Job {
            id: job_id,
            pid: child.id(),
            started_at_ms: unix_time_ms(),
            timeout_ms,
            control,
            data: Mutex::new(JobData::new()),
            changed: Condvar::new(),
            worker: Mutex::new(None),
        });
        let worker_job = Arc::clone(&job);
        let worker = thread::spawn(move || monitor_job(worker_job, child, stdout, stderr));
        *lock(&job.worker) = Some(worker);
        manager.jobs.insert(job_id, Arc::clone(&job));
        drop(manager);

        Ok(Value::Object(status_object(&job, &lock(&job.data))))
    }

    fn get(&self, job_id: u64) -> AgentResult<Arc<Job>> {
        if job_id == 0 {
            return Err(AgentError::invalid("job_id must be at least 1"));
        }
        lock(&self.inner)
            .jobs
            .get(&job_id)
            .cloned()
            .ok_or_else(|| AgentError::invalid(format!("unknown background job: {job_id}")))
    }

    fn close(&self, job_id: u64) -> AgentResult<Value> {
        if job_id == 0 {
            return Err(AgentError::invalid("job_id must be at least 1"));
        }
        let job = {
            let mut manager = lock(&self.inner);
            let job =
                manager.jobs.get(&job_id).cloned().ok_or_else(|| {
                    AgentError::invalid(format!("unknown background job: {job_id}"))
                })?;
            if lock(&job.data).state == JobState::Running {
                return Err(AgentError::command(
                    "background job is still running; signal and wait for it before closing",
                ));
            }
            manager.jobs.remove(&job_id).expect("job exists")
        };
        job.join_worker();
        Ok(json!({"job_id": job_id, "closed": true}))
    }
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for JobManager {
    fn drop(&mut self) {
        let manager = self
            .inner
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let jobs: Vec<_> = manager.jobs.drain().map(|(_, job)| job).collect();
        for job in &jobs {
            if lock(&job.data).state == JobState::Running {
                let _ = job.control.terminate();
            }
        }
        for job in jobs {
            job.join_worker();
        }
    }
}

impl Job {
    fn append_output(&self, stdout: bool, bytes: &[u8]) {
        let mut data = lock(&self.data);
        if stdout {
            data.stdout.append(bytes);
        } else {
            data.stderr.append(bytes);
        }
        self.changed.notify_all();
    }

    fn complete_output(&self, stdout: bool) {
        let mut data = lock(&self.data);
        if stdout {
            data.stdout_complete = true;
        } else {
            data.stderr_complete = true;
        }
        self.changed.notify_all();
    }

    fn finish(&self, status: Option<ExitStatus>, timed_out: bool, error: Option<String>) {
        let mut data = lock(&self.data);
        data.state = if error.is_some() {
            JobState::Failed
        } else {
            JobState::Exited
        };
        data.exit_code = status.and_then(|status| status.code());
        data.timed_out = timed_out;
        data.error = error;
        data.finished_at_ms = Some(unix_time_ms());
        self.changed.notify_all();
    }

    fn join_worker(&self) {
        if let Some(worker) = lock(&self.worker).take() {
            let _ = worker.join();
        }
    }
}

impl JobData {
    fn new() -> Self {
        Self {
            state: JobState::Running,
            exit_code: None,
            timed_out: false,
            error: None,
            finished_at_ms: None,
            stdout: OutputBuffer::new(),
            stderr: OutputBuffer::new(),
            stdout_complete: false,
            stderr_complete: false,
        }
    }
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Failed => "failed",
        }
    }
}

impl OutputBuffer {
    fn new() -> Self {
        Self {
            bytes: VecDeque::new(),
            start_cursor: 0,
            end_cursor: 0,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        self.end_cursor = self.end_cursor.saturating_add(bytes.len() as u64);
        self.bytes.extend(bytes.iter().copied());
        while self.bytes.len() > PROCESS_OUTPUT_BUFFER_BYTES {
            self.bytes.pop_front();
            self.start_cursor = self.start_cursor.saturating_add(1);
        }
    }

    fn read(&self, cursor: u64, max_bytes: usize, stream: &str) -> AgentResult<OutputSlice> {
        if cursor > self.end_cursor {
            return Err(AgentError::invalid(format!(
                "{stream}_cursor exceeds the current output cursor"
            )));
        }
        let truncated = cursor < self.start_cursor;
        let actual_cursor = cursor.max(self.start_cursor);
        let skip = (actual_cursor - self.start_cursor) as usize;
        let bytes = self
            .bytes
            .iter()
            .skip(skip)
            .take(max_bytes)
            .copied()
            .collect::<Vec<_>>();
        let next_cursor = actual_cursor + bytes.len() as u64;
        Ok(OutputSlice {
            bytes,
            start_cursor: self.start_cursor,
            next_cursor,
            truncated,
        })
    }
}

pub fn process_start(
    manager: &JobManager,
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
    timeout_ms: u64,
) -> AgentResult<Value> {
    validate_process_start(program, args, cwd, env)?;
    let mut command = Command::new(program);
    command.args(args).envs(env);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    manager.start(command, timeout_ms)
}

pub fn process_output(
    manager: &JobManager,
    job_id: u64,
    stdout_cursor: u64,
    stderr_cursor: u64,
    max_bytes: usize,
) -> AgentResult<Value> {
    if max_bytes > MAX_PROCESS_OUTPUT_BYTES {
        return Err(AgentError::invalid(format!(
            "max_bytes must be in range 0..={MAX_PROCESS_OUTPUT_BYTES}"
        )));
    }
    let job = manager.get(job_id)?;
    let data = lock(&job.data);
    let stdout = data.stdout.read(stdout_cursor, max_bytes, "stdout")?;
    let stderr = data.stderr.read(stderr_cursor, max_bytes, "stderr")?;
    let mut result = status_object(&job, &data);
    result.insert(
        "stdout".to_string(),
        json!(String::from_utf8_lossy(&stdout.bytes)),
    );
    result.insert(
        "stderr".to_string(),
        json!(String::from_utf8_lossy(&stderr.bytes)),
    );
    result.insert(
        "stdout_start_cursor".to_string(),
        json!(stdout.start_cursor),
    );
    result.insert(
        "stderr_start_cursor".to_string(),
        json!(stderr.start_cursor),
    );
    result.insert("next_stdout_cursor".to_string(), json!(stdout.next_cursor));
    result.insert("next_stderr_cursor".to_string(), json!(stderr.next_cursor));
    result.insert("stdout_truncated".to_string(), json!(stdout.truncated));
    result.insert("stderr_truncated".to_string(), json!(stderr.truncated));
    Ok(Value::Object(result))
}

pub fn process_wait(manager: &JobManager, job_id: u64, wait_ms: u64) -> AgentResult<Value> {
    if wait_ms > MAX_PROCESS_WAIT_MS {
        return Err(AgentError::invalid(format!(
            "wait_ms must be in range 0..={MAX_PROCESS_WAIT_MS}"
        )));
    }
    let job = manager.get(job_id)?;
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    let mut data = lock(&job.data);
    while data.state == JobState::Running {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(now);
        let result = job.changed.wait_timeout(data, remaining);
        data = match result {
            Ok((data, _)) => data,
            Err(poisoned) => poisoned.into_inner().0,
        };
    }
    Ok(Value::Object(status_object(&job, &data)))
}

pub fn process_signal(manager: &JobManager, job_id: u64, signal: i32) -> AgentResult<Value> {
    if !(1..=64).contains(&signal) {
        return Err(AgentError::invalid("signal must be in range 1..=64"));
    }
    let job = manager.get(job_id)?;
    if lock(&job.data).state != JobState::Running {
        return Err(AgentError::command("background job has already finished"));
    }
    job.control.signal(signal)?;
    let data = lock(&job.data);
    let mut result = status_object(&job, &data);
    result.insert("signal".to_string(), json!(signal));
    Ok(Value::Object(result))
}

pub fn process_close(manager: &JobManager, job_id: u64) -> AgentResult<Value> {
    manager.close(job_id)
}

fn validate_process_start(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
) -> AgentResult<()> {
    if program.is_empty() {
        return Err(AgentError::invalid("program must not be empty"));
    }
    if program.contains('\0') {
        return Err(AgentError::invalid("program must not contain NUL"));
    }
    if args.iter().any(|arg| arg.contains('\0')) {
        return Err(AgentError::invalid("args must not contain NUL"));
    }
    if let Some(cwd) = cwd {
        if cwd.is_empty() {
            return Err(AgentError::invalid("cwd must not be empty"));
        }
        if cwd.contains('\0') {
            return Err(AgentError::invalid("cwd must not contain NUL"));
        }
    }
    if env
        .iter()
        .any(|(name, value)| name.is_empty() || name.contains(['\0', '=']) || value.contains('\0'))
    {
        return Err(AgentError::invalid(
            "environment names must be non-empty without NUL or '=' and values must not contain NUL",
        ));
    }
    Ok(())
}

fn validate_job_timeout(timeout_ms: u64) -> AgentResult<()> {
    if timeout_ms == 0 || timeout_ms > MAX_PROCESS_JOB_TIMEOUT_MS {
        Err(AgentError::invalid(format!(
            "timeout_ms must be in range 1..={MAX_PROCESS_JOB_TIMEOUT_MS}"
        )))
    } else {
        Ok(())
    }
}

fn monitor_job(
    job: Arc<Job>,
    mut child: Child,
    stdout: impl Read + Send + 'static,
    stderr: impl Read + Send + 'static,
) {
    let stdout_job = Arc::clone(&job);
    let stdout_reader = thread::spawn(move || read_output(stdout_job, stdout, true));
    let stderr_job = Arc::clone(&job);
    let stderr_reader = thread::spawn(move || read_output(stderr_job, stderr, false));
    let deadline = Instant::now() + Duration::from_millis(job.timeout_ms);
    let mut status = None;
    let mut timed_out = false;
    let mut error = None;

    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(value) => status = value,
                Err(wait_error) => {
                    append_error(
                        &mut error,
                        format!("wait for background process: {wait_error}"),
                    );
                    let _ = terminate_background_process(&job.control, &mut child);
                    status = child.wait().ok();
                    break;
                }
            }
        }
        if status.is_some() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            if let Err(terminate_error) = terminate_background_process(&job.control, &mut child) {
                append_error(&mut error, terminate_error.to_string());
            }
            if status.is_none() {
                match child.wait() {
                    Ok(exit_status) => status = Some(exit_status),
                    Err(wait_error) => append_error(
                        &mut error,
                        format!("wait for terminated background process: {wait_error}"),
                    ),
                }
            }
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    match stdout_reader.join() {
        Ok(Ok(())) => {}
        Ok(Err(reader_error)) => append_error(&mut error, reader_error),
        Err(_) => append_error(&mut error, "background stdout reader panicked".to_string()),
    }
    match stderr_reader.join() {
        Ok(Ok(())) => {}
        Ok(Err(reader_error)) => append_error(&mut error, reader_error),
        Err(_) => append_error(&mut error, "background stderr reader panicked".to_string()),
    }
    job.finish(status, timed_out, error);
}

fn read_output(job: Arc<Job>, mut reader: impl Read, stdout: bool) -> Result<(), String> {
    let mut buffer = [0u8; 8192];
    let result = loop {
        match reader.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => job.append_output(stdout, &buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => break Err(format!("read background process output: {error}")),
        }
    };
    job.complete_output(stdout);
    result
}

fn terminate_background_process(control: &JobControl, child: &mut Child) -> AgentResult<()> {
    #[cfg(any(unix, windows))]
    {
        if let Err(control_error) = control.terminate() {
            child.kill().map_err(|child_error| {
                AgentError::command(format!(
                    "terminate background process: {control_error}; fallback failed: {child_error}"
                ))
            })?;
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = control;
        child
            .kill()
            .map_err(|error| AgentError::io("terminate background process", error))
    }
}

fn status_object(job: &Job, data: &JobData) -> Map<String, Value> {
    let mut result = Map::new();
    result.insert("job_id".to_string(), json!(job.id));
    result.insert("pid".to_string(), json!(job.pid));
    result.insert("state".to_string(), json!(data.state.as_str()));
    result.insert("exit_code".to_string(), json!(data.exit_code));
    result.insert("timed_out".to_string(), json!(data.timed_out));
    result.insert("error".to_string(), json!(data.error));
    result.insert("started_at_ms".to_string(), json!(job.started_at_ms));
    result.insert("finished_at_ms".to_string(), json!(data.finished_at_ms));
    result.insert("timeout_ms".to_string(), json!(job.timeout_ms));
    result.insert("stdout_complete".to_string(), json!(data.stdout_complete));
    result.insert("stderr_complete".to_string(), json!(data.stderr_complete));
    result
}

fn evict_completed_job_if_full(manager: &mut ManagerData) {
    if manager.jobs.len() < MAX_PROCESS_JOBS {
        return;
    }
    let oldest = manager
        .jobs
        .iter()
        .filter_map(|(job_id, job)| {
            let data = lock(&job.data);
            (data.state != JobState::Running)
                .then_some((*job_id, data.finished_at_ms.unwrap_or(u64::MAX)))
        })
        .min_by_key(|(_, finished_at_ms)| *finished_at_ms)
        .map(|(job_id, _)| job_id);
    if let Some(job_id) = oldest {
        manager.jobs.remove(&job_id);
    }
}

fn append_error(error: &mut Option<String>, addition: String) {
    match error {
        Some(error) => {
            error.push_str("; ");
            error.push_str(&addition);
        }
        None => *error = Some(addition),
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(unix)]
#[derive(Clone)]
struct JobControl {
    process_group: libc::pid_t,
}

#[cfg(unix)]
impl JobControl {
    fn new(child: &Child) -> AgentResult<Self> {
        Ok(Self {
            process_group: child.id() as libc::pid_t,
        })
    }

    fn signal(&self, signal: i32) -> AgentResult<()> {
        if unsafe { libc::kill(-self.process_group, signal) } == 0 {
            Ok(())
        } else {
            Err(AgentError::io(
                "signal background process group",
                std::io::Error::last_os_error(),
            ))
        }
    }

    fn terminate(&self) -> AgentResult<()> {
        if unsafe { libc::kill(-self.process_group, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(AgentError::io("terminate background process group", error))
        }
    }
}

#[cfg(windows)]
#[derive(Clone)]
struct JobControl {
    job: Arc<WindowsJob>,
}

#[cfg(windows)]
impl JobControl {
    fn new(child: &Child) -> AgentResult<Self> {
        let job = Arc::new(WindowsJob::new()?);
        job.assign(child)?;
        Ok(Self { job })
    }

    fn signal(&self, signal: i32) -> AgentResult<()> {
        if !matches!(signal, 9 | 15) {
            return Err(AgentError::invalid("signal must be 9 or 15 on Windows"));
        }
        self.job.terminate()
    }

    fn terminate(&self) -> AgentResult<()> {
        self.job.terminate()
    }
}

#[cfg(not(any(unix, windows)))]
#[derive(Clone)]
struct JobControl;

#[cfg(not(any(unix, windows)))]
impl JobControl {
    fn new(_child: &Child) -> AgentResult<Self> {
        Ok(Self)
    }

    fn signal(&self, _signal: i32) -> AgentResult<()> {
        Err(AgentError::unsupported(
            "process_signal requires Unix or Windows",
        ))
    }

    fn terminate(&self) -> AgentResult<()> {
        Err(AgentError::unsupported(
            "background process groups require Unix or Windows",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use remote_ops_protocol::{DEFAULT_PROCESS_OUTPUT_BYTES, DEFAULT_PROCESS_WAIT_MS};

    use super::*;

    const HELPER_ROLE: &str = "REMOTE_OPS_JOB_TEST_ROLE";
    const HELPER_TEST: &str = "tools::jobs::tests::background_process_helper";

    #[test]
    fn background_output_is_incremental_and_cursor_based() {
        let manager = JobManager::new();
        let program = std::env::current_exe().unwrap();
        let args = vec![
            "--exact".to_string(),
            HELPER_TEST.to_string(),
            "--nocapture".to_string(),
        ];
        let mut env = BTreeMap::new();
        env.insert(HELPER_ROLE.to_string(), "run".to_string());
        let started = process_start(
            &manager,
            program.to_str().unwrap(),
            &args,
            None,
            &env,
            5_000,
        )
        .unwrap();
        let job_id = started["job_id"].as_u64().unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let first = loop {
            let output =
                process_output(&manager, job_id, 0, 0, DEFAULT_PROCESS_OUTPUT_BYTES).unwrap();
            if output["stdout"].as_str().unwrap().contains("job-first") {
                break output;
            }
            assert!(Instant::now() < deadline, "first output was not observed");
            thread::sleep(Duration::from_millis(10));
        };
        let stdout_cursor = first["next_stdout_cursor"].as_u64().unwrap();
        let stderr_cursor = first["next_stderr_cursor"].as_u64().unwrap();
        let finished = process_wait(&manager, job_id, DEFAULT_PROCESS_WAIT_MS).unwrap();
        assert_eq!(finished["state"], "exited");
        assert_eq!(finished["exit_code"], 7);

        let second = process_output(
            &manager,
            job_id,
            stdout_cursor,
            stderr_cursor,
            DEFAULT_PROCESS_OUTPUT_BYTES,
        )
        .unwrap();
        assert!(second["stdout"].as_str().unwrap().contains("job-second"));
        assert!(second["stderr"].as_str().unwrap().contains("job-error"));
        assert_eq!(second["stdout_truncated"], false);
        assert_eq!(process_close(&manager, job_id).unwrap()["closed"], true);
    }

    #[test]
    fn running_job_must_be_signaled_before_close() {
        let manager = JobManager::new();
        let program = std::env::current_exe().unwrap();
        let args = vec![
            "--exact".to_string(),
            HELPER_TEST.to_string(),
            "--nocapture".to_string(),
        ];
        let mut env = BTreeMap::new();
        env.insert(HELPER_ROLE.to_string(), "sleep".to_string());
        let started = process_start(
            &manager,
            program.to_str().unwrap(),
            &args,
            None,
            &env,
            5_000,
        )
        .unwrap();
        let job_id = started["job_id"].as_u64().unwrap();
        assert_eq!(process_close(&manager, job_id).unwrap_err().kind, "command");
        process_signal(&manager, job_id, 9).unwrap();
        assert_ne!(
            process_wait(&manager, job_id, DEFAULT_PROCESS_WAIT_MS).unwrap()["state"],
            "running"
        );
        process_close(&manager, job_id).unwrap();
    }

    #[test]
    fn background_job_timeout_sets_status_and_terminates_process() {
        let manager = JobManager::new();
        let program = std::env::current_exe().unwrap();
        let args = vec![
            "--exact".to_string(),
            HELPER_TEST.to_string(),
            "--nocapture".to_string(),
        ];
        let mut env = BTreeMap::new();
        env.insert(HELPER_ROLE.to_string(), "sleep".to_string());
        let started =
            process_start(&manager, program.to_str().unwrap(), &args, None, &env, 250).unwrap();
        let job_id = started["job_id"].as_u64().unwrap();
        let finished = process_wait(&manager, job_id, DEFAULT_PROCESS_WAIT_MS).unwrap();
        assert_eq!(finished["state"], "exited");
        assert_eq!(finished["timed_out"], true);
        process_close(&manager, job_id).unwrap();
    }

    #[test]
    fn dropping_manager_terminates_running_jobs() {
        let manager = JobManager::new();
        let program = std::env::current_exe().unwrap();
        let args = vec![
            "--exact".to_string(),
            HELPER_TEST.to_string(),
            "--nocapture".to_string(),
        ];
        let mut env = BTreeMap::new();
        env.insert(HELPER_ROLE.to_string(), "sleep".to_string());
        process_start(
            &manager,
            program.to_str().unwrap(),
            &args,
            None,
            &env,
            5_000,
        )
        .unwrap();

        let started = Instant::now();
        drop(manager);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn output_buffer_reports_evicted_bytes() {
        let mut output = OutputBuffer::new();
        output.append(&vec![b'x'; PROCESS_OUTPUT_BUFFER_BYTES + 7]);
        let slice = output.read(0, 16, "stdout").unwrap();
        assert!(slice.truncated);
        assert_eq!(slice.start_cursor, 7);
        assert_eq!(slice.next_cursor, 23);
        assert_eq!(slice.bytes.len(), 16);
    }

    #[test]
    fn background_process_helper() {
        let Some(role) = std::env::var_os(HELPER_ROLE) else {
            return;
        };
        if role == "sleep" {
            thread::sleep(Duration::from_secs(30));
            return;
        }
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "job-first").unwrap();
        stdout.flush().unwrap();
        thread::sleep(Duration::from_millis(150));
        writeln!(stdout, "job-second").unwrap();
        stdout.flush().unwrap();
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "job-error").unwrap();
        stderr.flush().unwrap();
        std::process::exit(7);
    }
}
