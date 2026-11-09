//! Shared argument decoding and validation for basic file and process tools.
//! Filesystem type/permission checks still belong to the Agent implementation.

use serde::Deserialize;

pub const READ_TEXT_MAX_BYTES: usize = 1024 * 1024;
pub const TAIL_TEXT_MAX_BYTES: usize = 1024 * 1024;
pub const FILE_HASH_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_TAIL_LINES: usize = 10_000;
pub const MAX_PID_RESULTS: usize = 1024;
pub const MAX_PROCESS_IDS: usize = 65_536;
pub const MAX_PROCESS_CMDLINE_BYTES: usize = 128 * 1024;
pub const MAX_PROCESS_SCAN_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PROCESS_SCAN_COUNT: usize = 4096;
pub const MAX_PROCESS_PAGE_BYTES: usize = 1024 * 1024;

pub trait ValidateArguments {
    fn validate(&self) -> Result<(), String>;
}

pub fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.contains('\0') {
        Err("path must be non-empty without NUL".into())
    } else {
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadTextArgs {
    pub path: String,
    #[serde(default)]
    pub offset: u64,
    #[serde(default = "default_read_bytes")]
    pub max_bytes: usize,
}

impl ValidateArguments for ReadTextArgs {
    fn validate(&self) -> Result<(), String> {
        validate_path(&self.path)?;
        if self.max_bytes > READ_TEXT_MAX_BYTES {
            return Err(format!(
                "max_bytes must be in range 0..={READ_TEXT_MAX_BYTES}"
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TailTextArgs {
    pub path: String,
    #[serde(default = "default_tail_lines")]
    pub lines: usize,
    #[serde(default = "default_tail_bytes")]
    pub max_bytes: usize,
}

impl ValidateArguments for TailTextArgs {
    fn validate(&self) -> Result<(), String> {
        validate_path(&self.path)?;
        if self.lines > MAX_TAIL_LINES || self.max_bytes > TAIL_TEXT_MAX_BYTES {
            return Err(format!(
                "lines must be in range 0..={MAX_TAIL_LINES} and max_bytes in 0..={TAIL_TEXT_MAX_BYTES}"
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteTextArgs {
    pub path: String,
    pub content: String,
}

impl ValidateArguments for WriteTextArgs {
    fn validate(&self) -> Result<(), String> {
        validate_path(&self.path)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathArgs {
    pub path: String,
}

impl ValidateArguments for PathArgs {
    fn validate(&self) -> Result<(), String> {
        validate_path(&self.path)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HashArgs {
    pub path: String,
    #[serde(default = "default_hash_bytes")]
    pub max_bytes: u64,
}

impl ValidateArguments for HashArgs {
    fn validate(&self) -> Result<(), String> {
        validate_path(&self.path)?;
        if self.max_bytes > FILE_HASH_MAX_BYTES {
            return Err(format!(
                "max_bytes must be in range 0..={FILE_HASH_MAX_BYTES}"
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PidsArgs {
    pub filter: Option<String>,
    pub cursor: Option<String>,
    #[serde(default = "default_pid_limit")]
    pub limit: usize,
}

impl ValidateArguments for PidsArgs {
    fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_PID_RESULTS).contains(&self.limit) {
            return Err(format!("limit must be in range 1..={MAX_PID_RESULTS}"));
        }
        if let Some(cursor) = &self.cursor
            && cursor.parse::<u32>().ok().filter(|pid| *pid > 0).is_none()
        {
            return Err("cursor must be a positive PID".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PidArgs {
    pub pid: i32,
}

impl ValidateArguments for PidArgs {
    fn validate(&self) -> Result<(), String> {
        if self.pid <= 0 {
            Err("pid must be greater than zero".into())
        } else {
            Ok(())
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KillArgs {
    pub pid: i32,
    #[serde(default = "default_signal")]
    pub signal: i32,
}

impl ValidateArguments for KillArgs {
    fn validate(&self) -> Result<(), String> {
        PidArgs { pid: self.pid }.validate()?;
        if !(1..=64).contains(&self.signal) {
            return Err("signal must be in range 1..=64".into());
        }
        Ok(())
    }
}

fn default_read_bytes() -> usize {
    READ_TEXT_MAX_BYTES
}
fn default_tail_bytes() -> usize {
    256 * 1024
}
fn default_tail_lines() -> usize {
    100
}
fn default_hash_bytes() -> u64 {
    FILE_HASH_MAX_BYTES
}
fn default_pid_limit() -> usize {
    MAX_PID_RESULTS
}
fn default_signal() -> i32 {
    15
}
