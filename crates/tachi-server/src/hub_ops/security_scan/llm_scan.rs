use crate::utils::parse_env_bool;
use crate::MemoryServer;
use tachi_hub::{resolve_security_scan_backend, SecurityScanBackend};

/// Strong-tier model override for the #1087 two-vote provider path only —
/// independent of `SKILL_SECURITY_SCAN_MODEL`, which stays the legacy
/// single-shot backends' (CLI/raw_api) fast-tier default. Unset uses the
/// reasoning lane's own configured model (DeepSeek-reasoner when
/// `DEEPSEEK_API_KEY` is set — see `tachi-llm`'s `ProviderRuntimeConfig`).
const STRONG_TIER_MODEL_ENV: &str = "SKILL_SECURITY_SCAN_STRONG_MODEL";

pub(in crate::hub_ops) async fn scan_skill_definition_with_llm(
    server: &MemoryServer,
    def: &serde_json::Value,
) -> Option<serde_json::Value> {
    if cfg!(test) {
        return None;
    }
    let enabled = parse_env_bool("SKILL_SECURITY_SCAN_USE_LLM").unwrap_or(true);
    if !enabled {
        return None;
    }

    // Phase 2: SKILL_SECURITY_SCAN_BACKEND chooses how the LLM portion is
    // executed. `claude_cli` is the default — call the pool first and fall
    // back to the raw_api lane on Err. `raw_api` bypasses the pool entirely.
    // `disabled` skips the LLM portion (callers still receive the static
    // heuristic scan via merge_skill_scans).
    let backend = resolve_security_scan_backend();
    if backend == SecurityScanBackend::Disabled {
        return None;
    }

    let model = std::env::var("SKILL_SECURITY_SCAN_MODEL")
        .unwrap_or_else(|_| "Qwen/Qwen3.5-27B".to_string());
    let payload = serde_json::to_string(def).ok()?;

    let llm_call_result: Result<(String, &'static str), String> = match backend {
        SecurityScanBackend::ClaudeCli => {
            if tachi_llm::claude_pool::provider_rollout_enabled() {
                // #1087 rollout: security scan is the strong-tier, two-vote,
                // fail-closed path — never the plain provider-first-then-CLI
                // pattern the other consumers use, since a single vote (or a
                // silent LLM-error-becomes-"skipped" merge) is fail-OPEN for
                // a security surface.
                Ok(two_vote_provider_scan(server, &payload).await)
            } else {
                let llm_for_fallback = server.llm.clone();
                let payload_for_fallback = payload.clone();
                let model_for_fallback = model.clone();
                tachi_llm::claude_pool::pool_call_with_fallback(
                    &server.claude_pool,
                    crate::prompts::SKILL_SECURITY_SCAN_PROMPT,
                    &payload,
                    "security-scan",
                    move || async move {
                        llm_for_fallback
                            .call_extract_llm(
                                crate::prompts::SKILL_SECURITY_SCAN_PROMPT,
                                &payload_for_fallback,
                                Some(&model_for_fallback),
                                0.1,
                                800,
                            )
                            .await
                    },
                )
                .await
                .map(|(text, src)| (text, src.as_str()))
            }
        }
        SecurityScanBackend::RawApi => server
            .llm
            .call_extract_llm(
                crate::prompts::SKILL_SECURITY_SCAN_PROMPT,
                &payload,
                Some(&model),
                0.1,
                800,
            )
            .await
            .map(|text| (text, "raw_api")),
        SecurityScanBackend::Disabled => unreachable!("handled above"),
    };

    match llm_call_result {
        Ok((raw, source)) => {
            let parsed: serde_json::Value = serde_json::from_str(
                tachi_llm::LlmClient::strip_code_fence(&raw),
            )
            .unwrap_or_else(|_| {
                serde_json::json!({
                    "risk": "medium",
                    "blocked": false,
                    "findings": ["Failed to parse LLM security scan JSON output"],
                    "reason": raw
                })
            });
            Some(serde_json::json!({
                "status": "ok",
                "model": model,
                "backend": source,
                "result": parsed,
            }))
        }
        Err(e) => Some(serde_json::json!({
            "status": "error",
            "model": model,
            "backend": backend.as_str(),
            "error": e,
        })),
    }
}

// ── #1087 strong-tier, two-vote, fail-closed provider scan ──────────────

/// Call the reasoning lane (strong tier) twice and merge the votes
/// fail-closed. Always returns `Ok`-shaped output — even total failure
/// produces a maximally-restrictive verdict rather than an `Err` that would
/// flow into `merge_skill_scans`'s `status != "ok"` branch, which treats a
/// missing/errored LLM scan as "skipped" (fail-OPEN for the LLM component,
/// acceptable for a best-effort CLI call, NOT acceptable for a security
/// surface's primary path).
async fn two_vote_provider_scan(server: &MemoryServer, payload: &str) -> (String, &'static str) {
    let strong_model = std::env::var(STRONG_TIER_MODEL_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty());

    let vote_a = one_vote(
        server,
        payload,
        strong_model.as_deref(),
        "security-scan-vote-a",
    )
    .await;
    let vote_b = one_vote(
        server,
        payload,
        strong_model.as_deref(),
        "security-scan-vote-b",
    )
    .await;

    let merged = match (vote_a, vote_b) {
        (Ok(a), Ok(b)) => fail_closed_merge(a, b),
        (Ok(a), Err(e)) | (Err(e), Ok(a)) => fail_closed_merge(
            a,
            fail_closed_sentinel(&format!("second vote unavailable: {e}")),
        ),
        (Err(e1), Err(e2)) => fail_closed_default(&format!("both votes unavailable: {e1}; {e2}")),
    };

    (
        serde_json::to_string(&merged).unwrap_or_else(|_| merged.to_string()),
        "provider_two_vote",
    )
}

/// One strong-tier vote: provider (reasoning lane, no Claude-CLI-first
/// behavior — see `LlmClient::call_reasoning_llm_provider_only`'s doc
/// comment) first; on Err, the Claude CLI pool as a fallback for the
/// rollout cycle (#1087 point 4), writing the same run-directory artifact
/// contract as every other `ClaudePool::call` site.
async fn one_vote(
    server: &MemoryServer,
    payload: &str,
    model: Option<&str>,
    label: &str,
) -> Result<serde_json::Value, String> {
    match server
        .llm
        .call_reasoning_llm_provider_only(
            crate::prompts::SKILL_SECURITY_SCAN_PROMPT,
            payload,
            model,
            0.1,
            800,
        )
        .await
    {
        Ok(raw) => parse_vote(&raw).ok_or_else(|| {
            format!(
                "vote returned unparsable JSON: {}",
                raw.chars().take(200).collect::<String>()
            )
        }),
        Err(provider_err) => {
            let prompt = format!(
                "<system>\n{}\n</system>\n\n{}",
                crate::prompts::SKILL_SECURITY_SCAN_PROMPT,
                payload
            );
            match server.claude_pool.call(label, &prompt).await {
                Ok(outcome) => parse_vote(&outcome.text).ok_or_else(|| {
                    format!("cli fallback vote unparsable (provider err was: {provider_err})")
                }),
                Err(cli_err) => Err(format!(
                    "provider vote failed ({provider_err}); CLI pool fallback also failed ({cli_err})"
                )),
            }
        }
    }
}

fn parse_vote(raw: &str) -> Option<serde_json::Value> {
    serde_json::from_str(tachi_llm::LlmClient::strip_code_fence(raw)).ok()
}

/// Sentinel standing in for a vote that could not be obtained at all
/// (provider AND CLI fallback both failed for one of the two votes) —
/// treated as maximally risky so `fail_closed_merge` still fails closed.
fn fail_closed_sentinel(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "blocked": true,
        "risk": "high",
        "findings": [format!("Second vote unavailable, failing closed: {reason}")],
        "signals": ["scan_second_vote_unavailable"],
    })
}

/// Both votes unavailable — the maximally restrictive verdict.
fn fail_closed_default(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "risk": "high",
        "blocked": true,
        "findings": [format!("Strong-tier two-vote security scan failed closed: {reason}")],
        "signals": ["scan_unavailable_fail_closed"],
    })
}

/// Merge two vote verdicts fail-closed: either vote flagging `blocked`
/// blocks; the higher of the two `risk` levels wins; malformed/missing
/// `blocked`/`risk` fields default to the most restrictive reading
/// (`blocked: true`, `risk: "high"`) rather than the most permissive.
fn fail_closed_merge(a: serde_json::Value, b: serde_json::Value) -> serde_json::Value {
    let a_blocked = a.get("blocked").and_then(|v| v.as_bool()).unwrap_or(true);
    let b_blocked = b.get("blocked").and_then(|v| v.as_bool()).unwrap_or(true);
    let blocked = a_blocked || b_blocked;

    let a_risk = a.get("risk").and_then(|v| v.as_str()).unwrap_or("high");
    let b_risk = b.get("risk").and_then(|v| v.as_str()).unwrap_or("high");
    let risk = higher_risk(a_risk, b_risk);

    let mut findings = value_str_vec(a.get("findings"));
    findings.extend(value_str_vec(b.get("findings")));
    if a_blocked != b_blocked {
        findings.push(
            "Two-vote security scan disagreement on blocked status; failing closed".to_string(),
        );
    }
    findings.sort();
    findings.dedup();

    let mut signals = value_str_vec(a.get("signals"));
    signals.extend(value_str_vec(b.get("signals")));
    signals.sort();
    signals.dedup();

    serde_json::json!({
        "risk": risk,
        "blocked": blocked,
        "findings": findings,
        "signals": signals,
    })
}

fn higher_risk(a: &str, b: &str) -> &'static str {
    fn rank(risk: &str) -> u8 {
        match risk {
            "high" => 2,
            "medium" => 1,
            _ => 0,
        }
    }
    match rank(a).max(rank(b)) {
        2 => "high",
        1 => "medium",
        _ => "low",
    }
}

fn value_str_vec(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_closed_merge_blocks_when_either_vote_blocks() {
        let a = serde_json::json!({"blocked": false, "risk": "low", "findings": [], "signals": []});
        let b = serde_json::json!({"blocked": true, "risk": "high", "findings": ["bad"], "signals": ["x"]});
        let merged = fail_closed_merge(a, b);
        assert_eq!(merged["blocked"], true);
        assert_eq!(merged["risk"], "high");
    }

    #[test]
    fn fail_closed_merge_takes_higher_risk_even_when_both_unblocked() {
        let a = serde_json::json!({"blocked": false, "risk": "low", "findings": [], "signals": []});
        let b =
            serde_json::json!({"blocked": false, "risk": "medium", "findings": [], "signals": []});
        let merged = fail_closed_merge(a, b);
        assert_eq!(merged["blocked"], false);
        assert_eq!(merged["risk"], "medium");
    }

    #[test]
    fn fail_closed_merge_defaults_malformed_fields_to_restrictive() {
        // Missing/malformed `blocked`/`risk` must NOT silently resolve to
        // "safe" — that would be exactly the fail-open bug #1087 retires.
        let a = serde_json::json!({"findings": [], "signals": []});
        let b = serde_json::json!({"blocked": false, "risk": "low", "findings": [], "signals": []});
        let merged = fail_closed_merge(a, b);
        assert_eq!(merged["blocked"], true, "malformed vote must fail closed");
        assert_eq!(merged["risk"], "high");
    }

    #[test]
    fn fail_closed_merge_flags_disagreement_in_findings() {
        let a = serde_json::json!({"blocked": true, "risk": "high", "findings": [], "signals": []});
        let b = serde_json::json!({"blocked": false, "risk": "low", "findings": [], "signals": []});
        let merged = fail_closed_merge(a, b);
        assert_eq!(merged["blocked"], true);
        let findings = merged["findings"].as_array().unwrap();
        assert!(findings
            .iter()
            .any(|f| f.as_str().unwrap_or("").contains("disagreement")));
    }

    #[test]
    fn fail_closed_default_is_maximally_restrictive() {
        let v = fail_closed_default("network down");
        assert_eq!(v["blocked"], true);
        assert_eq!(v["risk"], "high");
        assert!(v["findings"][0].as_str().unwrap().contains("network down"));
    }

    #[test]
    fn higher_risk_orders_correctly() {
        assert_eq!(higher_risk("low", "medium"), "medium");
        assert_eq!(higher_risk("medium", "high"), "high");
        assert_eq!(higher_risk("low", "low"), "low");
        assert_eq!(higher_risk("unknown", "low"), "low");
    }
}
