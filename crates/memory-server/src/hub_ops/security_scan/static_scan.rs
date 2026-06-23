use chrono::Utc;

pub(in crate::hub_ops) fn scan_skill_definition(def: &serde_json::Value) -> serde_json::Value {
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
