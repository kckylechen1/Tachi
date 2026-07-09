use super::super::*;

pub(in crate::gh_ops) fn render_pr_review_digest_markdown(digest: &Value) -> String {
    let repo = digest.get("repo").and_then(Value::as_str).unwrap_or("");
    let pr_number = digest
        .get("pr_number")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let author_filter = digest
        .get("author_filter")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_REVIEW_AUTHOR_FILTER);
    let mut out = format!(
        "# PR Review Digest\n\nRepo: `{repo}`\nPR: `#{pr_number}`\nAuthor filter: `{author_filter}`\n\n\
         ## Triage Contract\n\n\
         - Mark each item as `valid`, `partially_valid`, `false_positive`, or `unresolved` before promoting.\n\
         - Store raw review artifacts here; save only distilled conclusions to memory.\n\
         - Promote repeated valid patterns to the Gemini PR review handbook or worker checklist.\n\n"
    );

    out.push_str("## Counts\n\n");
    if let Some(counts) = digest.get("counts").and_then(Value::as_object) {
        for (category, count) in counts {
            out.push_str(&format!("- `{category}`: {count}\n"));
        }
    }

    out.push_str("\n## Review Output Routing\n\n");
    if let Some(counts) = digest
        .pointer("/routing_plan/destination_counts")
        .and_then(Value::as_object)
    {
        for (destination, count) in counts {
            out.push_str(&format!("- `{destination}`: {count}\n"));
        }
    } else {
        out.push_str("- No route candidates.\n");
    }

    out.push_str("\n## Items\n\n");
    if let Some(items) = digest.get("items").and_then(Value::as_array) {
        for (idx, item) in items.iter().enumerate() {
            let category = item
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("unclassified");
            let summary = item.get("summary").and_then(Value::as_str).unwrap_or("");
            let path = item
                .pointer("/source/path")
                .and_then(Value::as_str)
                .unwrap_or("");
            let line = item.pointer("/source/line").and_then(Value::as_u64);
            let url = item
                .pointer("/source/url")
                .and_then(Value::as_str)
                .unwrap_or("");
            let future_rule = item
                .get("future_rule")
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push_str(&format!(
                "### {}. `{}`\n\nVerdict: `needs_leader_verdict`\n\n",
                idx + 1,
                category
            ));
            if !path.is_empty() {
                match line {
                    Some(line) => out.push_str(&format!("Location: `{path}:{line}`\n\n")),
                    None => out.push_str(&format!("Location: `{path}`\n\n")),
                }
            }
            if !url.is_empty() {
                out.push_str(&format!("Source: {url}\n\n"));
            }
            out.push_str(&format!("Summary: {summary}\n\n"));
            out.push_str(&format!("Future rule candidate: {future_rule}\n\n"));
            if let Some(primary) = item
                .pointer("/routing/primary_destination")
                .and_then(Value::as_str)
            {
                out.push_str(&format!("Primary route: `{primary}`\n\n"));
            }
        }
    }
    out
}
