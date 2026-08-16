//! Issue → Doc → Memory closure helpers (#150).

use crate::tool_params::{TachiGhParams, WikiWriteParams};
use crate::MemoryServer;
use serde_json::{json, Value};
use tachi_llm::PersistedModelInvocationReceiptV1;

/// Footer stamped on every closure write-back comment. Also the idempotency
/// key: if an issue/PR already carries a comment containing this, close_loop
/// skips re-posting instead of spamming on re-run.
const CLOSURE_COMMENT_MARKER: &str = "_Posted automatically by the Tachi closure loop._";

/// Build validated `references[]` for wiki closure (issue + docs + related issues).
pub(crate) fn build_closure_references(
    issue_ref: &str,
    doc_paths: &[String],
    related_issues: &[String],
) -> Vec<String> {
    let mut refs = Vec::new();
    let issue = issue_ref.trim();
    if !issue.is_empty() {
        refs.push(issue.to_string());
    }
    for doc in doc_paths {
        let doc = doc.trim();
        if !doc.is_empty() && !refs.iter().any(|r| r == doc) {
            refs.push(doc.to_string());
        }
    }
    for related in related_issues {
        let related = related.trim();
        if !related.is_empty() && !refs.iter().any(|r| r == related) {
            refs.push(related.to_string());
        }
    }
    refs
}

fn promotion_destination_layer(path: Option<&str>) -> &'static str {
    let Some(path) = path.map(str::trim).filter(|value| !value.is_empty()) else {
        return "wiki";
    };
    if path == "/guide"
        || path.starts_with("/guide/")
        || path == "guide"
        || path.starts_with("guide/")
    {
        "guide"
    } else {
        "wiki"
    }
}

fn trimmed_nonempty_unique(values: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !out.iter().any(|existing| existing == value) {
            out.push(value.to_string());
        }
    }
    out
}

pub(crate) fn build_close_loop_metadata(
    issue_ref: &str,
    doc_paths: &[String],
    related_issues: &[String],
    wiki_path: Option<&str>,
    references: &[String],
) -> Value {
    json!({
        "promotion": {
            "source": "close_loop",
            "decision": "promote",
            "decision_mode": "explicit_invocation",
            "destination_layer": promotion_destination_layer(wiki_path),
            "source_ref": issue_ref,
            "source_refs": references,
            "doc_paths": trimmed_nonempty_unique(doc_paths),
            "related_issues": trimmed_nonempty_unique(related_issues),
            "automatic_double_write": false,
        },
    })
}

pub(crate) fn build_promotion_plan(issue_ref: &str, references: &[String]) -> Value {
    json!({
        "requires_explicit_invocation": true,
        "automatic_double_write": false,
        "source_ref": issue_ref,
        "source_refs": references,
        "destinations": [
            {
                "destination": "wiki",
                "layer": "wiki",
                "authority": "advisory",
                "tool": "tachi_gh",
                "action": "close_loop",
                "when": "project-specific durable lesson or decision after the work is complete"
            },
            {
                "destination": "guide",
                "layer": "guide",
                "authority": "playbook",
                "tool": "tachi_gh",
                "action": "close_loop",
                "when": "reusable workflow/SOP lesson; pass wiki_path under /guide"
            },
            {
                "destination": "feedback_rule",
                "layer": "feedback_rule",
                "authority": "behavior_patch",
                "tool": "tachi_memory",
                "action": "save",
                "when": "the lesson should patch future agent prompts or evidence contracts"
            },
            {
                "destination": "github_issue",
                "layer": "github_ref",
                "authority": "project_work_record",
                "tool": "tachi_gh",
                "action": "issue_create",
                "when": "a valid project task or bug remains open after review"
            },
            {
                "destination": "eval",
                "layer": "eval",
                "authority": "evidence",
                "tool": "tachi_complete",
                "action": "complete",
                "when": "record verification, reviewer usefulness, or false-positive signal"
            }
        ]
    })
}

/// Templated closure comment posted back to the source issue/PR. Deterministic
/// (not free-form) so the loop's write-back can't spam your own issues.
fn build_closure_comment_body(
    wiki_title: &str,
    wiki_path: Option<&str>,
    doc_paths: &[String],
    spec_paths: &[String],
    references: &[String],
) -> String {
    let mut body = String::from("✅ **Tachi close_loop**\n\n");
    body.push_str(&format!("Durable lesson saved to wiki: **{wiki_title}**"));
    if let Some(p) = wiki_path.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str(&format!(" (`{p}`)"));
    }
    body.push('\n');
    if !doc_paths.is_empty() {
        body.push_str(&format!("\nDocs: {}\n", doc_paths.join(", ")));
    }
    if !spec_paths.is_empty() {
        body.push_str(&format!("\nSpec touched: {}\n", spec_paths.join(", ")));
    }
    if !references.is_empty() {
        body.push_str(&format!("\nReferences: {}\n", references.join(", ")));
    }
    body.push('\n');
    body.push_str(CLOSURE_COMMENT_MARKER);
    body
}

/// Spec is the source of truth; flag when a closure may have left it stale.
fn spec_advisory(spec_paths: &[String], doc_paths: &[String]) -> Value {
    if !spec_paths.is_empty() {
        json!({
            "status": "recorded",
            "spec_paths": spec_paths,
            "note": "Confirm these specs reflect the merged behavior.",
        })
    } else if !doc_paths.is_empty() {
        json!({
            "status": "advisory",
            "note": "Docs referenced but no spec_paths recorded. If this change altered behavior, update the canonical spec so it does not drift.",
        })
    } else {
        json!({ "status": "none", "note": "No docs/specs referenced." })
    }
}

/// Best-effort write-back: post the closure comment to an issue or PR. Never
/// fails the closure — if GitHub is unreachable the result records it as debt
/// for the briefing to resurface.
async fn post_closure_comment(
    server: &MemoryServer,
    target_ref: &str,
    is_pr: bool,
    body: &str,
) -> Value {
    let parsed = if is_pr {
        crate::task_lifecycle::parse_pr_ref(target_ref)
    } else {
        crate::task_lifecycle::parse_issue_ref(target_ref, None)
    };
    let Some(target) = parsed else {
        return json!({ "posted": false, "reason": format!("could not parse ref '{target_ref}'") });
    };
    let kind = if is_pr { "pr" } else { "issue" };
    let label = format!("{}#{}", target.repo, target.number);

    // Idempotency: don't re-post the closure comment if one is already there
    // (re-run, retry, or a loop firing close_loop twice). Best-effort — if the
    // probe can't reach GitHub it returns false and we proceed.
    if crate::gh_ops::gh_comment_marker_present(
        server,
        kind,
        &target.repo,
        target.number,
        CLOSURE_COMMENT_MARKER,
    ) {
        return json!({ "posted": false, "ref": label, "reason": "closure comment already present (idempotent skip)" });
    }

    match crate::gh_ops::handle_gh_comment(
        server,
        kind,
        crate::tool_params::GhCommentParams {
            repo: target.repo,
            number: target.number,
            body: Some(body.to_string()),
            dry_run: false,
        },
    )
    .await
    {
        Ok(result) => json!({
            "posted": true,
            "ref": label,
            "result": serde_json::from_str::<Value>(&result).unwrap_or(json!(result)),
        }),
        Err(e) => json!({ "posted": false, "ref": label, "error": e }),
    }
}

/// Draft a wiki title + body from a flow's `result.md` when the caller didn't
/// supply them — lowering the activation energy to close a loop (the agent can
/// call close_loop with just a flow_id).
///
/// Title is deterministic (first markdown heading, or the issue ref). For the
/// body the backend model DRAFTS a distilled, reusable lesson; the raw-result
/// dump is the guaranteed fallback. The model is purely advisory: the call is
/// best-effort + time-bounded and NEVER blocks or fails the closure (GitHub
/// outage, missing provider key, slow model → fall back). Returns the draft
/// plus its source and optional model receipt, or None if there's nothing to
/// draft. A deterministic fallback intentionally carries no receipt.
async fn draft_from_result(
    server: &MemoryServer,
    flow_id: &str,
    issue_ref: &str,
) -> Option<(
    String,
    String,
    &'static str,
    Option<PersistedModelInvocationReceiptV1>,
)> {
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).ok()?;
    let result = std::fs::read_to_string(run_dir.join("result.md")).ok()?;
    let trimmed = result.trim();
    if trimmed.is_empty() {
        return None;
    }

    let title = first_heading_or_closure_title(trimmed, issue_ref);

    // Deterministic fallback body: the capped raw result. Always available.
    const MAX_BODY_CHARS: usize = 4000;
    let fallback_body = if trimmed.chars().take(MAX_BODY_CHARS + 1).count() > MAX_BODY_CHARS {
        let capped: String = trimmed.chars().take(MAX_BODY_CHARS).collect();
        format!("{capped}\n\n_(drafted from result.md; truncated at {MAX_BODY_CHARS} chars — edit before relying on it)_")
    } else {
        trimmed.to_string()
    };

    // Preferred: let the backend model distill the result into a reusable
    // lesson. `TACHI_DISABLE_LLM_DRAFT` (set in tests) forces the deterministic
    // path so the suite never makes a network call.
    if std::env::var_os("TACHI_DISABLE_LLM_DRAFT").is_none() {
        const MAX_INPUT_CHARS: usize = 8000;
        let input = format!(
            "Distill the durable, reusable lesson from this completed work on {issue_ref}:\n\n{}",
            trimmed.chars().take(MAX_INPUT_CHARS).collect::<String>()
        );
        let llm = server.llm.clone();
        let distilled = tokio::time::timeout(std::time::Duration::from_secs(30), async move {
            llm.generate_distill_with_receipt(&input).await
        })
        .await;
        if let Ok(Ok(generated)) = distilled {
            let body = generated.value.trim();
            if !body.is_empty() {
                return Some((title, body.to_string(), "llm", Some(generated.invocation)));
            }
        }
    }

    Some((title, fallback_body, "result_md", None))
}

/// Draft wiki title/body from caller `notes` when result.md is unavailable (#925).
fn draft_from_notes(
    notes: &str,
    issue_ref: &str,
) -> Option<(
    String,
    String,
    &'static str,
    Option<PersistedModelInvocationReceiptV1>,
)> {
    let trimmed = notes.trim();
    if trimmed.is_empty() {
        return None;
    }
    let title = first_heading_or_closure_title(trimmed, issue_ref);
    const MAX_BODY_CHARS: usize = 4000;
    let body = if trimmed.chars().take(MAX_BODY_CHARS + 1).count() > MAX_BODY_CHARS {
        let capped: String = trimmed.chars().take(MAX_BODY_CHARS).collect();
        format!(
            "{capped}\n\n_(drafted from notes; truncated at {MAX_BODY_CHARS} chars — edit before relying on it)_"
        )
    } else {
        trimmed.to_string()
    };
    Some((title, body, "notes", None))
}

fn first_heading_or_closure_title(text: &str, issue_ref: &str) -> String {
    text.lines()
        .find_map(|line| {
            let heading = line.trim_start();
            let text = heading.trim_start_matches('#').trim();
            if heading.starts_with('#') && !text.is_empty() {
                Some(text.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| format!("Closure: {issue_ref}"))
}

fn close_loop_missing_draft_error(flow_id: Option<&str>, has_notes: bool) -> String {
    // Minimal template so agents can repair without a second discovery round (#925).
    let flow = flow_id.unwrap_or("<flow_id>");
    format!(
        "close_loop needs wiki content. Provide one of:\n\
         - wiki_title + wiki_text (preferred durable lesson)\n\
         - flow_id with result.md at .tachi/runs/{flow}/result.md\n\
         - notes (draft source when result.md is missing)\n\
         Missing now: wiki_title/wiki_text{}; notes={}.",
        if flow_id.is_some() {
            "; result.md unavailable or empty"
        } else {
            "; flow_id not set"
        },
        if has_notes {
            "present-but-empty?"
        } else {
            "absent"
        }
    )
}

pub(crate) async fn handle_close_loop(
    server: &MemoryServer,
    params: TachiGhParams,
) -> Result<String, String> {
    let doc_paths = trimmed_nonempty_unique(&params.doc_paths);
    let related_issues = trimmed_nonempty_unique(&params.related_issues);
    let issue_ref = params.issue_ref.as_deref().unwrap_or("").trim().to_string();
    let references = build_closure_references(&issue_ref, &doc_paths, &related_issues);
    let promotion_plan = build_promotion_plan(&issue_ref, &references);

    // Preview is deliberately side-effect free: it validates and returns the
    // exact reference/promotion plan, before drafting, wiki writes, pattern
    // feedback, comments, or flow markers can run.
    if params.dry_run.unwrap_or(false) {
        crate::wiki_ops::validate_references(&references)?;
        return serde_json::to_string(&json!({
            "ok": true,
            "action": "close_loop",
            "dry_run": true,
            "references": references,
            "promotion_plan": promotion_plan,
        }))
        .map_err(|e| format!("serialize close_loop preview: {e}"));
    }

    let issue_ref = if issue_ref.is_empty() {
        return Err("issue_ref is required for close_loop".to_string());
    } else {
        issue_ref
    };
    let spec_paths = trimmed_nonempty_unique(&params.spec_paths);
    // Resolve the wiki title/text. If either is omitted, draft it from
    // the flow's result.md, then from notes (#925).
    let explicit_title = params.wiki_title.clone().filter(|s| !s.trim().is_empty());
    let explicit_text = params.wiki_text.clone().filter(|s| !s.trim().is_empty());
    let mut auto_drafted = false;
    let mut draft_source = "explicit";
    let (title, text, model_invocation) = match (explicit_title.clone(), explicit_text.clone()) {
        (Some(t), Some(x)) => (t, x, None),
        (maybe_t, maybe_x) => {
            let drafted = match params.flow_id.as_deref() {
                Some(fid) => draft_from_result(server, fid, &issue_ref).await,
                None => None,
            };
            let drafted = match drafted {
                Some(d) => Some(d),
                None => params
                    .notes
                    .as_deref()
                    .and_then(|notes| draft_from_notes(notes, &issue_ref)),
            };
            match drafted {
                Some((dt, dx, src, invocation)) => {
                    auto_drafted = maybe_t.is_none() || maybe_x.is_none();
                    draft_source = src;
                    (maybe_t.unwrap_or(dt), maybe_x.unwrap_or(dx), invocation)
                }
                None => {
                    return Err(close_loop_missing_draft_error(
                        params.flow_id.as_deref(),
                        params
                            .notes
                            .as_deref()
                            .is_some_and(|n| !n.trim().is_empty()),
                    ));
                }
            }
        }
    };

    let metadata = build_close_loop_metadata(
        &issue_ref,
        &doc_paths,
        &related_issues,
        params.wiki_path.as_deref(),
        &references,
    );

    // Build the write-back comment BEFORE the wiki write moves `title`
    // and `references` into WikiWriteParams.
    let comment_body = build_closure_comment_body(
        &title,
        params.wiki_path.as_deref(),
        &doc_paths,
        &spec_paths,
        &references,
    );
    let spec_advisory = spec_advisory(&spec_paths, &doc_paths);
    let pattern_query = format!(
        "{} {} {}",
        title,
        params.wiki_summary.as_deref().unwrap_or_default(),
        text.chars().take(500).collect::<String>()
    );
    let closure_source_revision = crate::tool_params::canonical_json_sha256(&json!({
        "issue_ref": &issue_ref,
        "title": &title,
        "text": &text,
        "wiki_path": &params.wiki_path,
        "wiki_topic": &params.wiki_topic,
        "wiki_summary": &params.wiki_summary,
        "doc_paths": &doc_paths,
        "spec_paths": &spec_paths,
        "related_issues": &related_issues,
    }));

    let wiki_params = WikiWriteParams {
        title,
        text,
        path: params.wiki_path.clone(),
        topic: params.wiki_topic.clone(),
        summary: params.wiki_summary.clone(),
        category: params
            .wiki_category
            .clone()
            .unwrap_or_else(|| "experience".to_string()),
        keywords: params.wiki_keywords.clone(),
        entities: params.wiki_entities.clone(),
        importance: params.wiki_importance.unwrap_or(0.85),
        scope: params
            .wiki_scope
            .clone()
            .unwrap_or_else(|| "global".to_string()),
        retention_policy: "permanent".to_string(),
        domain: params.wiki_domain.clone(),
        project: params.project.clone(),
        metadata: Some(metadata),
        force: params.force,
        references,
        include_patterns: true,
        pattern_query: Some(pattern_query),
        pattern_top_k: Some(5),
    };
    let wiki_result = match model_invocation {
        Some(invocation) => {
            crate::copilot_ops::handle_tachi_wiki_write_with_model_invocation(
                server,
                wiki_params,
                invocation,
            )
            .await?
        }
        None => crate::copilot_ops::handle_tachi_wiki_write(server, wiki_params).await?,
    };
    let wiki_json = serde_json::from_str::<Value>(&wiki_result).unwrap_or(json!(wiki_result));
    let pattern_refs = wiki_json
        .get("pattern_refs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let pattern_feedback = if pattern_refs.is_empty() {
        json!("skipped (no pattern refs attached)")
    } else if let Some(flow_id) = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let verified_flow_revision =
            crate::task_lifecycle::verified_flow_revision(flow_id, Some(&issue_ref), None);
        let evidence_digest = crate::tool_params::canonical_json_sha256(&json!({
            "wiki_id": wiki_json.get("id"),
            "wiki_path": wiki_json.get("wiki_path"),
            "pattern_refs": &pattern_refs,
        }));
        match (
            verified_flow_revision,
            closure_source_revision,
            evidence_digest,
        ) {
            (Ok(Some(flow_revision)), Ok(closure_revision), Ok(evidence_digest)) => {
                let source_revision =
                    format!("flow:{};closure:sha256:{closure_revision}", flow_revision);
                let evidence_digest = format!("sha256:{evidence_digest}");
                crate::continuity_ops::append_pattern_evidence_for_refs(
                    server,
                    crate::continuity_ops::PatternEvidenceBatchInput {
                        project: params.project.as_deref(),
                        refs: &pattern_refs,
                        default_outcome: "hit",
                        source: crate::continuity_ops::PatternEvidenceSource::WorkflowClosure,
                        run_id: flow_id,
                        source_revision: &source_revision,
                        evidence_digest: &evidence_digest,
                    },
                )
            }
            (Ok(None), _, _) => json!({
                "status": "skipped",
                "reason": "unverified_flow_identity",
                "saved_count": 0,
                "error_count": 0,
                "events": [],
                "errors": [],
            }),
            (Err(error), _, _) => json!({
                "status": "failed",
                "reason": "flow_identity_read_failed",
                "error": error,
                "saved_count": 0,
            }),
            (_, Err(error), _) | (_, _, Err(error)) => json!({
                "status": "failed",
                "reason": "workflow_closure_evidence_digest_failed",
                "error": error,
                "saved_count": 0,
            }),
        }
    } else {
        json!({
            "status": "skipped",
            "reason": "missing_real_flow_id",
            "saved_count": 0,
            "error_count": 0,
            "events": [],
            "errors": [],
        })
    };

    // Write-back arc: post the closure comment to the source issue (and
    // PR, if given). Best-effort — a GitHub outage never fails the wiki
    // closure; the failure is recorded so the briefing can resurface it.
    let post = params.post_comment.unwrap_or(true);
    let issue_comment = if post {
        post_closure_comment(server, &issue_ref, false, &comment_body).await
    } else {
        json!({ "posted": false, "reason": "post_comment=false" })
    };
    let pr_comment = match (post, params.pr_ref.as_deref()) {
        (true, Some(pr)) if !pr.trim().is_empty() => {
            post_closure_comment(server, pr, true, &comment_body).await
        }
        _ => json!({ "posted": false, "reason": "no pr_ref or post_comment=false" }),
    };

    serde_json::to_string(&json!({
        "ok": true,
        "action": "close_loop",
        "dry_run": false,
        "issue_ref": issue_ref,
        "promotion_plan": promotion_plan,
        "wiki": wiki_json,
        "pattern_feedback": pattern_feedback,
        "closure_actions": {
            "comment_body": comment_body,
            "issue_comment": issue_comment,
            "pr_comment": pr_comment,
            "spec_advisory": spec_advisory,
            "auto_drafted": auto_drafted,
            "draft_source": draft_source,
        },
    }))
    .map_err(|e| format!("serialize close_loop: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_refs_dedup_and_order() {
        let refs = build_closure_references(
            "kckylechen1/tachi#150",
            &["docs/foo.md".to_string()],
            &["#149".to_string(), "kckylechen1/tachi#150".to_string()],
        );
        assert_eq!(
            refs,
            vec![
                "kckylechen1/tachi#150".to_string(),
                "docs/foo.md".to_string(),
                "#149".to_string(),
            ]
        );
    }
}
