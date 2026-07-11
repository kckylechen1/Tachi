use super::*;

pub(crate) fn build_issue_automation_plan(
    issue: &IssueSnapshot,
    risk_override: Option<&str>,
) -> Value {
    let has_acceptance_criteria = issue_has_acceptance_criteria(issue);
    let risk_signals = issue_risk_signals(issue, risk_override);
    // Only high-confidence signals gate dispatch (#925). Body-only keyword
    // hits stay advisory so renderer/CLI issues mentioning "token" in prose
    // do not false-positive into needs_leader.
    let high_risk_reasons: Vec<String> = risk_signals
        .iter()
        .filter(|signal| signal.confidence == "high")
        .map(|signal| signal.reason.clone())
        .collect();
    let mut high_risk_reasons = high_risk_reasons;
    dedupe_strings(&mut high_risk_reasons);
    let risk_advisory: Vec<Value> = risk_signals
        .iter()
        .filter(|signal| signal.confidence == "low")
        .map(RiskSignal::to_json)
        .collect();
    let risk_evidence: Vec<Value> = risk_signals.iter().map(RiskSignal::to_json).collect();
    let mut leader_gate_reasons = Vec::new();
    if !has_acceptance_criteria {
        leader_gate_reasons.push("missing_acceptance_criteria".to_string());
    }
    leader_gate_reasons.extend(high_risk_reasons.iter().cloned());
    dedupe_strings(&mut leader_gate_reasons);

    let dispatch_allowed = leader_gate_reasons.is_empty();
    let risk_level = if !high_risk_reasons.is_empty() {
        "high"
    } else if !risk_advisory.is_empty() {
        "advisory"
    } else {
        "standard"
    };
    let branch = format!(
        "tachi/issue-{}-{}",
        issue.number,
        slug_for_branch(&issue.title)
    );
    let recommended_next_action = if dispatch_allowed {
        "Run tachi_task(action='cycle_plan', flow_id=...), then follow its next_step for recommend/dispatch/verification/PR handoff."
    } else {
        "Ask the leader to clarify acceptance criteria or approve the high-risk boundary, then rerun tachi_task(action='cycle_plan', flow_id=...)."
    };

    json!({
        "status": if dispatch_allowed { "ready_for_dispatch" } else { "needs_leader" },
        "dispatch_allowed": dispatch_allowed,
        "requires_leader": !dispatch_allowed,
        "risk": risk_level,
        "has_acceptance_criteria": has_acceptance_criteria,
        "missing_acceptance_criteria": !has_acceptance_criteria,
        "high_risk_reasons": high_risk_reasons,
        "risk_evidence": risk_evidence,
        "risk_advisory": risk_advisory,
        "leader_gate_reasons": leader_gate_reasons,
        "branch": branch,
        "pr_title": issue.title,
        "recommended_next_action": recommended_next_action,
        "pr_body_contract": {
            "requires_linked_issue": true,
            "requires_implementation_summary": true,
            "requires_verification_evidence": true,
            "requires_known_gaps": true,
            "auto_merge_allowed": false
        }
    })
}

pub(crate) async fn handle_task_intake(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let target = resolve_task_issue_target(params)?;
    let issue = read_issue_snapshot(server, &target, params).await?;
    let objective = params
        .task
        .clone()
        .filter(|task| !task.trim().is_empty())
        .unwrap_or_else(|| issue.title.clone());
    let flow_id = params
        .flow_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| new_task_flow_id("intake", &objective));
    let automation_plan = build_issue_automation_plan(&issue, params.risk.as_deref());
    // #1001: zero-ceremony presence claim — auto-register/heartbeat on intake
    // so a briefing read from another session sees this session is now
    // working this issue. Degrades to no-op on any storage error; never
    // fails intake.
    crate::claims_ops::auto_register_or_heartbeat_claim(
        server,
        &crate::claims_ops::ClaimHookInput {
            issue_ref: Some(format!("{}#{}", issue.repo, issue.number)),
            flow_id: Some(flow_id.clone()),
            dispatch_id: None,
            branch: automation_plan
                .get("branch")
                .and_then(Value::as_str)
                .map(str::to_string),
            declared_file_scope: None,
        },
    );
    write_intake_flow_artifacts(&flow_id, &objective, &issue, &automation_plan)?;
    seed_intake_orchestrator(server, &flow_id, &objective, &issue, &automation_plan).await?;
    let briefing_params = intake_briefing_params(params, &flow_id, &objective, &issue);
    let briefing =
        crate::copilot_ops::handle_tachi_feature_briefing(server, &briefing_params).await?;
    let pr_handoff_path = run_dir_for_flow_id(&flow_id)?.join("pr_handoff.md");
    // #527: default intake is a receipt (plan + paths). Full briefing is a
    // large read model — include only on format=full (was 50KB+ in dogfood).
    let full = crate::facade_memory_ops::wants_full_format(params.format.as_deref());
    let mut receipt = json!({
        "ok": true,
        "action": "intake",
        "flow_id": flow_id,
        "issue_ref": format!("{}#{}", issue.repo, issue.number),
        "issue": issue_to_json(&issue),
        "automation_plan": automation_plan,
        "doc_paths": issue.doc_paths,
        "spec_paths": issue.spec_paths,
        "pr_handoff_path": pr_handoff_path.to_string_lossy(),
        "run_dir": run_dir_for_flow_id(&flow_id)?.to_string_lossy(),
    });
    if full {
        receipt.as_object_mut().expect("receipt object").insert(
            "briefing".to_string(),
            serde_json::from_str::<Value>(&briefing).unwrap_or(json!(briefing)),
        );
    } else {
        receipt.as_object_mut().expect("receipt object").insert(
            "note".to_string(),
            json!("receipt: briefing omitted; format=full for feature briefing board, or tachi_task(action='briefing')"),
        );
    }
    serde_json::to_string(&receipt).map_err(|e| format!("serialize intake: {e}"))
}

pub(crate) async fn handle_task_link_pr(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let flow_id = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "flow_id is required for link_pr".to_string())?;
    let target = resolve_task_pr_target(params)?;
    let pr = read_pr_snapshot(server, &target).await?;
    let issue_ref = resolve_link_pr_issue_ref(flow_id, params.issue_ref.as_deref())?;
    write_link_pr_artifacts(flow_id, &pr, issue_ref.as_deref())?;
    serde_json::to_string(&json!({
        "ok": true,
        "action": "link_pr",
        "flow_id": flow_id,
        "pr_ref": format!("{}#{}", pr.repo, pr.number),
        "issue_ref": issue_ref,
        "pr": pr_to_json(&pr),
        "run_dir": run_dir_for_flow_id(flow_id)?.to_string_lossy(),
    }))
    .map_err(|e| format!("serialize link_pr: {e}"))
}

pub(crate) fn handle_task_pr_handoff(params: &TachiTaskParams) -> Result<String, String> {
    let started = std::time::Instant::now();
    let flow_id = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "flow_id is required for pr_handoff".to_string())?;
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let status = read_json_file(&run_dir.join("status.json"))?
        .ok_or_else(|| format!("flow status not found for flow_id '{flow_id}'"))?;
    let after_status = started.elapsed();
    let task = params
        .task
        .clone()
        .or_else(|| status_string(&status, "task"))
        .unwrap_or_else(|| "Tachi issue-driven change".to_string());
    let issue_ref = params
        .issue_ref
        .clone()
        .or_else(|| release_note_issue_ref(&status));
    let automation_plan = status
        .get("automation_plan")
        .cloned()
        .unwrap_or_else(|| json!({ "status": "unknown", "dispatch_allowed": true }));
    let verification = crate::verify_ops::read_verification_ledger(flow_id)?;
    let after_verification = started.elapsed();
    let verification_overall = verification
        .as_ref()
        .and_then(|ledger| ledger.get("overall"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let branch = params
        .branch
        .clone()
        .or_else(|| {
            automation_plan
                .get("branch")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("tachi/{}", slug_for_branch(&task)));
    let mut blockers = string_array_field(&automation_plan, "leader_gate_reasons");
    if verification_overall.as_deref() != Some("passed") {
        blockers.push("verification_not_passed".to_string());
    }
    dedupe_strings(&mut blockers);
    let safe_to_open = blockers.is_empty();
    let pr_title = params
        .notes
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            automation_plan
                .get("pr_title")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| task.clone());
    let pr_body = build_pr_handoff_body(
        flow_id,
        &task,
        issue_ref.as_deref(),
        &status,
        verification.as_ref(),
        &blockers,
    );
    let path = run_dir.join("pr_handoff.md");
    write_text_atomic(&path, &pr_body)?;
    let after_write = started.elapsed();
    let path_string = path.to_string_lossy().to_string();
    merge_flow_status(
        &run_dir,
        json!({
            "flow_id": flow_id,
            "stage": "pr_handoff",
            "state": if safe_to_open { "pr_handoff_ready" } else { "pr_handoff_blocked" },
            "branch": branch.clone(),
            "pr_title": pr_title.clone(),
            "pr_handoff_path": path_string.clone(),
            "artifacts": { "pr_handoff": path_string.clone() },
            "updated_at": Utc::now().to_rfc3339(),
        }),
    )?;
    // #527: default receipt keeps the path, not the full body (caller can
    // open the file). format=full restores the pre-change echo of pr_body.
    let full = crate::facade_memory_ops::wants_full_format(params.format.as_deref());
    let mut receipt = json!({
        "ok": true,
        "action": "pr_handoff",
        "flow_id": flow_id,
        "issue_ref": issue_ref,
        "safe_to_open": safe_to_open,
        "blocked_reasons": blockers,
        "branch": branch,
        "pr_title": pr_title,
        "pr_handoff_path": path_string,
        "verification_overall": verification_overall,
        "timing_ms": {
            "status_read": after_status.as_millis() as u64,
            "verification_read": after_verification.saturating_sub(after_status).as_millis() as u64,
            "write_handoff": after_write.saturating_sub(after_verification).as_millis() as u64,
            "total": started.elapsed().as_millis() as u64,
        },
    });
    if full {
        receipt
            .as_object_mut()
            .expect("receipt object")
            .insert("pr_body".to_string(), json!(pr_body));
    } else {
        receipt.as_object_mut().expect("receipt object").insert(
            "note".to_string(),
            json!("receipt: pr_body written to pr_handoff_path; format=full to echo body"),
        );
    }
    serde_json::to_string(&receipt).map_err(|e| format!("serialize pr_handoff: {e}"))
}

pub(crate) fn guard_issue_flow_dispatch(params: &TachiTaskParams) -> Result<(), String> {
    let Some(flow_id) = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(());
    };
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let status = read_json_file(&run_dir.join("status.json"))?.unwrap_or_else(|| json!({}));
    let Some(plan) = status.get("automation_plan") else {
        return Ok(());
    };
    let dispatch_allowed = plan
        .get("dispatch_allowed")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if dispatch_allowed || params.confirm {
        return Ok(());
    }
    let reasons = string_array_field(plan, "leader_gate_reasons");
    let reason_text = if reasons.is_empty() {
        "automation plan requires leader confirmation".to_string()
    } else {
        reasons.join(", ")
    };
    Err(format!(
        "dispatch requires leader confirmation for flow_id '{flow_id}': {reason_text}. Re-run with confirm=true only after leader review."
    ))
}

pub(super) fn issue_has_acceptance_criteria(issue: &IssueSnapshot) -> bool {
    let text = issue_text_for_gate(issue);
    [
        "acceptance criteria",
        "acceptance",
        "definition of done",
        "done when",
        "completion criteria",
        "验收",
        "完成标准",
        "- [ ]",
        "* [ ]",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// One high-risk classifier hit with cited evidence (#925).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RiskSignal {
    pub reason: String,
    pub needle: String,
    pub source: String,
    pub confidence: String,
    pub evidence: String,
}

impl RiskSignal {
    fn to_json(&self) -> Value {
        json!({
            "reason": self.reason,
            "needle": self.needle,
            "source": self.source,
            "confidence": self.confidence,
            "evidence": self.evidence,
        })
    }
}

/// Classify issue risk with per-hit evidence. Title/label hits and risk_override
/// are high confidence (gate dispatch). Body-only keyword hits are low confidence
/// advisory signals — they do not block dispatch (#925 false-positive fix).
pub(super) fn issue_risk_signals(
    issue: &IssueSnapshot,
    risk_override: Option<&str>,
) -> Vec<RiskSignal> {
    let mut signals = Vec::new();
    if matches!(
        risk_override.map(|risk| risk.trim().to_ascii_lowercase()),
        Some(risk) if matches!(risk.as_str(), "high" | "critical" | "security")
    ) {
        signals.push(RiskSignal {
            reason: "risk_override_high".to_string(),
            needle: risk_override.unwrap_or("").trim().to_string(),
            source: "risk_override".to_string(),
            confidence: "high".to_string(),
            evidence: format!("caller set risk={}", risk_override.unwrap_or("").trim()),
        });
    }

    let title = issue.title.to_ascii_lowercase();
    let body = issue
        .body
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let labels = issue.labels.join("\n").to_ascii_lowercase();

    for (needle, reason) in [
        ("security", "touches_security"),
        ("secret", "touches_secrets"),
        ("token", "touches_credentials"),
        ("credential", "touches_credentials"),
        ("authentication", "touches_auth"),
        ("authorization", "touches_auth"),
        ("authn", "touches_auth"),
        ("authz", "touches_auth"),
        ("permission", "touches_permissions"),
        ("vault", "touches_vault"),
        ("migration", "touches_migrations"),
        ("schema", "touches_schema"),
        ("data integrity", "touches_data_integrity"),
        ("delete", "touches_destructive_behavior"),
        ("destructive", "touches_destructive_behavior"),
        ("safe_merge", "touches_merge_gate"),
        ("merge gate", "touches_merge_gate"),
    ] {
        // Prefer high-confidence sources first so a title+body double hit
        // records once with high confidence (not two rows).
        if let Some(snippet) = find_keyword_snippet(&title, needle) {
            signals.push(RiskSignal {
                reason: reason.to_string(),
                needle: needle.to_string(),
                source: "title".to_string(),
                confidence: "high".to_string(),
                evidence: snippet,
            });
            continue;
        }
        if let Some(snippet) = find_keyword_snippet(&labels, needle) {
            signals.push(RiskSignal {
                reason: reason.to_string(),
                needle: needle.to_string(),
                source: "labels".to_string(),
                confidence: "high".to_string(),
                evidence: snippet,
            });
            continue;
        }
        if let Some(snippet) = find_keyword_snippet(&body, needle) {
            signals.push(RiskSignal {
                reason: reason.to_string(),
                needle: needle.to_string(),
                source: "body".to_string(),
                confidence: "low".to_string(),
                evidence: snippet,
            });
        }
    }
    signals
}

pub(super) fn issue_text_for_gate(issue: &IssueSnapshot) -> String {
    format!(
        "{}\n{}\n{}",
        issue.title,
        issue.body.as_deref().unwrap_or_default(),
        issue.labels.join("\n")
    )
    .to_ascii_lowercase()
}

/// Word-boundary keyword search; returns a short evidence snippet on hit.
fn find_keyword_snippet(haystack: &str, needle: &str) -> Option<String> {
    let needle = needle.trim().to_ascii_lowercase();
    if needle.is_empty() || haystack.is_empty() {
        return None;
    }
    let mut offset = 0;
    while let Some(pos) = haystack[offset..].find(&needle) {
        let start = offset + pos;
        let end = start + needle.len();
        if is_keyword_boundary(haystack[..start].chars().next_back())
            && is_keyword_boundary(haystack[end..].chars().next())
        {
            let snippet_start = haystack[..start]
                .char_indices()
                .rev()
                .nth(24)
                .map(|(i, _)| i)
                .unwrap_or(0);
            let snippet_end = haystack[end..]
                .char_indices()
                .nth(24)
                .map(|(i, _)| end + i)
                .unwrap_or_else(|| haystack.len());
            let snippet = haystack[snippet_start..snippet_end].trim();
            return Some(snippet.to_string());
        }
        offset = end;
    }
    None
}

fn is_keyword_boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
}

pub(super) fn slug_for_branch(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if matches!(ch, ' ' | '-' | '_' | '/' | ':' | '.') && !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "work".to_string()
    } else {
        out
    }
}

pub(super) fn build_pr_handoff_body(
    flow_id: &str,
    task: &str,
    issue_ref: Option<&str>,
    status: &Value,
    verification: Option<&Value>,
    blockers: &[String],
) -> String {
    let mut body = String::new();
    body.push_str("## Summary\n\n");
    body.push_str("- ");
    body.push_str(task.trim());
    body.push('\n');
    if let Some(issue_ref) = issue_ref {
        body.push_str(&format!("- Linked issue: {issue_ref}\n"));
    }
    body.push_str(&format!("- Tachi flow: `{flow_id}`\n"));

    let dispatch_ids = string_array_field(status, "completed_dispatch_ids");
    if !dispatch_ids.is_empty() {
        body.push_str("\n## Completed Dispatches\n\n");
        for id in dispatch_ids {
            body.push_str(&format!("- `{id}`\n"));
        }
    }

    body.push_str("\n## Verification\n\n");
    if let Some(verification) = verification {
        if let Some(overall) = verification.get("overall").and_then(Value::as_str) {
            body.push_str(&format!("- Overall: `{overall}`\n"));
        }
        if let Some(items) = verification.get("items").and_then(Value::as_array) {
            for item in items {
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let command = item
                    .get("command")
                    .or_else(|| item.get("id"))
                    .or_else(|| item.get("kind"))
                    .and_then(Value::as_str)
                    .unwrap_or("verification item");
                body.push_str(&format!("- `{status}` {command}\n"));
            }
        }
    } else {
        body.push_str(
            "- Missing verification ledger. Run required checks before opening a non-draft PR.\n",
        );
    }

    body.push_str("\n## Known Gaps / Review Gates\n\n");
    if blockers.is_empty() {
        body.push_str("- None recorded by Tachi automation gate.\n");
    } else {
        for blocker in blockers {
            body.push_str(&format!("- {blocker}\n"));
        }
    }
    body
}
