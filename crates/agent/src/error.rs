use std::fmt;

use remote_ops_protocol::RemoteError;

#[derive(Debug)]
pub struct AgentError {
    pub kind: &'static str,
    pub message: String,
}

impl AgentError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: "invalid_params",
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            kind: "unsupported",
            message: message.into(),
        }
    }

    pub fn io(context: impl fmt::Display, error: std::io::Error) -> Self {
        Self {
            kind: "io",
            message: format!("{context}: {error}"),
        }
    }

    pub fn command(message: impl Into<String>) -> Self {
        Self {
            kind: "command",
            message: message.into(),
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AgentError {}

impl From<AgentError> for RemoteError {
    fn from(value: AgentError) -> Self {
        Self {
            kind: value.kind.to_string(),
            message: value.message,
        }
    }
}

pub type AgentResult<T> = Result<T, AgentError>;
