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
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "issue_ref is required for close_loop".to_string())?;
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
                build_closure_references(issue_ref, &params.doc_paths, &params.related_issues);
            crate::wiki_ops::validate_references(&references)?;

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
                    force: params.force,
                    references,
                },
            )
            .await?;

            serde_json::to_string(&json!({
                "ok": true,
                "action": "close_loop",
                "issue_ref": issue_ref,
                "wiki": serde_json::from_str::<Value>(&wiki_result).unwrap_or(json!(wiki_result)),
            }))
            .map_err(|e| format!("serialize close_loop: {e}"))
        }
        "build_references" => {
            let issue_ref = params.issue_ref.as_deref().unwrap_or("");
            let references =
                build_closure_references(issue_ref, &params.doc_paths, &params.related_issues);
            crate::wiki_ops::validate_references(&references)?;
            serde_json::to_string(&json!({
                "references": references,
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
