use super::gate::evaluate_verification_gate;
use super::receipt_store::best_receipt_head;
use super::storage::{markup_status, read_json};
use super::*;
use std::path::Path;

fn parse_updated_at(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// #1454 F6: board rows publish the authority-aware gate verdict, never the
/// raw ledger `overall`. Per flow: best server-known head = the receipt-store
/// head; a flow with no receipts has no server-known head → the row verdict
/// is `unverified` (fail-closed display). Ledger counts stay visible as
/// detail rows; a gate evaluation error degrades to `unverified` for that
/// row (a board must not fabricate readiness from a broken store).
///
/// #1454 F6-adjudication: the gate verdict stays PRIMARY, but a caller-
/// asserted ledger `overall` that EXISTS and diverges from it is real signal
/// and must not be erased from the display. Each row therefore also carries
/// `overall_display` — the gate verdict alone when the caller-asserted value
/// is absent or agrees, else `"{gate} (caller-asserted: {ledger_overall})"`
/// (e.g. `unverified (caller-asserted: failed)`). Renderers use
/// `overall_display`; JSON consumers keep `overall` as the machine verdict.
pub(crate) fn recent_verification_summaries(tachi_home: &Path, limit: usize) -> Value {
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
        let flow_id = ledger.get("flow_id").and_then(Value::as_str).unwrap_or("?");
        // Authority verdict (F6): receipt-store head → gate; none → unverified.
        let verdict = match best_receipt_head(tachi_home, flow_id) {
            Some(head) => evaluate_verification_gate(Some(flow_id), &head, tachi_home)
                .ok()
                .flatten()
                .and_then(|gate| {
                    gate.get("overall")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "unverified".to_string()),
            None => "unverified".to_string(),
        };
        // F6-adjudication: a caller-asserted ledger `overall` that EXISTS and
        // diverges from the gate verdict is surfaced as a marker on the
        // display string (never folded into the machine `overall`).
        //
        // #1454 H2: the caller-asserted value is normalized against the
        // closed vocabulary at the READ boundary — a crafted ledger
        // `overall` (e.g. `]\n- [passed] ...`) renders as the fixed
        // `invalid` marker and can never mint a new board line.
        let ledger_overall = ledger
            .get("overall")
            .and_then(Value::as_str)
            .map(markup_status);
        let overall_display = match ledger_overall.as_deref() {
            Some(caller) if caller != verdict.as_str() => {
                format!("{verdict} (caller-asserted: {caller})")
            }
            _ => verdict.clone(),
        };
        rows.push(json!({
            "flow_id": flow_id,
            "pr_ref": ledger.get("pr_ref").cloned().unwrap_or(Value::Null),
            "head_sha": ledger.get("head_sha").cloned().unwrap_or(Value::Null),
            "overall": verdict,
            "overall_display": overall_display,
            "ledger_overall": ledger_overall.unwrap_or_else(|| "pending".to_string()),
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
