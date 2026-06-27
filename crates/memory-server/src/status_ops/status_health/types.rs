use crate::status_ops::ApiKeyRotationMemberStatus;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct HealthDeduction {
    pub(crate) code: String,
    pub(crate) label: String,
    pub(crate) points: u8,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ProviderProbeResult {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) message: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ProviderRotationGroupProbe {
    pub(crate) logical_name: String,
    pub(crate) total_keys: i64,
    pub(crate) configured_keys: i64,
    pub(crate) healthy_keys: i64,
    pub(crate) rate_limited_keys: i64,
    pub(crate) auth_failed_keys: i64,
    pub(crate) current_index: i64,
    pub(crate) strategy: String,
    pub(crate) next_retry_at: Option<String>,
    pub(crate) keys: Vec<ApiKeyRotationMemberStatus>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ProviderProbeReport {
    pub(crate) probes: Vec<ProviderProbeResult>,
    pub(crate) rotation_groups: Vec<ProviderRotationGroupProbe>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ProviderProbeCache {
    pub(crate) last_probe_at: String,
    pub(crate) ttl_seconds: i64,
    pub(crate) probes: Vec<ProviderProbeResult>,
    #[serde(default)]
    pub(crate) rotation_groups: Vec<ProviderRotationGroupProbe>,
}

impl ProviderProbeCache {
    pub(crate) fn is_stale(&self) -> bool {
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&self.last_probe_at) else {
            return true;
        };
        let age = chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc));
        age.num_seconds() > self.ttl_seconds
    }
}
