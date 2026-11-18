use super::*;

impl super::super::super::LlmClient {
    pub(in crate::llm::provider_health) fn now_utc() -> Timestamp {
        Timestamp::now()
    }

    /// The persisted key-health stamp form: chrono's `to_rfc3339()` normal
    /// shape (AutoSi fraction, `+00:00`), kept byte-stable through the
    /// jiff migration via memcore's compat renderer.
    pub(in crate::llm::provider_health) fn format_now_utc() -> String {
        memcore::time_compat::rfc3339_compat(Self::now_utc())
    }

    pub(in crate::llm::provider_health) fn parse_timestamp(ts: &str) -> Option<Timestamp> {
        Timestamp::from_str(ts).ok()
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
        now: Timestamp,
    ) -> (KeyAvailability, Option<i64>) {
        state.health_availability(logical_name, key_id, now)
    }
}
