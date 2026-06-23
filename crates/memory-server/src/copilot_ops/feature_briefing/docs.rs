use super::super::*;

pub(super) fn project_work_records(params: &TachiTaskParams) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    push_project_work_record(
        &mut out,
        &mut seen,
        "github_issue",
        params.issue_ref.as_deref(),
    );
    push_project_work_record(&mut out, &mut seen, "github_pr", params.pr_ref.as_deref());
    out
}

pub(super) fn push_project_work_record(
    out: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    kind: &str,
    raw_ref: Option<&str>,
) {
    let Some(reference) = raw_ref.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if !seen.insert(format!("{kind}:{reference}")) {
        return;
    }
    out.push(json!({
        "kind": kind,
        "ref": reference,
        "layer": "github_ref",
        "authority": "project_work_record",
        "source_of_truth": true,
        "status": "ref_only",
        "retrieval": "Call tachi_task(action='intake') for issue snapshots or tachi_task(action='link_pr'/'pr_status') for PR state.",
    }));
}

pub(super) fn build_feature_doc_index(
    project_work_record: &[Value],
    canonical_docs: &[Value],
    wiki_hits: &[Value],
    guide_hits: &[Value],
    feedback_rules: &Value,
    eval_evidence: &[Value],
    run_artifacts: &[Value],
) -> Value {
    let feedback_items = feedback_rules
        .get("rules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "authority_order": [
            "project_work_record",
            "canonical",
            "project_wiki",
            "global_guide",
            "feedback_rule",
            "eval",
            "runtime_artifact"
        ],
        "groups": [
            doc_index_group(
                "project_work_record",
                "github_ref",
                "project_work_record",
                "GitHub Issues/PRs remain the source of truth for active project work.",
                project_work_record,
            ),
            doc_index_group(
                "canonical_docs",
                "repo_doc_ref",
                "canonical",
                "Repo docs/specs define accepted design and API truth.",
                canonical_docs,
            ),
            doc_index_group(
                "project_wiki",
                "wiki",
                "advisory",
                "Project decisions and lessons are durable but do not override GitHub or repo docs.",
                wiki_hits,
            ),
            doc_index_group(
                "global_guide",
                "guide",
                "playbook",
                "Global guide entries apply by task_type/profile/stage as reusable workflow playbooks.",
                guide_hits,
            ),
            doc_index_group(
                "feedback_rules",
                "feedback_rule",
                "behavior_patch",
                "Feedback rules patch future agent behavior; they are not project facts.",
                &feedback_items,
            ),
            doc_index_group(
                "eval_evidence",
                "eval",
                "evidence",
                "Eval rows and reviewer findings are evidence for routing and verification.",
                eval_evidence,
            ),
            doc_index_group(
                "runtime_artifacts",
                "runtime_artifact",
                "runtime_state",
                "Arena/dispatch/run artifacts describe execution state and handoffs.",
                run_artifacts,
            ),
        ],
    })
}

pub(super) fn doc_index_group(
    name: &str,
    layer: &str,
    authority: &str,
    rule: &str,
    items: &[Value],
) -> Value {
    json!({
        "name": name,
        "layer": layer,
        "authority": authority,
        "rule": rule,
        "count": items.len(),
        "items": items,
    })
}

pub(super) fn feature_briefing_query(params: &TachiTaskParams) -> String {
    params
        .task
        .as_deref()
        .or(params.issue_ref.as_deref())
        .or(params.pr_ref.as_deref())
        .or(params.flow_id.as_deref())
        .unwrap_or("current feature handoff")
        .to_string()
}

pub(super) fn canonical_doc_refs(params: &TachiTaskParams) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if let Ok((flow_docs, flow_specs)) =
        crate::task_lifecycle::flow_status_doc_refs(params.flow_id.as_deref())
    {
        for path in flow_specs {
            push_doc_ref(
                &mut out,
                &mut seen,
                "flow_spec",
                &path,
                params.cwd.as_deref(),
            );
        }
        for path in flow_docs {
            push_doc_ref(
                &mut out,
                &mut seen,
                "flow_doc",
                &path,
                params.cwd.as_deref(),
            );
        }
    }
    for path in params
        .spec_paths
        .iter()
        .map(|path| ("spec", path))
        .chain(params.doc_paths.iter().map(|path| ("doc", path)))
    {
        push_doc_ref(&mut out, &mut seen, path.0, path.1, params.cwd.as_deref());
    }
    if let Some(task) = params.task.as_deref() {
        for path in extract_markdown_paths(task) {
            push_doc_ref(
                &mut out,
                &mut seen,
                "mentioned_doc",
                &path,
                params.cwd.as_deref(),
            );
        }
    }
    out
}

pub(super) fn push_doc_ref(
    out: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    kind: &str,
    raw_path: &str,
    cwd: Option<&str>,
) {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() || !seen.insert(raw_path.to_string()) {
        return;
    }
    let resolved = resolve_workspace_path(raw_path, cwd);
    out.push(json!({
        "kind": kind,
        "path": raw_path,
        "exists": resolved.as_ref().is_some_and(|path| path.exists()),
        "resolved_path": resolved.map(|path| path.to_string_lossy().to_string()),
        "layer": "repo_doc_ref",
        "authority": "canonical",
        "source_of_truth": true,
    }));
}

pub(super) fn extract_markdown_paths(text: &str) -> Vec<String> {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ')' | '(' | '[' | ']'))
        .map(|token| token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | ':' | ';')))
        .filter(|token| {
            token.ends_with(".md") && (token.starts_with("docs/") || token.contains("/docs/"))
        })
        .map(str::to_string)
        .collect()
}

pub(super) fn resolve_workspace_path(raw_path: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let path = PathBuf::from(raw_path);
    if path.is_absolute() {
        return Some(path);
    }
    if let Some(cwd) = cwd {
        let cwd = Path::new(cwd);
        for ancestor in cwd.ancestors() {
            let candidate = ancestor.join(raw_path);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        return Some(cwd.join(raw_path));
    }
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join(raw_path);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    Some(cwd.join(raw_path))
}
