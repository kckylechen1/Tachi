use super::super::*;

pub(in crate::gh_ops) fn review_project_base_path(layer: &str, repo: &str) -> String {
    let repo = repo.trim_matches('/');
    format!("/{layer}/projects/{repo}")
}

pub(in crate::gh_ops) fn review_route_for_item(
    repo: &str,
    pr_number: u64,
    category: &str,
    path: Option<&str>,
    summary: &str,
    future_rule: &str,
) -> Value {
    let actionable = matches!(
        category,
        "security" | "correctness" | "tests" | "api-contract"
    );
    let reusable = matches!(
        category,
        "security" | "correctness" | "tests" | "api-contract" | "maintainability"
    );
    let primary_destination = if actionable {
        "github_issue"
    } else if reusable {
        "project_wiki"
    } else {
        "pr_comment"
    };
    let mut destinations = vec![json!({
        "destination": "pr_comment",
        "layer": "github_ref",
        "authority": "project_work_record",
        "when": "reply, resolve, or mark false-positive on the PR after leader verdict",
        "target_ref": format!("{repo}#{pr_number}"),
    })];

    if actionable {
        destinations.push(json!({
            "destination": "github_issue",
            "layer": "github_ref",
            "authority": "project_work_record",
            "when": "valid actionable project bug/task remains after the PR review pass",
            "title_hint": summary,
            "source_ref": format!("{repo}#{pr_number}"),
            "path": path,
        }));
    }

    if reusable {
        destinations.push(json!({
            "destination": "feedback_rule",
            "layer": "feedback_rule",
            "authority": "behavior_patch",
            "when": "the finding is a reusable prompt/process correction for future workers",
            "path_hint": format!("{}/review/{}", review_project_base_path("feedback", repo), category),
            "rule": future_rule,
        }));
        destinations.push(json!({
            "destination": "guide",
            "layer": "guide",
            "authority": "playbook",
            "when": "the finding changes reusable AgentReview or workflow SOP",
            "path_hint": "/guide/global/workflows/agent-review",
        }));
    }

    if !matches!(category, "style" | "unclassified") {
        destinations.push(json!({
            "destination": "project_wiki",
            "layer": "wiki",
            "authority": "advisory",
            "when": "the finding is a project-specific durable lesson after close_loop",
            "path_hint": format!("{}/lessons/pr-{pr_number}", review_project_base_path("wiki", repo)),
            "source_ref": format!("{repo}#{pr_number}"),
        }));
    }

    if category == "api-contract"
        || path.is_some_and(|path| path.starts_with("docs/") || path.starts_with("spec"))
    {
        destinations.push(json!({
            "destination": "repo_doc_ref",
            "layer": "repo_doc_ref",
            "authority": "canonical",
            "when": "the accepted fix changes canonical design, API, or spec truth",
            "path": path,
        }));
    }

    destinations.push(json!({
        "destination": "eval",
        "layer": "eval",
        "authority": "evidence",
        "when": "after leader verdict, record reviewer usefulness/false-positive signal",
        "source_ref": format!("{repo}#{pr_number}"),
    }));

    json!({
        "primary_destination": primary_destination,
        "promotion_requires": "leader_verdict",
        "destinations": destinations,
    })
}

pub(in crate::gh_ops) fn review_routing_plan(items: &[Value]) -> Value {
    let mut destination_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut routed_items = Vec::new();
    for item in items {
        let Some(routing) = item.get("routing") else {
            continue;
        };
        let routes = routing
            .get("destinations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for route in &routes {
            if let Some(destination) = route.get("destination").and_then(Value::as_str) {
                *destination_counts
                    .entry(destination.to_string())
                    .or_insert(0) += 1;
            }
        }
        routed_items.push(json!({
            "category": item.get("category").cloned().unwrap_or(Value::Null),
            "summary": item.get("summary").cloned().unwrap_or(Value::Null),
            "primary_destination": routing.get("primary_destination").cloned().unwrap_or(Value::Null),
            "destinations": routes
                .iter()
                .filter_map(|route| route.get("destination").and_then(Value::as_str))
                .collect::<Vec<_>>(),
        }));
    }

    json!({
        "status": if routed_items.is_empty() { "empty" } else { "needs_leader_verdict" },
        "authority_order": [
            "github_ref",
            "repo_doc_ref",
            "wiki",
            "guide",
            "feedback_rule",
            "eval"
        ],
        "destination_counts": destination_counts,
        "items": routed_items,
    })
}
