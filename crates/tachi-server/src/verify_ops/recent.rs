use super::storage::read_json;
use super::*;

fn parse_updated_at(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

pub(crate) fn recent_verification_summaries(limit: usize) -> Value {
    let root = flow_runs_root();
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return json!([]);
    };
    let scan_cap = limit.saturating_mul(4).clamp(32, RECENT_SCAN_MAX);
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in read_dir.flatten() {
        let ledger_path = entry.path().join(LEDGER_FILE);
        if !ledger_path.exists() {
            continue;
        }
        let modified = ledger_path
            .metadata()
            .and_then(|m| m.modified())
            .or_else(|_| entry.metadata().and_then(|m| m.modified()))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((modified, ledger_path));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.truncate(scan_cap);

    let mut rows: Vec<Value> = Vec::new();
    for (_, path) in candidates {
        let Ok(Some(ledger)) = read_json(&path) else {
            continue;
        };
        let items = ledger
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let failed = items
            .iter()
            .filter(|i| i.get("status").and_then(Value::as_str) == Some("failed"))
            .count();
        let pending = items
            .iter()
            .filter(|i| {
                i.get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| matches!(s, "pending" | "running" | "stale"))
            })
            .count();
        rows.push(json!({
            "flow_id": ledger.get("flow_id").and_then(Value::as_str).unwrap_or("?"),
            "pr_ref": ledger.get("pr_ref").cloned().unwrap_or(Value::Null),
            "head_sha": ledger.get("head_sha").cloned().unwrap_or(Value::Null),
            "overall": ledger.get("overall").and_then(Value::as_str).unwrap_or("pending"),
            "updated_at": ledger.get("updated_at").cloned().unwrap_or(Value::Null),
            "total": items.len(),
            "failed": failed,
            "pending": pending,
        }));
    }
    rows.sort_by(|a, b| parse_updated_at(b).cmp(&parse_updated_at(a)));
    rows.truncate(limit);
    json!(rows)
}
