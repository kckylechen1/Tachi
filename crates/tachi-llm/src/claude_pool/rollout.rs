//! Rollout flag for the claude_pool → provider-based `tachi-llm` migration
//! (#1087). Kept in its own module so the flag's parsing rule is a single
//! source both `pool_call_with_fallback` (this crate) and per-consumer call
//! sites (tachi-server) can consult.

/// Env var gating the provider-first rollout. Unset/anything not
/// `"1"`/`"true"` (case-insensitive) keeps today's CLI-first behavior —
/// this PR must land inert by default (#1087 point 4: flagged provider path
/// + CLI-pool fallback, NOT a default flip and NOT the CLI pool's deletion).
pub const PROVIDER_ROLLOUT_ENV: &str = "TACHI_CLAUDE_POOL_PROVIDER_FIRST";

/// Whether callers should try the provider-based executor first, falling
/// back to the Claude CLI pool on error. Defaults to `false`.
pub fn provider_rollout_enabled() -> bool {
    std::env::var(PROVIDER_ROLLOUT_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `PROVIDER_ROLLOUT_ENV` is process-global and also mutated by
    // `claude_pool::tests` (the `pool_call_with_fallback` rollout tests) —
    // both use `crate::test_support::global_test_lock` to serialize access
    // (crate convention; see `foundry_runs_dir_honors_tachi_home`).

    #[test]
    fn defaults_to_disabled_when_unset() {
        let _guard = crate::test_support::global_test_lock().lock();
        let prev = std::env::var(PROVIDER_ROLLOUT_ENV).ok();
        std::env::remove_var(PROVIDER_ROLLOUT_ENV);
        assert!(!provider_rollout_enabled());
        match prev {
            Some(v) => std::env::set_var(PROVIDER_ROLLOUT_ENV, v),
            None => std::env::remove_var(PROVIDER_ROLLOUT_ENV),
        }
    }

    #[test]
    fn accepts_1_and_true_case_insensitive() {
        let _guard = crate::test_support::global_test_lock().lock();
        let prev = std::env::var(PROVIDER_ROLLOUT_ENV).ok();
        for value in ["1", "true", "TRUE", "True"] {
            std::env::set_var(PROVIDER_ROLLOUT_ENV, value);
            assert!(provider_rollout_enabled(), "value `{value}` should enable");
        }
        for value in ["0", "false", "", "yes"] {
            std::env::set_var(PROVIDER_ROLLOUT_ENV, value);
            assert!(
                !provider_rollout_enabled(),
                "value `{value}` should NOT enable"
            );
        }
        match prev {
            Some(v) => std::env::set_var(PROVIDER_ROLLOUT_ENV, v),
            None => std::env::remove_var(PROVIDER_ROLLOUT_ENV),
        }
    }
}
