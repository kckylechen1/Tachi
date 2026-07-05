use super::*;

#[derive(Clone)]
pub(in crate::llm) struct ChatLaneConfig {
    pub(in crate::llm) base_url: String,
    pub(in crate::llm) model: String,
    pub(in crate::llm) api_key_envs: Vec<&'static str>,
}

#[derive(Clone)]
pub struct ProviderSecret {
    pub key_id: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderKeyCooldownStatus {
    pub key_id: String,
    pub remaining_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderPoolStatus {
    pub logical_name: String,
    pub total_keys: usize,
    pub available_keys: usize,
    pub rate_limited_keys: Vec<ProviderKeyCooldownStatus>,
    pub current_index: usize,
    pub strategy: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealthStatus {
    pub source_of_truth: &'static str,
    pub reload_ttl_secs: u64,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_success_age_secs: Option<u64>,
    pub last_error: Option<String>,
    pub persist_last_attempt_at: Option<String>,
    pub persist_last_success_at: Option<String>,
    pub persist_last_success_age_secs: Option<u64>,
    pub persist_last_error: Option<String>,
}

#[derive(Clone)]
pub(in crate::llm) struct SelectedProviderSecret {
    pub(in crate::llm) logical_name: String,
    pub(in crate::llm) key_id: String,
    pub(in crate::llm) value: String,
}

#[derive(Clone, Copy)]
pub(in crate::llm) enum ChatLane {
    Extract,
    Distill,
    Reasoning,
    Summary,
}

impl ChatLane {
    pub(in crate::llm) fn as_str(self) -> &'static str {
        match self {
            Self::Extract => "extract",
            Self::Distill => "distill",
            Self::Reasoning => "reasoning",
            Self::Summary => "summary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::llm) enum KeyAvailability {
    Available,
    Cooldown,
    AuthFailed,
    Disabled,
    Exhausted,
}

impl KeyAvailability {
    pub(in crate::llm) fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Cooldown => "cooldown",
            Self::AuthFailed => "auth_failed",
            Self::Disabled => "disabled",
            Self::Exhausted => "exhausted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::llm) enum KeyRetryStatus {
    Available,
    RetryAfter(Duration),
    Unavailable,
}

#[derive(Debug, Clone)]
pub(in crate::llm) struct ClaudeCliFailure {
    pub(in crate::llm) kind: ClaudeCliFailureKind,
    pub(in crate::llm) failed_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::llm) enum ClaudeCliFailureKind {
    SpawnFailed,
    Timeout,
}

impl ClaudeCliFailureKind {
    pub(in crate::llm) fn from_error(error: &str) -> Option<Self> {
        let error = error.trim_start();
        if error.starts_with("claude cli spawn failed:") {
            Some(Self::SpawnFailed)
        } else if error.starts_with("claude cli timeout after ") {
            Some(Self::Timeout)
        } else {
            None
        }
    }

    pub(in crate::llm) fn as_str(self) -> &'static str {
        match self {
            Self::SpawnFailed => "spawn_failed",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::llm) struct ClaudeCliSkip {
    pub(in crate::llm) kind: ClaudeCliFailureKind,
    pub(in crate::llm) remaining: Duration,
}
