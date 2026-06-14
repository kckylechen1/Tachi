//! Issue → Doc → Memory closure helpers (#150).

use super::*;

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
                "tool": "tachi_task",
                "action": "close_loop",
                "when": "project-specific durable lesson or decision after the work is complete"
            },
            {
                "destination": "guide",
                "layer": "guide",
                "authority": "playbook",
                "tool": "tachi_task",
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

pub(crate) async fn handle_workflow(
    server: &MemoryServer,
    params: TachiWorkflowParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    match action.as_str() {
        "close_loop" => {
            let issue_ref = params
                .issue_ref
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "issue_ref is required for close_loop".to_string())?
                .to_string();
            let title = params
                .wiki_title
                .clone()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "wiki_title is required for close_loop".to_string())?;
            let text = params
                .wiki_text
                .clone()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "wiki_text is required for close_loop".to_string())?;

            let references =
                build_closure_references(&issue_ref, &params.doc_paths, &params.related_issues);
            crate::wiki_ops::validate_references(&references)?;
            let metadata = build_close_loop_metadata(
                &issue_ref,
                &params.doc_paths,
                &params.related_issues,
                params.wiki_path.as_deref(),
                &references,
            );
            let promotion_plan = build_promotion_plan(&issue_ref, &references);

            let wiki_result = crate::copilot_ops::handle_tachi_wiki_write(
                server,
                WikiWriteParams {
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
                },
            )
            .await?;

            serde_json::to_string(&json!({
                "ok": true,
                "action": "close_loop",
                "issue_ref": issue_ref,
                "promotion_plan": promotion_plan,
                "wiki": serde_json::from_str::<Value>(&wiki_result).unwrap_or(json!(wiki_result)),
            }))
            .map_err(|e| format!("serialize close_loop: {e}"))
        }
        "build_references" => {
            let issue_ref = params.issue_ref.as_deref().unwrap_or("");
            let references =
                build_closure_references(issue_ref, &params.doc_paths, &params.related_issues);
            crate::wiki_ops::validate_references(&references)?;
            let promotion_plan = build_promotion_plan(issue_ref, &references);
            serde_json::to_string(&json!({
                "references": references,
                "promotion_plan": promotion_plan,
            }))
            .map_err(|e| format!("serialize build_references: {e}"))
        }
        other => Err(format!(
            "Invalid workflow action '{other}'. Use close_loop or build_references."
        )),
    }
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
