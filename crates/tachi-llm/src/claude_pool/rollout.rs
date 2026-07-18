//! Rollout flag for the claude_pool → provider-based `tachi-llm` migration
//! (#1087). Kept in its own module so the flag's parsing rule is a single
//! source both `pool_call_with_fallback` (this crate) and per-consumer call
//! sites (tachi-server) can consult.
//!
//! **Default flipped to `true` (2026-07-18, ClaudePool decommission step 1/3,
//! issue #1261).** All five live call sites (distill / dispatch_v2 /
//! security_scan / register / evolve) now route through the provider-based
//! `tachi-llm` executor by default; the `claude` CLI binary fallback path
//! is reached only when a caller explicitly opts back in with
//! `TACHI_CLAUDE_POOL_PROVIDER_FIRST=0`. The CLI path itself is scheduled
//! for removal in step 3 of the decommission — keeping the escape hatch
//! here gives step 2 (removing the per-call-site CLI fallback branches) a
//! revert knob if a provider regression surfaces in production.

/// Env var gating the provider-first rollout. **Env unset (the common case)
/// or `"1"`/`"true"` (case-insensitive) enables provider-first behavior.**
/// Any other value — including `"0"`/`"false"`/`""`/`"yes"` — opts back into
/// the legacy CLI-first path. The parsing rule itself (recognize `1`/`true`,
/// treat everything else as opt-out) is unchanged from pre-#1261; only the
/// *default* when the env is unset flipped from `false` to `true`.
pub const PROVIDER_ROLLOUT_ENV: &str = "TACHI_CLAUDE_POOL_PROVIDER_FIRST";

/// Whether callers should try the provider-based executor first, falling
/// back to the Claude CLI pool on error. Defaults to `true` as of the
/// ClaudePool decommission (issue #1261, step 1/3).
pub fn provider_rollout_enabled() -> bool {
    std::env::var(PROVIDER_ROLLOUT_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `PROVIDER_ROLLOUT_ENV` is process-global and also mutated by
    // `claude_pool::tests` (the `pool_call_with_fallback` rollout tests) —
    // both use `crate::test_support::global_test_lock` to serialize access
    // (crate convention; see `foundry_runs_dir_honors_tachi_home`).

    #[test]
    fn defaults_to_enabled_when_unset() {
        let _guard = crate::test_support::global_test_lock().lock();
        let prev = std::env::var(PROVIDER_ROLLOUT_ENV).ok();
        std::env::remove_var(PROVIDER_ROLLOUT_ENV);
        assert!(
            provider_rollout_enabled(),
            "post-#1261 default must be provider-first when the env is unset"
        );
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
        // The pre-flip version treated every non-`1`/`true` value as
        // disabled (including `""` and `"yes"`). Post-#1261 the *default*
        // (env unset) flipped to enabled, but the explicit-value parsing
        // is unchanged: only `1`/`true` enable, everything else — including
        // `0`/`false`/`""`/`yes` — is treated as an explicit opt-OUT back
        // to the legacy CLI-first path. This keeps the parsing rule a
        // single positive recognition (`1`/`true`) rather than two
        // (recognize opt-in AND recognize opt-out), and matches how
        // ops would expect `TACHI_CLAUDE_POOL_PROVIDER_FIRST=0` to behave.
        for value in ["0", "false", "FALSE", "False", "", "yes"] {
            std::env::set_var(PROVIDER_ROLLOUT_ENV, value);
            assert!(
                !provider_rollout_enabled(),
                "value `{value}` is non-`1`/`true` and opts back into CLI-first (the legacy path); only env-UNSET defaults to enabled post-#1261"
            );
        }
        match prev {
            Some(v) => std::env::set_var(PROVIDER_ROLLOUT_ENV, v),
            None => std::env::remove_var(PROVIDER_ROLLOUT_ENV),
        }
    }
}
