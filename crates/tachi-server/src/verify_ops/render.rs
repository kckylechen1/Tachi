use super::gate::evaluate_verification_gate;
use super::*;

pub(super) fn gate_for_status(
    params: &TachiVerifyParams,
    ledger: Option<&Value>,
) -> Result<Option<Value>, String> {
    if let (Some(flow_id), Some(head_sha), Some(_)) = (
        params.flow_id.as_deref(),
        params.head_sha.as_deref(),
        ledger,
    ) {
        evaluate_verification_gate(Some(flow_id), head_sha)
    } else {
        Ok(None)
    }
}

pub(super) fn render_status(value: &Value) -> String {
    if let Some(rows) = value.get("runs").and_then(Value::as_array) {
        let mut out = vec!["## Tachi verify board".to_string()];
        if rows.is_empty() {
            out.push("_No verification ledgers found._".to_string());
        } else {
            for row in rows {
                let flow_id = row.get("flow_id").and_then(Value::as_str).unwrap_or("?");
                let overall = row
                    .get("overall")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                let total = row.get("total").and_then(Value::as_u64).unwrap_or(0);
                let failed = row.get("failed").and_then(Value::as_u64).unwrap_or(0);
                let pending = row.get("pending").and_then(Value::as_u64).unwrap_or(0);
                let pr = row.get("pr_ref").and_then(Value::as_str).unwrap_or("");
                out.push(format!(
                    "- [{overall}] `{flow_id}`{} checks={total} failed={failed} pending={pending}",
                    if pr.is_empty() {
                        String::new()
                    } else {
                        format!(" `{pr}`")
                    }
                ));
            }
        }
        return out.join("\n");
    }

    let ledger = value.get("verification").unwrap_or(value);
    let flow_id = ledger.get("flow_id").and_then(Value::as_str).unwrap_or("?");
    let overall = ledger
        .get("overall")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let mut out = vec![
        "## Tachi verify status".to_string(),
        format!("flow_id: `{flow_id}`"),
        format!("overall: `{overall}`"),
    ];
    if let Some(gate) = value.get("gate") {
        out.push(format!(
            "gate: `{}`",
            gate.get("overall")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ));
    }
    if let Some(items) = ledger.get("items").and_then(Value::as_array) {
        for item in items {
            let id = item.get("id").and_then(Value::as_str).unwrap_or("check");
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let summary = item.get("summary").and_then(Value::as_str).unwrap_or("");
            out.push(format!(
                "- [{status}] `{id}`{}",
                if summary.is_empty() {
                    String::new()
                } else {
                    format!(" - {summary}")
                }
            ));
        }
    }
    out.join("\n")
}
