use crate::server_state::MemoryServer;
use memory_server_runtime::{IdenticalCallPolicy, RateLimitRejection};

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

    /// Derive the narrow polling exception from the native route and canonical
    /// action-effect authority. Wire arguments cannot select limiter policy.
    pub(crate) fn check_session_rate_limit_for_call(
        &self,
        tool_name: &str,
        args_hash: &str,
        arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> Result<Option<String>, rmcp::ErrorData> {
        let status_poll = self.tool_router.has_route(tool_name)
            && tool_name == "tachi_staff"
            && arguments
                .and_then(|args| {
                    // Malformed calls keep strict loop detection; the owning
                    // router still returns their canonical parameter error.
                    serde_json::from_value::<tachi_params::TachiStaffParams>(
                        serde_json::Value::Object(args.clone()),
                    )
                    .ok()
                })
                .is_some_and(|params| {
                    params.action.trim().eq_ignore_ascii_case("status")
                        && params
                            .dispatch_id
                            .as_deref()
                            .is_some_and(crate::dispatch_ops::is_valid_dispatch_id)
                        && crate::action_effect::facade_action_effect(
                            tool_name,
                            Some(&params.action),
                        )
                        .is_some_and(|metadata| {
                            metadata.effect == crate::action_effect::ActionEffect::ReadOnly
                        })
                });
        if !status_poll {
            return self.check_session_rate_limit(tool_name, args_hash);
        }
        self.check_rate_limit_with_policy(
            tool_name,
            args_hash,
            &self.rate_limit_session_id(),
            IdenticalCallPolicy::AllowPolling,
        )
    }

    pub(crate) fn check_rate_limit(
        &self,
        tool_name: &str,
        args_hash: &str,
        session_id: &str,
    ) -> Result<Option<String>, rmcp::ErrorData> {
        self.check_rate_limit_with_policy(
            tool_name,
            args_hash,
            session_id,
            IdenticalCallPolicy::DetectLoop,
        )
    }

    fn check_rate_limit_with_policy(
        &self,
        tool_name: &str,
        args_hash: &str,
        session_id: &str,
        identical_call_policy: IdenticalCallPolicy,
    ) -> Result<Option<String>, rmcp::ErrorData> {
        let overrides = {
            let rt = self.agent_runtime_read();
            rt.agent_profile
                .as_ref()
                .map(|p| (p.rate_limit_rpm, p.rate_limit_burst))
        };
        let (rpm_override, burst_override) = overrides.unwrap_or((None, None));

        let mut rl = self.rate_limiter_lock();
        rl.check_tool_call_with_policy(
            tool_name,
            args_hash,
            session_id,
            rpm_override,
            burst_override,
            identical_call_policy,
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
