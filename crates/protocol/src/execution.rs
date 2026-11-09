use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 10_000;
pub const MAX_COMMAND_TIMEOUT_MS: u64 = 300_000;
pub const MAX_SHELL_COMMAND_BYTES: usize = 256 * 1024;
pub const MAX_CMD_COMMAND_UTF16: usize = 8000;
pub const MAX_SHELL_ENV_ENTRIES: usize = 128;
pub const MAX_SHELL_ENV_BYTES: usize = 64 * 1024;
pub const MAX_EXECUTION_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    #[default]
    Native,
    Sh,
    Bash,
    Busybox,
    Pwsh,
    Powershell,
    Cmd,
    GitBash,
}

impl ShellKind {
    pub const ALL: [Self; 8] = [
        Self::Native,
        Self::Sh,
        Self::Bash,
        Self::Busybox,
        Self::Pwsh,
        Self::Powershell,
        Self::Cmd,
        Self::GitBash,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Sh => "sh",
            Self::Bash => "bash",
            Self::Busybox => "busybox",
            Self::Pwsh => "pwsh",
            Self::Powershell => "powershell",
            Self::Cmd => "cmd",
            Self::GitBash => "git_bash",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShellArgs {
    pub command: String,
    #[serde(default)]
    pub shell: ShellKind,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_command_timeout")]
    pub timeout_ms: u64,
}

fn default_command_timeout() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_MS
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecArgs {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_command_timeout")]
    pub timeout_ms: u64,
}

impl crate::arguments::ValidateArguments for ExecArgs {
    fn validate(&self) -> Result<(), String> {
        validate_exec(
            &self.program,
            &self.args,
            self.cwd.as_deref(),
            &self.env,
            self.timeout_ms,
        )
    }
}

pub fn validate_exec(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
    timeout_ms: u64,
) -> Result<(), String> {
    validate_program(program, args, cwd, env)?;
    if timeout_ms > MAX_COMMAND_TIMEOUT_MS {
        return Err(format!(
            "timeout_ms must be in range 0..={MAX_COMMAND_TIMEOUT_MS}"
        ));
    }
    Ok(())
}

pub fn validate_program(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if program.is_empty() || program.contains('\0') {
        return Err("program must be non-empty without NUL".into());
    }
    if args.iter().any(|arg| arg.contains('\0')) {
        return Err("args must not contain NUL".into());
    }
    if cwd.is_some_and(|cwd| cwd.is_empty() || cwd.contains('\0')) {
        return Err("cwd must be non-empty without NUL".into());
    }
    if env
        .iter()
        .any(|(name, value)| name.is_empty() || name.contains(['\0', '=']) || value.contains('\0'))
    {
        return Err("environment names must be non-empty without NUL or '=' and values must not contain NUL".into());
    }
    Ok(())
}

impl ShellArgs {
    pub fn validate(&self) -> Result<(), String> {
        if self.command.len() > MAX_SHELL_COMMAND_BYTES || self.command.contains('\0') {
            return Err(format!(
                "command must be at most {MAX_SHELL_COMMAND_BYTES} UTF-8 bytes without NUL"
            ));
        }
        if self.shell == ShellKind::Cmd
            && self.command.encode_utf16().count() > MAX_CMD_COMMAND_UTF16
        {
            return Err(format!(
                "cmd command must be at most {MAX_CMD_COMMAND_UTF16} UTF-16 units; use PowerShell for larger scripts"
            ));
        }
        if self.timeout_ms > MAX_COMMAND_TIMEOUT_MS {
            return Err(format!(
                "timeout_ms must be in range 0..={MAX_COMMAND_TIMEOUT_MS}"
            ));
        }
        if let Some(cwd) = &self.cwd
            && (cwd.is_empty() || cwd.len() > MAX_EXECUTION_PATH_BYTES || cwd.contains('\0'))
        {
            return Err(format!(
                "cwd must contain 1..={MAX_EXECUTION_PATH_BYTES} UTF-8 bytes without NUL"
            ));
        }
        if self.env.len() > MAX_SHELL_ENV_ENTRIES
            || self
                .env
                .iter()
                .map(|(name, value)| name.len() + value.len())
                .sum::<usize>()
                > MAX_SHELL_ENV_BYTES
        {
            return Err(format!(
                "env must have at most {MAX_SHELL_ENV_ENTRIES} entries and {MAX_SHELL_ENV_BYTES} UTF-8 bytes"
            ));
        }
        if self.env.iter().any(|(name, value)| {
            name.is_empty() || name.contains(['\0', '=']) || value.contains('\0')
        }) {
            return Err("environment names must be non-empty without NUL or '=' and values must not contain NUL".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shell_contract_has_bounded_input_and_rejects_ambiguous_selectors() {
        let defaults: ShellArgs = serde_json::from_value(json!({"command":"echo ok"})).unwrap();
        assert_eq!(defaults.shell, ShellKind::Native);
        assert_eq!(defaults.timeout_ms, DEFAULT_COMMAND_TIMEOUT_MS);
        for bad in [
            json!({"command":"bad\u{0}script"}),
            json!({"command":"x".repeat(MAX_SHELL_COMMAND_BYTES+1)}),
            json!({"command":"echo ok","cwd":""}),
            json!({"command":"echo ok","env":{"A=B":"x"}}),
            json!({"command":"echo ok","env":{"X":"x".repeat(MAX_SHELL_ENV_BYTES)}}),
            json!({"command":"echo ok","timeout_ms":MAX_COMMAND_TIMEOUT_MS+1}),
            json!({"command":"x".repeat(MAX_CMD_COMMAND_UTF16+1),"shell":"cmd"}),
        ] {
            assert!(
                serde_json::from_value::<ShellArgs>(bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        for bad in [
            json!({"command":"ok","shell":"auto"}),
            json!({"command":"ok","shell":null}),
            json!({"command":"ok","extra":true}),
        ] {
            assert!(serde_json::from_value::<ShellArgs>(bad).is_err());
        }
        for shell in ShellKind::ALL {
            assert_eq!(serde_json::to_value(shell).unwrap(), shell.name());
        }
    }
}
