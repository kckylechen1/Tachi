use super::super::*;
use super::classify::{
    author_matches_filter, classify_review_comment, comment_text, first_meaningful_line,
    infer_future_rule,
};
use super::routing::{review_route_for_item, review_routing_plan};

pub(in crate::gh_ops) fn build_pr_review_digest(
    repo: &str,
    pr_number: u64,
    author_filter: &str,
    comments: &[Value],
) -> Value {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut items = Vec::new();
    let mut memory_candidates = Vec::new();
    let mut handbook_candidates = Vec::new();
    let lower_filter = author_filter.trim().to_ascii_lowercase();

    for comment in comments
        .iter()
        .filter(|comment| author_matches_filter(comment, &lower_filter))
    {
        let body = comment_text(comment, "body").unwrap_or_default();
        if body.is_empty() {
            continue;
        }
        let path = comment_text(comment, "path");
        let category = classify_review_comment(&body, path.as_deref());
        *counts.entry(category.to_string()).or_insert(0) += 1;
        let summary = first_meaningful_line(&body);
        let future_rule = infer_future_rule(category, path.as_deref(), &body);
        let routing = review_route_for_item(
            repo,
            pr_number,
            category,
            path.as_deref(),
            &summary,
            &future_rule,
        );
        let source = json!({
            "kind": comment.get("kind").cloned().unwrap_or(Value::Null),
            "id": comment.get("id").cloned().unwrap_or(Value::Null),
            "review_id": comment.get("review_id").cloned().unwrap_or(Value::Null),
            "author": comment.get("author").cloned().unwrap_or(Value::Null),
            "path": path,
            "line": comment.get("line").cloned().unwrap_or(Value::Null),
            "url": comment.get("url").cloned().unwrap_or(Value::Null),
            "created_at": comment.get("created_at").cloned().unwrap_or(Value::Null),
        });
        let item = json!({
            "source": source,
            "category": category,
            "verdict": "needs_leader_verdict",
            "summary": summary,
            "body": body,
            "future_rule": future_rule,
            "routing": routing,
        });

        memory_candidates.push(json!({
            "source": "github_pr_review",
            "repo": repo,
            "pr_number": pr_number,
            "category": category,
            "verdict": "needs_leader_verdict",
            "comment_id": item["source"]["id"],
            "path": item["source"]["path"],
            "line": item["source"]["line"],
            "summary": item["summary"],
            "future_rule": item["future_rule"],
        }));
        if !matches!(category, "style" | "unclassified") {
            handbook_candidates.push(json!({
                "category": category,
                "requires_verdict": true,
                "rule": item["future_rule"],
                "source": {
                    "repo": repo,
                    "pr_number": pr_number,
                    "comment_id": item["source"]["id"],
                    "path": item["source"]["path"],
                    "line": item["source"]["line"],
                    "url": item["source"]["url"],
                },
            }));
        }
        items.push(item);
    }

    let routing_plan = review_routing_plan(&items);

    json!({
        "repo": repo,
        "pr_number": pr_number,
        "author_filter": author_filter,
        "comment_count": items.len(),
        "counts": counts,
        "items": items,
        "routing_plan": routing_plan,
        "memory_candidates": memory_candidates,
        "handbook_candidates": handbook_candidates,
        "promotion_policy": {
            "raw": "keep raw/digest artifacts as evidence",
            "memory": "promote only valid or useful false-positive cases after leader verdict",
            "wiki": "promote repeated valid patterns into handbook/checklist rules",
        },
    })
}
