use super::*;

pub(crate) fn format_alerts(warnings: &[String], wiki_counts: &Value) -> String {
    let mut out = vec!["## Tachi alerts".to_string(), "\n### Warnings".to_string()];
    if warnings.is_empty() {
        out.push("- No active warnings".to_string());
    } else {
        for (idx, warning) in warnings.iter().enumerate().take(12) {
            out.push(format!("{}. {warning}", idx + 1));
        }
    }

    let orphans = wiki_counts
        .get("orphans")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let stale = wiki_counts
        .get("stale_nodes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let duplicates = wiki_counts
        .get("duplicates")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if orphans + stale + duplicates > 0 {
        out.push(format!(
            "\n### Wiki hygiene\n- Orphans: {orphans} | Stale: {stale} | Duplicates: {duplicates}"
        ));
        out.push(
            "- Next: run `tachi tidy --dry-run` to inspect DB fragmentation, or use `wiki_lint`/admin profile for wiki graph cleanup before applying changes."
                .to_string(),
        );
    }

    out.join("\n")
}
