use super::*;

pub(crate) async fn handle_task_cycle_plan(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let raw_status = super::cycle_status::handle_task_cycle_status(server, params)
        .await
        .map_err(|err| err.replace("cycle_status", "cycle_plan"))?;
    let status = serde_json::from_str::<Value>(&raw_status)
        .map_err(|e| format!("parse cycle_status for cycle_plan: {e}"))?;
    let plan = build_cycle_plan_from_status(&status);
    serde_json::to_string(&plan).map_err(|e| format!("serialize cycle_plan: {e}"))
}

fn build_cycle_plan_from_status(status: &Value) -> Value {
    let flow_id = status.get("flow_id").and_then(Value::as_str);
    let issue_ref = status.get("issue_ref").and_then(Value::as_str);
    let pr_ref = status.get("pr_ref").and_then(Value::as_str);
    let linked_docs = string_array_field(status, "linked_docs");
    let linked_specs = string_array_field(status, "linked_specs");
    let verification_overall = status
        .get("verification")
        .and_then(|verification| verification.get("overall"))
        .and_then(Value::as_str);
    let merge_state = status
        .get("github")
        .and_then(|github| github.get("merge_state"))
        .and_then(Value::as_str);
    let flow_bound = flow_id.is_some();
    let has_issue = issue_ref.is_some();
    let has_pr = pr_ref.is_some();
    let has_specs = !linked_specs.is_empty();
    let has_docs = !linked_docs.is_empty();
    let verification_passed = verification_overall == Some("passed");
    let pr_gate_ready = matches!(merge_state, Some("ready" | "merged"));
    let release_note_present = artifact_exists(status, "release_note");
    let close_loop_present = artifact_exists(status, "close_loop");
    let dispatch_started = has_event(status, "dispatch_linked");
    let dispatch_completed = has_event(status, "dispatch_completed");
    let downstream_work_started = has_pr
        || verification_overall.is_some()
        || pr_gate_ready
        || release_note_present
        || close_loop_present;
    let pr_handoff_ready = status
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|state| matches!(state, "pr_handoff_ready" | "pr_linked"))
        || matches!(
            status.get("stage").and_then(Value::as_str),
            Some("review" | "verified" | "release" | "closed")
        );

    let intake_status = if flow_bound && has_issue {
        "passed"
    } else if has_issue {
        "ready"
    } else {
        "blocked"
    };
    let docs_status = if has_specs {
        "passed"
    } else if has_docs {
        "needs_confirmation"
    } else {
        "blocked"
    };
    let contract_ready = has_specs || has_docs;
    let execution_ready = has_issue && flow_bound && contract_ready;
    let briefing_status = if downstream_work_started || dispatch_started || dispatch_completed {
        "passed"
    } else if execution_ready {
        "ready"
    } else {
        "blocked"
    };
    let dispatch_status = if dispatch_completed || downstream_work_started {
        "passed"
    } else if dispatch_started {
        "active"
    } else if execution_ready && docs_status != "needs_confirmation" {
        "ready"
    } else if execution_ready {
        "needs_confirmation"
    } else {
        "blocked"
    };
    let complete_status = if dispatch_completed || downstream_work_started {
        "passed"
    } else if dispatch_started {
        "ready"
    } else if execution_ready {
        "pending"
    } else {
        "blocked"
    };
    let verification_status = match verification_overall {
        Some("passed") => "passed",
        Some("failed" | "stale") => "blocked",
        Some(_) => "active",
        None if dispatch_completed || has_pr => "ready",
        None if execution_ready => "pending",
        None => "blocked",
    };
    let pr_handoff_status = if has_pr || (pr_handoff_ready && verification_passed) {
        "passed"
    } else if verification_passed {
        "ready"
    } else if execution_ready {
        "pending"
    } else {
        "blocked"
    };
    let link_pr_status = if has_pr {
        "passed"
    } else if pr_handoff_status == "passed" || pr_handoff_status == "ready" {
        "ready"
    } else {
        "pending"
    };
    let pr_status_status = if pr_gate_ready {
        "passed"
    } else if has_pr && verification_passed {
        "ready"
    } else if has_pr {
        "blocked"
    } else {
        "pending"
    };
    let release_status = if release_note_present {
        "passed"
    } else if pr_gate_ready {
        "ready"
    } else {
        "pending"
    };
    let close_loop_status = if close_loop_present {
        "passed"
    } else if release_note_present && pr_gate_ready {
        "ready"
    } else {
        "pending"
    };

    let steps = vec![
        cycle_plan_step(
            "intake",
            "Bind issue to flow",
            "tachi_task",
            "intake",
            intake_status,
            true,
            vec_if([
                (flow_bound, "local flow exists"),
                (has_issue, "issue_ref linked"),
            ]),
            gaps_if([
                (!has_issue, "missing issue_ref"),
                (has_issue && !flow_bound, "no local flow artifact"),
            ]),
            cycle_command("intake", flow_id, issue_ref, pr_ref),
        ),
        cycle_plan_step(
            "contract",
            "Attach docs/specs contract",
            "tachi_task",
            "briefing",
            docs_status,
            true,
            contract_evidence(&linked_docs, &linked_specs),
            gaps_if([
                (!has_docs && !has_specs, "missing linked docs/specs"),
                (
                    has_docs && !has_specs,
                    "linked docs present but no explicit spec refs",
                ),
            ]),
            cycle_command("briefing", flow_id, issue_ref, pr_ref),
        ),
        cycle_plan_step(
            "briefing",
            "Load feature briefing",
            "tachi_task",
            "briefing",
            briefing_status,
            true,
            vec_if([(execution_ready, "issue and contract context available")]),
            gaps_if([(
                !execution_ready,
                "issue flow or contract context is incomplete",
            )]),
            cycle_command("briefing", flow_id, issue_ref, pr_ref),
        ),
        cycle_plan_step(
            "dispatch",
            "Dispatch bounded implementation",
            "tachi_task",
            "dispatch",
            dispatch_status,
            true,
            vec_if([
                (dispatch_started, "dispatch linked"),
                (dispatch_completed, "dispatch completed"),
                (
                    downstream_work_started && !dispatch_completed,
                    "downstream PR or verification evidence exists",
                ),
            ]),
            gaps_if([
                (
                    !execution_ready,
                    "intake/contract prerequisites are not ready",
                ),
                (
                    execution_ready && docs_status == "needs_confirmation",
                    "leader confirmation needed because no explicit spec refs are linked",
                ),
            ]),
            cycle_command("dispatch", flow_id, issue_ref, pr_ref),
        ),
        cycle_plan_step(
            "complete",
            "Record worker completion and eval",
            "tachi_task",
            "complete",
            complete_status,
            true,
            vec_if([
                (dispatch_completed, "dispatch completion recorded"),
                (
                    downstream_work_started && !dispatch_completed,
                    "downstream PR or verification evidence exists",
                ),
            ]),
            gaps_if([(!dispatch_started, "no dispatch has been linked to the flow")]),
            cycle_command("complete", flow_id, issue_ref, pr_ref),
        ),
        cycle_plan_step(
            "verification",
            "Record required verification",
            "tachi_verify",
            "record",
            verification_status,
            true,
            verification_evidence(verification_overall),
            verification_gaps(verification_overall),
            "tachi_verify(action='record', flow_id=..., items=[...])".to_string(),
        ),
        cycle_plan_step(
            "pr_handoff",
            "Prepare PR handoff",
            "tachi_gh",
            "pr_handoff",
            pr_handoff_status,
            true,
            vec_if([
                (pr_handoff_ready, "PR handoff state recorded"),
                (has_pr, "PR already linked"),
            ]),
            gaps_if([(!verification_passed, "verification has not passed")]),
            cycle_command("pr_handoff", flow_id, issue_ref, pr_ref),
        ),
        cycle_plan_step(
            "link_pr",
            "Link GitHub PR",
            "tachi_gh",
            "link_pr",
            link_pr_status,
            true,
            vec_if([(has_pr, "pr_ref linked")]),
            gaps_if([(!has_pr, "missing pr_ref")]),
            cycle_command("link_pr", flow_id, issue_ref, pr_ref),
        ),
        cycle_plan_step(
            "pr_status",
            "Run PR gate preview",
            "tachi_gh",
            "pr_status",
            pr_status_status,
            true,
            vec_if([(pr_gate_ready, "PR merge gate ready or merged")]),
            pr_status_gaps(has_pr, verification_passed, merge_state),
            cycle_command("pr_status", flow_id, issue_ref, pr_ref),
        ),
        cycle_plan_step(
            "release_note",
            "Generate release note",
            "tachi_gh",
            "release_note",
            release_status,
            true,
            vec_if([(release_note_present, "release_note.md exists")]),
            gaps_if([(!pr_gate_ready, "PR merge gate is not ready or merged")]),
            cycle_command("release_note", flow_id, issue_ref, pr_ref),
        ),
        cycle_plan_step(
            "close_loop",
            "Sink lessons to issue/docs/wiki/memory",
            "tachi_task",
            "close_loop",
            close_loop_status,
            true,
            vec_if([(close_loop_present, "close_loop.json exists")]),
            gaps_if([(!release_note_present, "release note has not been generated")]),
            cycle_command("close_loop", flow_id, issue_ref, pr_ref),
        ),
    ];
    let next_step = steps
        .iter()
        .find(|step| step.get("status").and_then(Value::as_str) != Some("passed"))
        .cloned()
        .unwrap_or_else(|| {
            cycle_plan_step(
                "closed",
                "Lifecycle closed",
                "tachi_task",
                "cycle_status",
                "passed",
                false,
                vec!["all required lifecycle steps passed".to_string()],
                Vec::new(),
                cycle_command("cycle_status", flow_id, issue_ref, pr_ref),
            )
        });
    let current_blockers = current_blockers(&next_step, status);

    json!({
        "ok": true,
        "action": "cycle_plan",
        "flow_id": flow_id,
        "cycle_id": status.get("cycle_id").cloned().unwrap_or(Value::Null),
        "stage": status.get("stage").cloned().unwrap_or(Value::Null),
        "state": status.get("state").cloned().unwrap_or(Value::Null),
        "issue_ref": issue_ref,
        "pr_ref": pr_ref,
        "next_step": next_step,
        "current_blockers": current_blockers,
        "steps": steps,
        "readiness": {
            "execution_ready": execution_ready && docs_status == "passed",
            "dispatch_needs_leader_confirmation": dispatch_status == "needs_confirmation",
            "ready_for_pr_handoff": pr_handoff_status == "ready",
            "ready_for_pr_gate": pr_status_status == "ready",
            "ready_for_release_note": release_status == "ready",
            "ready_for_close_loop": close_loop_status == "ready",
            "closed": close_loop_present,
        },
        "status_summary": {
            "linked_docs": linked_docs,
            "linked_specs": linked_specs,
            "verification_overall": verification_overall,
            "merge_state": merge_state,
            "source_action": "cycle_status",
            "read_only": true,
        },
        "spec_drift": status.get("spec_drift").cloned().unwrap_or_else(|| json!([])),
        "next_action": status.get("next_action").cloned().unwrap_or(Value::Null),
        "source": status.get("source").cloned().unwrap_or(Value::Null),
    })
}

fn artifact_exists(status: &Value, name: &str) -> bool {
    status
        .get("artifacts")
        .and_then(|artifacts| artifacts.get(name))
        .and_then(|entry| entry.get("exists"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn has_event(status: &Value, event_name: &str) -> bool {
    status
        .get("events")
        .and_then(Value::as_array)
        .is_some_and(|events| {
            events
                .iter()
                .any(|event| event.get("event").and_then(Value::as_str) == Some(event_name))
        })
}

fn cycle_plan_step(
    id: &str,
    label: &str,
    tool: &str,
    action: &str,
    status: &str,
    required: bool,
    evidence: Vec<String>,
    gaps: Vec<String>,
    command: String,
) -> Value {
    json!({
        "id": id,
        "label": label,
        "tool": tool,
        "action": action,
        "status": status,
        "required": required,
        "evidence": evidence,
        "gaps": gaps,
        "command": command,
    })
}

fn contract_evidence(linked_docs: &[String], linked_specs: &[String]) -> Vec<String> {
    let mut evidence = Vec::new();
    evidence.extend(linked_specs.iter().map(|path| format!("spec:{path}")));
    evidence.extend(linked_docs.iter().map(|path| format!("doc:{path}")));
    evidence
}

fn verification_evidence(overall: Option<&str>) -> Vec<String> {
    overall
        .map(|value| vec![format!("verification_overall:{value}")])
        .unwrap_or_default()
}

fn verification_gaps(overall: Option<&str>) -> Vec<String> {
    match overall {
        Some("passed") => Vec::new(),
        Some(value) => vec![format!("verification state is {value}")],
        None => vec!["missing verification ledger".to_string()],
    }
}

fn pr_status_gaps(
    has_pr: bool,
    verification_passed: bool,
    merge_state: Option<&str>,
) -> Vec<String> {
    let mut gaps = Vec::new();
    if !has_pr {
        gaps.push("missing pr_ref".to_string());
    }
    if !verification_passed {
        gaps.push("verification has not passed".to_string());
    }
    if !matches!(merge_state, Some("ready" | "merged")) {
        gaps.push(format!(
            "merge_state is {}",
            merge_state.unwrap_or("unknown")
        ));
    }
    gaps
}

fn current_blockers(next_step: &Value, status: &Value) -> Vec<Value> {
    let next_step_gaps = next_step
        .get("gaps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|gap| {
            json!({
                "kind": "next_step_gap",
                "step": next_step.get("id").cloned().unwrap_or(Value::Null),
                "detail": gap,
            })
        });
    let drift_blockers = status
        .get("spec_drift")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|drift| {
            drift
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(is_blocking_drift)
        })
        .cloned();
    next_step_gaps.chain(drift_blockers).collect()
}

fn is_blocking_drift(kind: &str) -> bool {
    matches!(
        kind,
        "missing_issue_ref"
            | "missing_docs_specs"
            | "missing_linked_specs"
            | "missing_verification"
            | "verification_not_passed"
            | "verification_head_mismatch"
            | "release_note_before_ready_pr"
    )
}

fn cycle_command(
    action: &str,
    flow_id: Option<&str>,
    issue_ref: Option<&str>,
    pr_ref: Option<&str>,
) -> String {
    let mut args = vec![format!("action='{action}'")];
    // F2: GitHub PR lifecycle coaching points at tachi_gh (canonical).
    let tool = match action {
        "link_pr" | "pr_status" | "pr_handoff" | "release_note" => "tachi_gh",
        _ => "tachi_task",
    };
    match action {
        "intake" => args.push(format!(
            "issue_ref='{}'",
            issue_ref.unwrap_or("owner/repo#123")
        )),
        "link_pr" | "pr_status" | "release_note" => {
            args.push(format!("flow_id='{}'", flow_id.unwrap_or("flow_...")));
            args.push(format!("pr_ref='{}'", pr_ref.unwrap_or("owner/repo#123")));
        }
        "dispatch" => {
            args.push(format!("flow_id='{}'", flow_id.unwrap_or("flow_...")));
            if let Some(issue_ref) = issue_ref {
                args.push(format!("issue_ref='{issue_ref}'"));
            }
            args.push("profile=...".to_string());
            args.push("task=...".to_string());
        }
        "complete" => {
            args.push(format!("flow_id='{}'", flow_id.unwrap_or("flow_...")));
            args.push("dispatch_id=...".to_string());
            args.push("outcome='success'".to_string());
        }
        "briefing" | "pr_handoff" | "close_loop" | "cycle_status" => {
            args.push(format!("flow_id='{}'", flow_id.unwrap_or("flow_...")));
        }
        _ => {}
    }
    format!("{tool}({})", args.join(", "))
}
