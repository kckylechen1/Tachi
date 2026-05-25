use super::*;

pub(super) fn risk_rank(risk: &str) -> u8 {
    match risk.trim().to_ascii_lowercase().as_str() {
        "high" => 3,
        "medium" => 2,
        _ => 1,
    }
}

pub(super) fn normalize_risk(risk: &str) -> &'static str {
    match risk_rank(risk) {
        3 => "high",
        2 => "medium",
        _ => "low",
    }
}

pub(super) fn normalize_review_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "pending" => Some("pending"),
        "approved" => Some("approved"),
        "rejected" => Some("rejected"),
        _ => None,
    }
}

pub(super) fn findings_to_vec(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| match x {
                    serde_json::Value::String(s) => Some(s.trim().to_string()),
                    serde_json::Value::Object(map) => map
                        .get("finding")
                        .or_else(|| map.get("issue"))
                        .or_else(|| map.get("signal"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string()),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

pub(super) fn signals_to_vec(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| match x {
                    serde_json::Value::String(s) => Some(s.trim().to_string()),
                    serde_json::Value::Object(map) => map
                        .get("signal")
                        .or_else(|| map.get("id"))
                        .or_else(|| map.get("name"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string()),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

pub(super) fn merge_findings(static_findings: &[String], llm_findings: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for item in static_findings.iter().chain(llm_findings.iter()) {
        if seen.insert(item.as_str()) {
            merged.push(item.clone());
        }
    }
    merged
}

pub(super) fn scan_skill_definition(def: &serde_json::Value) -> serde_json::Value {
    let mut findings: Vec<String> = Vec::new();
    let mut signals: Vec<String> = Vec::new();
    let mut seen_findings: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_signals: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut high_hits: u32 = 0;
    let mut medium_hits: u32 = 0;

    let prompt_like_fields = [
        "prompt",
        "template",
        "system",
        "instructions",
        "description",
    ];
    let mut corpus = String::new();
    for field in prompt_like_fields {
        if let Some(text) = def.get(field).and_then(|v| v.as_str()) {
            if !corpus.is_empty() {
                corpus.push('\n');
            }
            corpus.push_str(text);
        }
    }
    let lower = corpus.to_ascii_lowercase();

    let mut record = |signal: &str, finding: &str, severity: &str| {
        if seen_signals.insert(signal.to_string()) {
            signals.push(signal.to_string());
        }
        if seen_findings.insert(finding.to_string()) {
            findings.push(finding.to_string());
        }
        if severity == "high" {
            high_hits += 1;
        } else {
            medium_hits += 1;
        }
    };

    let critical_patterns = [
        (
            "rm -rf",
            "destructive_action",
            "Contains destructive shell command pattern 'rm -rf'",
        ),
        (
            "mkfs",
            "destructive_action",
            "Contains disk format pattern 'mkfs'",
        ),
        (
            "dd if=",
            "destructive_action",
            "Contains raw disk write pattern 'dd if='",
        ),
        (
            "shred ",
            "destructive_action",
            "Contains secure delete pattern 'shred'",
        ),
        (
            "sudo ",
            "privilege_escalation",
            "Contains privileged command pattern 'sudo'",
        ),
        (
            "curl | sh",
            "remote_bootstrap",
            "Contains remote shell pipe pattern 'curl | sh'",
        ),
        (
            "curl|sh",
            "remote_bootstrap",
            "Contains remote shell pipe pattern 'curl|sh'",
        ),
        (
            "wget | sh",
            "remote_bootstrap",
            "Contains remote shell pipe pattern 'wget | sh'",
        ),
        (
            "wget|sh",
            "remote_bootstrap",
            "Contains remote shell pipe pattern 'wget|sh'",
        ),
        (
            "invoke-expression",
            "remote_bootstrap",
            "Contains PowerShell remote execution pattern 'Invoke-Expression'",
        ),
        (
            "begin rsa private key",
            "secret_exposure",
            "Contains private key material marker",
        ),
        (
            "begin openssh private key",
            "secret_exposure",
            "Contains OpenSSH private key material marker",
        ),
        (
            "aws_secret_access_key",
            "secret_exposure",
            "Contains inline AWS secret key marker",
        ),
        (
            "ghp_",
            "secret_exposure",
            "Contains inline GitHub token marker",
        ),
    ];
    for (pat, signal, msg) in critical_patterns {
        if lower.contains(pat) {
            record(signal, msg, "high");
        }
    }

    let warning_patterns = [
        (
            "os.system(",
            "unbounded_execution",
            "Contains os.system execution pattern",
        ),
        (
            "subprocess.",
            "unbounded_execution",
            "Contains subprocess execution pattern",
        ),
        (
            "eval(",
            "unbounded_execution",
            "Contains eval execution pattern",
        ),
        (
            "exec(",
            "unbounded_execution",
            "Contains exec execution pattern",
        ),
        (
            "bash -c",
            "unbounded_execution",
            "Contains shell trampoline pattern 'bash -c'",
        ),
        (
            "sh -c",
            "unbounded_execution",
            "Contains shell trampoline pattern 'sh -c'",
        ),
        (
            "process.env",
            "secret_exposure",
            "Contains process.env environment access pattern",
        ),
        (
            "printenv",
            "secret_exposure",
            "Contains environment dump pattern 'printenv'",
        ),
        (
            ".env",
            "secret_exposure",
            "Contains '.env' access indicator",
        ),
        (
            "~/.ssh",
            "secret_exposure",
            "Contains '~/.ssh' key path indicator",
        ),
        (
            "/etc/passwd",
            "data_exfiltration",
            "Contains local system file path '/etc/passwd'",
        ),
        (
            "ignore previous instructions",
            "prompt_injection",
            "Contains prompt override phrase 'ignore previous instructions'",
        ),
        (
            "ignore all previous",
            "prompt_injection",
            "Contains prompt override phrase 'ignore all previous'",
        ),
        (
            "bypass safety",
            "prompt_injection",
            "Contains policy bypass phrase 'bypass safety'",
        ),
        (
            "disable safety",
            "prompt_injection",
            "Contains policy bypass phrase 'disable safety'",
        ),
        (
            "reveal system prompt",
            "prompt_injection",
            "Contains prompt extraction phrase 'reveal system prompt'",
        ),
        (
            "exfiltrate",
            "data_exfiltration",
            "Contains explicit exfiltration keyword",
        ),
        (
            "webhook",
            "data_exfiltration",
            "Contains webhook external delivery pattern",
        ),
    ];
    for (pat, signal, msg) in warning_patterns {
        if lower.contains(pat) {
            record(signal, msg, "medium");
        }
    }

    let risk = if high_hits > 0 {
        "high"
    } else if medium_hits > 0 {
        "medium"
    } else {
        "low"
    };

    serde_json::json!({
        "scanned_at": Utc::now().to_rfc3339(),
        "risk": risk,
        "blocked": risk == "high",
        "signals": signals,
        "findings": findings,
        "engine": "static-heuristic-v2",
        "high_hits": high_hits,
        "medium_hits": medium_hits
    })
}

pub(super) async fn scan_skill_definition_with_llm(
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
            let llm_for_fallback = server.llm.clone();
            let payload_for_fallback = payload.clone();
            let model_for_fallback = model.clone();
            crate::claude_pool::pool_call_with_fallback(
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
            let parsed: serde_json::Value =
                serde_json::from_str(llm::LlmClient::strip_code_fence(&raw)).unwrap_or_else(|_| {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SecurityScanBackend {
    ClaudeCli,
    RawApi,
    Disabled,
}

impl SecurityScanBackend {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            SecurityScanBackend::ClaudeCli => "claude_cli",
            SecurityScanBackend::RawApi => "raw_api",
            SecurityScanBackend::Disabled => "disabled",
        }
    }
}

/// Resolve `SKILL_SECURITY_SCAN_BACKEND` env var into a backend choice.
/// Unknown / missing values default to `claude_cli`.
pub(super) fn resolve_security_scan_backend() -> SecurityScanBackend {
    match std::env::var("SKILL_SECURITY_SCAN_BACKEND")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "raw_api" | "raw-api" | "rawapi" => SecurityScanBackend::RawApi,
        "disabled" | "off" | "false" | "0" => SecurityScanBackend::Disabled,
        _ => SecurityScanBackend::ClaudeCli,
    }
}

pub(super) fn merge_skill_scans(
    static_scan: &serde_json::Value,
    llm_scan: Option<&serde_json::Value>,
) -> serde_json::Value {
    let static_risk = static_scan
        .get("risk")
        .and_then(|v| v.as_str())
        .unwrap_or("low");
    let static_blocked = static_scan
        .get("blocked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let static_findings = findings_to_vec(static_scan.get("findings"));
    let static_signals = merge_findings(
        &signals_to_vec(static_scan.get("signals")),
        &signals_to_vec(static_scan.get("dangerous_signals")),
    );

    let mut llm_risk = "low";
    let mut llm_blocked = false;
    let mut llm_findings: Vec<String> = Vec::new();
    let mut llm_signals: Vec<String> = Vec::new();
    let mut llm_status = "skipped".to_string();
    let mut llm_meta = serde_json::json!({});

    if let Some(scan) = llm_scan {
        llm_status = scan
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        llm_meta = scan.clone();

        if llm_status == "ok" {
            if let Some(result) = scan.get("result") {
                llm_risk = result.get("risk").and_then(|v| v.as_str()).unwrap_or("low");
                llm_blocked = result
                    .get("blocked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                llm_findings = findings_to_vec(result.get("findings"));
                llm_signals = merge_findings(
                    &signals_to_vec(result.get("signals")),
                    &signals_to_vec(result.get("dangerous_signals")),
                );
            }
        }
    }

    let final_risk = if risk_rank(llm_risk) > risk_rank(static_risk) {
        llm_risk
    } else {
        static_risk
    };
    let blocked = static_blocked || llm_blocked || normalize_risk(final_risk) == "high";
    let findings = merge_findings(&static_findings, &llm_findings);
    let signals = merge_findings(&static_signals, &llm_signals);

    serde_json::json!({
        "scanned_at": Utc::now().to_rfc3339(),
        "risk": normalize_risk(final_risk),
        "blocked": blocked,
        "signals": signals,
        "findings": findings,
        "engine": "hybrid-static-llm-v1",
        "static": static_scan,
        "llm": llm_meta,
        "llm_status": llm_status
    })
}

#[cfg(test)]
mod backend_tests {
    use super::{resolve_security_scan_backend, SecurityScanBackend};
    use std::sync::Mutex;

    // Serialize env mutation across tests in this module.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_backend_env<F: FnOnce()>(value: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("SKILL_SECURITY_SCAN_BACKEND").ok();
        match value {
            Some(v) => std::env::set_var("SKILL_SECURITY_SCAN_BACKEND", v),
            None => std::env::remove_var("SKILL_SECURITY_SCAN_BACKEND"),
        }
        f();
        match prev {
            Some(v) => std::env::set_var("SKILL_SECURITY_SCAN_BACKEND", v),
            None => std::env::remove_var("SKILL_SECURITY_SCAN_BACKEND"),
        }
    }

    #[test]
    fn backend_defaults_to_claude_cli_when_unset() {
        with_backend_env(None, || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::ClaudeCli
            );
        });
    }

    #[test]
    fn backend_defaults_to_claude_cli_when_unknown() {
        with_backend_env(Some("bogus"), || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::ClaudeCli
            );
        });
    }

    #[test]
    fn backend_recognises_raw_api() {
        with_backend_env(Some("raw_api"), || {
            assert_eq!(resolve_security_scan_backend(), SecurityScanBackend::RawApi);
        });
        with_backend_env(Some("RAW-API"), || {
            assert_eq!(resolve_security_scan_backend(), SecurityScanBackend::RawApi);
        });
    }

    #[test]
    fn backend_recognises_disabled() {
        with_backend_env(Some("disabled"), || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::Disabled
            );
        });
        with_backend_env(Some("0"), || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::Disabled
            );
        });
    }

    #[test]
    fn backend_recognises_explicit_claude_cli() {
        with_backend_env(Some("claude_cli"), || {
            assert_eq!(
                resolve_security_scan_backend(),
                SecurityScanBackend::ClaudeCli
            );
        });
    }

    #[test]
    fn backend_as_str_matches_env_values() {
        assert_eq!(SecurityScanBackend::ClaudeCli.as_str(), "claude_cli");
        assert_eq!(SecurityScanBackend::RawApi.as_str(), "raw_api");
        assert_eq!(SecurityScanBackend::Disabled.as_str(), "disabled");
    }
}
