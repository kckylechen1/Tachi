use super::*;

/// Combine a `system` preamble and `user` payload into a single prompt the
/// Claude CLI can consume on stdin. Mirrors the OpenAI-compatible chat
/// shape used by the raw-API lanes so behaviour stays comparable.
pub(super) fn format_pool_prompt(system: &str, user: &str) -> String {
    let sys = system.trim();
    let usr = user.trim();
    if sys.is_empty() {
        usr.to_string()
    } else {
        format!("<system>\n{sys}\n</system>\n\n{usr}")
    }
}

/// Source label returned by [`pool_call_with_fallback`] indicating which
/// backend actually produced the response. Useful for audit/log output so
/// operators can see when the Claude pool degraded to the raw-API lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolCallSource {
    ClaudeCli,
    RawApiFallback,
    /// #1087 rollout: the provider-based `tachi-llm` executor answered
    /// directly (the `fallback` closure, run as the primary path via
    /// `ClaudePool::call_via_provider` because `provider_rollout_enabled()`
    /// was set) — not a degradation, the CLI pool was never invoked.
    ProviderTachiLlm,
}

impl PoolCallSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PoolCallSource::ClaudeCli => "claude_cli",
            PoolCallSource::RawApiFallback => "raw_api_fallback",
            PoolCallSource::ProviderTachiLlm => "provider_tachi_llm",
        }
    }
}

/// Try `pool.call(system+user, label)` first; on Err, invoke `fallback`
/// (the existing raw-API LLM helper closure). Returns the produced text
/// together with which backend supplied it so callers can log/audit.
///
/// The pool failure is logged to stderr so operators notice when the
/// Claude CLI lane keeps degrading.
///
/// #1087 rollout: when [`provider_rollout_enabled`] is set, the order
/// inverts — `fallback` (the provider closure) runs FIRST via
/// [`ClaudePool::call_via_provider`], preserving the exact run-directory
/// artifact contract (`prompt.md`/`result.md`/`status.json`); on Err, the
/// Claude CLI pool runs as the fallback. Defaults to the CLI-first path
/// (flag unset) so this lands inert.
pub async fn pool_call_with_fallback<F, Fut>(
    pool: &ClaudePool,
    system: &str,
    user: &str,
    label: &str,
    fallback: F,
) -> Result<(String, PoolCallSource), String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let combined = format_pool_prompt(system, user);

    if provider_rollout_enabled() {
        return match pool.call_via_provider(label, &combined, fallback).await {
            Ok(outcome) => Ok((outcome.text, PoolCallSource::ProviderTachiLlm)),
            Err(provider_err) => {
                eprintln!(
                    "[claude_pool:{label}] provider path failed, falling back to CLI pool: {provider_err}"
                );
                match pool.call(label, &combined).await {
                    Ok(outcome) => Ok((outcome.text, PoolCallSource::ClaudeCli)),
                    Err(cli_err) => Err(format!(
                        "provider path failed ({provider_err}); CLI pool fallback also failed ({cli_err})"
                    )),
                }
            }
        };
    }

    match pool.call(label, &combined).await {
        Ok(outcome) => Ok((outcome.text, PoolCallSource::ClaudeCli)),
        Err(pool_err) => {
            eprintln!("[claude_pool:{label}] degraded to raw_api fallback: {pool_err}");
            let text = fallback().await?;
            Ok((text, PoolCallSource::RawApiFallback))
        }
    }
}
