use super::*;

impl super::super::super::LlmClient {
    pub(in crate::llm::provider_health) fn now_utc() -> DateTime<Utc> {
        Utc::now()
    }

    pub(in crate::llm::provider_health) fn format_now_utc() -> String {
        Self::now_utc().to_rfc3339()
    }

    pub(in crate::llm::provider_health) fn parse_timestamp(ts: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|parsed| parsed.with_timezone(&Utc))
    }

    pub(in crate::llm::provider_health) fn status_from_key_health_status(
        raw: &str,
    ) -> &'static str {
        match raw {
            HEALTH_COOLDOWN | HEALTH_RATE_LIMITED => HEALTH_RATE_LIMITED,
            HEALTH_AUTH_FAILED => HEALTH_AUTH_FAILED,
            HEALTH_DISABLED => HEALTH_DISABLED,
            HEALTH_EXHAUSTED => HEALTH_EXHAUSTED,
            _ => HEALTH_OK,
        }
    }

    pub(in crate::llm::provider_health) fn key_health_blocked_in_state(
        state: &ProviderState,
        logical_name: &str,
        key_id: &str,
        now: DateTime<Utc>,
    ) -> (KeyAvailability, Option<i64>) {
        state
            .health_snapshots
            .get(logical_name)
            .and_then(|members| members.get(key_id))
            .map(|snapshot| snapshot.availability_at(now))
            .unwrap_or((KeyAvailability::Available, None))
    }
}
