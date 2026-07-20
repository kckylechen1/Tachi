use crate::server_state::MemoryServer;
use memory_server_runtime::RateLimitRejection;

impl MemoryServer {
    #[cfg(test)]
    pub(crate) fn rate_limiter_entry_counts_for_tests(&self) -> (usize, usize) {
        let rl = self.rate_limiter_lock();
        rl.entry_counts()
    }

    /// Rate-limit check keyed by this clone's stamped `#1255` session id.
    pub(crate) fn check_session_rate_limit(
        &self,
        tool_name: &str,
        args_hash: &str,
    ) -> Result<Option<String>, rmcp::ErrorData> {
        let session_id = self.rate_limit_session_id();
        self.check_rate_limit(tool_name, args_hash, &session_id)
    }

    pub(crate) fn check_rate_limit(
        &self,
        tool_name: &str,
        args_hash: &str,
        session_id: &str,
    ) -> Result<Option<String>, rmcp::ErrorData> {
        let overrides = {
            let rt = self.agent_runtime_read();
            rt.agent_profile
                .as_ref()
                .map(|p| (p.rate_limit_rpm, p.rate_limit_burst))
        };
        let (rpm_override, burst_override) = overrides.unwrap_or((None, None));

        let mut rl = self.rate_limiter_lock();
        rl.check_tool_call(
            tool_name,
            args_hash,
            session_id,
            rpm_override,
            burst_override,
        )
        .map_err(rate_limit_error)
    }
}

fn rate_limit_error(rejection: RateLimitRejection) -> rmcp::ErrorData {
    rmcp::ErrorData::new(
        rmcp::model::ErrorCode::INVALID_REQUEST,
        rejection.message,
        None,
    )
}
