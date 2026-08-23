use super::ledger::read_verification_ledger;
use super::receipt_store::read_run_receipts;
use super::*;
use std::path::Path;

/// #1454 F1/G2: a receipt is tree-bound when the run executed in the
/// SERVER-OWNED detached copy (`executed_in_detached_copy`), the copy was
/// checked out at the observed claim head (`source_head`), and the copy was
/// clean before AND after the run with its HEAD unmoved (`copy_head_before`
/// == `copy_head_after` == `source_head`, both clean). A receipt without the
/// G2 copy semantics — including any pre-G2 `head_before`/`clean_before`
/// shaped receipt from the broken claim-tree snapshot design — binds to
/// nothing and must never be treated as evidence for any head.
fn receipt_is_tree_bound(receipt: &Value) -> bool {
    let executed_in_detached_copy = receipt
        .get("executed_in_detached_copy")
        .and_then(Value::as_bool)
        == Some(true);
    let source_head = receipt.get("source_head").and_then(Value::as_str);
    let copy_clean_before = receipt.get("copy_clean_before").and_then(Value::as_bool);
    let copy_clean_after = receipt.get("copy_clean_after").and_then(Value::as_bool);
    let copy_head_before = receipt.get("copy_head_before").and_then(Value::as_str);
    let copy_head_after = receipt.get("copy_head_after").and_then(Value::as_str);
    executed_in_detached_copy
        && matches!(
            (
                copy_clean_before,
                copy_clean_after,
                copy_head_before,
                copy_head_after,
                source_head,
            ),
            (Some(true), Some(true), Some(before), Some(after), Some(source))
                if !source.is_empty() && before == source && after == source
        )
}

fn receipt_is_stale(receipt: &Value, head_sha: &str) -> bool {
    match receipt.get("head_sha").and_then(Value::as_str) {
        Some(receipt_sha) if !receipt_sha.is_empty() => receipt_sha != head_sha,
        _ => true,
    }
}

/// Per-kind classification of a server-run receipt against the evaluated
/// head. The receipt is the authority (F1); the ledger plays no role here.
fn classify_receipt(receipt: &Value, head_sha: &str) -> (&'static str, Option<String>) {
    // An explicitly failed receipt (tree mutation, timeout, nonzero exit)
    // is failed with its typed reason — never passed (F3).
    if receipt.get("status").and_then(Value::as_str) == Some("failed") {
        let reason = receipt
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("failed")
            .to_string();
        return ("failed", Some(reason));
    }
    if !receipt_is_tree_bound(receipt) {
        return ("failed", Some("tree_mutation_during_run".to_string()));
    }
    if receipt.get("exit_code").and_then(Value::as_i64) != Some(0) {
        return ("failed", Some("failed".to_string()));
    }
    if receipt_is_stale(receipt, head_sha) {
        return ("stale", None);
    }
    ("passed", None)
}

/// #1454 merge-authority gate.
///
/// Authority source: the server-owned receipt store only (F1). For EVERY
/// kind in [`MERGE_REQUIRED_RUN_KINDS`], `overall == "passed"` requires a
/// valid receipt — exit 0, head match, tree-bound (F3/F4) — for the
/// evaluated head. Missing kinds keep the gate `pending` with
/// `verification:<kind>:missing`.
///
/// A flow ledger whose required set is empty (all items `required:false`,
/// or a ledger with no required items) yields `not_required` WITH
/// `waiting_on: ["verification:missing"]` — the same waiting semantic as an
/// absent ledger, emitted by the gate itself under all policies (F2).
///
/// `Ok(None)` means no ledger exists for the flow; callers (safe-merge,
/// status) treat that as `verification:missing` / display `unverified`.
/// `tachi_home` is the server's global data root (receipts live under
/// `<tachi-home>/verify-receipts`).
pub(crate) fn evaluate_verification_gate(
    flow_id: Option<&str>,
    current_head_sha: &str,
    tachi_home: &Path,
) -> Result<Option<Value>, String> {
    let Some(flow_id) = flow_id else {
        return Ok(None);
    };
    let Some(ledger) = read_verification_ledger(flow_id)? else {
        return Ok(None);
    };
    let items = ledger
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let required: Vec<Value> = items
        .into_iter()
        .filter(|item| {
            item.get("required")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
        .collect();
    if required.is_empty() {
        // F2: an existing ledger whose required set is empty must not read as
        // "no verification needed" — the gate itself emits the missing reason
        // so no policy (including permissive) sees a clean pass from nothing.
        return Ok(Some(json!({
            "flow_id": flow_id,
            "overall": "not_required",
            "required_total": 0,
            "current_head_sha": current_head_sha,
            "passed": [],
            "failed": [],
            "pending": [],
            "stale": [],
            "waiting_on": ["verification:missing"],
            "reasons": [],
            "ledger_updated_at": ledger.get("updated_at").cloned().unwrap_or(Value::Null),
        })));
    }

    let receipts = read_run_receipts(tachi_home, flow_id)?;
    let mut by_kind: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for receipt in receipts {
        if let Some(kind) = receipt.get("kind").and_then(Value::as_str) {
            // Last write wins: a re-run for the same kind supersedes an
            // earlier receipt (same semantics as ledger upsert).
            by_kind.insert(kind.to_string(), receipt);
        }
    }

    let mut passed = Vec::new();
    let mut failed = Vec::new();
    let mut pending = Vec::new();
    let mut stale = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let mut waiting_on: Vec<String> = Vec::new();

    for kind in MERGE_REQUIRED_RUN_KINDS {
        let Some(receipt) = by_kind.get(*kind) else {
            pending.push((*kind).to_string());
            waiting_on.push(format!("verification:{kind}:missing"));
            continue;
        };
        let (class, reason) = classify_receipt(receipt, current_head_sha);
        match class {
            "passed" => passed.push((*kind).to_string()),
            "stale" => {
                stale.push((*kind).to_string());
                waiting_on.push(format!("verification:{kind}:stale"));
            }
            _ => {
                failed.push((*kind).to_string());
                reasons.push(format!(
                    "verification:{kind}:{}",
                    reason.unwrap_or_else(|| "failed".to_string())
                ));
            }
        }
    }

    let overall = if !failed.is_empty() {
        "failed"
    } else if !pending.is_empty() || !stale.is_empty() {
        "pending"
    } else {
        "passed"
    };

    Ok(Some(json!({
        "flow_id": flow_id,
        "overall": overall,
        "required_total": passed.len() + failed.len() + pending.len() + stale.len(),
        "current_head_sha": current_head_sha,
        "passed": passed,
        "failed": failed,
        "pending": pending,
        "stale": stale,
        "waiting_on": waiting_on,
        "reasons": reasons,
        "ledger_updated_at": ledger.get("updated_at").cloned().unwrap_or(Value::Null),
    })))
}
