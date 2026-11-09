use remote_ops_protocol::RemoteError;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Proxy,
    Transport,
    Protocol,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    NotSent,
    Unknown,
    RemoteError,
    Rejected,
    ResponseLimited,
    LocalError,
}

#[derive(Debug)]
pub struct ClientError {
    pub kind: String,
    pub message: String,
    source: Source,
    outcome: Outcome,
    request_started: bool,
    disconnect_observed: bool,
    prior_requests: u64,
    recovery: Option<Value>,
}

impl ClientError {
    pub(crate) fn local(kind: &str, message: impl Into<String>) -> Self {
        if kind == "protocol" {
            return Self::protocol(message);
        }
        Self {
            kind: kind.into(),
            message: message.into(),
            source: Source::Proxy,
            outcome: Outcome::NotSent,
            request_started: false,
            disconnect_observed: false,
            prior_requests: 0,
            recovery: None,
        }
    }

    pub(super) fn transport(kind: &str, message: impl Into<String>, started: bool) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            source: Source::Transport,
            outcome: if started {
                Outcome::Unknown
            } else {
                Outcome::NotSent
            },
            request_started: started,
            disconnect_observed: false,
            prior_requests: 0,
            recovery: None,
        }
    }

    pub(super) fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: "protocol".into(),
            message: message.into(),
            source: Source::Protocol,
            outcome: Outcome::Unknown,
            request_started: true,
            disconnect_observed: false,
            prior_requests: 0,
            recovery: None,
        }
    }

    pub(super) fn during_request(mut self) -> Self {
        self.request_started = true;
        if self.outcome == Outcome::NotSent {
            self.outcome = Outcome::Unknown;
        }
        self
    }

    pub(super) fn with_disconnect(mut self, observed: bool) -> Self {
        self.disconnect_observed = observed;
        self
    }

    pub(super) fn after_response(mut self) -> Self {
        self.request_started = true;
        self.outcome = Outcome::LocalError;
        self
    }

    pub(crate) fn response_limited(message: impl Into<String>) -> Self {
        Self {
            kind: "response_too_large".into(),
            message: message.into(),
            source: Source::Proxy,
            outcome: Outcome::ResponseLimited,
            request_started: true,
            disconnect_observed: false,
            prior_requests: 0,
            recovery: None,
        }
    }

    pub(crate) fn with_request_started(mut self, started: bool) -> Self {
        self.request_started = started;
        self
    }

    pub(crate) fn with_recovery(mut self, recovery: Value) -> Self {
        if let Some(existing) = self.recovery.as_mut() {
            if let (Some(object), Some(fields)) = (existing.as_object_mut(), recovery.as_object()) {
                object.extend(fields.clone());
            }
        } else {
            self.recovery = Some(recovery);
        }
        self
    }

    pub(crate) fn halts_workflow(&self) -> bool {
        matches!(self.source, Source::Transport | Source::Protocol)
            || matches!(
                self.outcome,
                Outcome::Unknown | Outcome::ResponseLimited | Outcome::Rejected
            )
    }

    pub(crate) fn is_preflight_error(&self) -> bool {
        self.kind == "invalid_params" && !self.request_started && self.source == Source::Proxy
    }

    pub(crate) fn workflow_context(mut self, attempts: u64) -> Self {
        self.prior_requests = attempts.saturating_sub(1);
        if attempts > 0 {
            self = self.during_request();
        }
        self
    }

    pub(super) fn reusable_reply(&self) -> bool {
        self.source == Source::Remote && self.outcome != Outcome::Rejected
    }

    pub(super) fn disconnected_during_request(&self) -> bool {
        self.source == Source::Transport
            && self.outcome == Outcome::Unknown
            && self.request_started
            && self.disconnect_observed
    }

    pub fn details(&self) -> Value {
        let source = match self.source {
            Source::Proxy => "proxy",
            Source::Transport => "transport",
            Source::Protocol => "protocol",
            Source::Remote => "remote",
        };
        let outcome = match self.outcome {
            Outcome::NotSent => "not_sent",
            Outcome::Unknown => "unknown",
            Outcome::RemoteError => "remote_error",
            Outcome::Rejected => "rejected",
            Outcome::ResponseLimited => "response_limited",
            Outcome::LocalError => "local_error",
        };
        let mut details = json!({"kind":self.kind,"message":self.message,"source":source,"outcome":outcome,"request_started":self.request_started,"prior_requests":self.prior_requests});
        if let Some(recovery) = &self.recovery {
            details["recovery"] = recovery.clone();
        }
        details
    }
}

impl From<RemoteError> for ClientError {
    fn from(error: RemoteError) -> Self {
        let outcome = match error.kind.as_str() {
            "manager_busy" => Outcome::Rejected,
            "response_too_large" => Outcome::ResponseLimited,
            _ => Outcome::RemoteError,
        };
        Self {
            kind: error.kind,
            message: error.message,
            source: Source::Remote,
            outcome,
            request_started: true,
            disconnect_observed: false,
            prior_requests: 0,
            recovery: None,
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}
