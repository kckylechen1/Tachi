use crate::tool_params::TachiVerifyParams;
use crate::MemoryServer;
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

pub(in crate::bootstrap::poke_cli) async fn probe_verify_ledger(
    server: &MemoryServer,
) -> Result<Value, String> {
    let flow_id = format!(
        "flow_{}_poke_verify_{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().as_simple().to_string()[..8]
    );
    let unrelated_flow_id = format!(
        "flow_{}_poke_unrelated_{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().as_simple().to_string()[..8]
    );
    use crate::tool_params::TachiVerifyAction;
    let record = server
        .tachi_verify(Parameters(TachiVerifyParams {
            action: TachiVerifyAction::Record,
            format: Some("json".to_string()),
            flow_id: Some(flow_id.clone()),
            pr_ref: None,
            head_sha: Some("poke-head".to_string()),
            check_id: Some("poke-ledger".to_string()),
            kind: Some("custom".to_string()),
            command: Some("poke ledger probe".to_string()),
            commands: Vec::new(),
            status: Some("passed".to_string()),
            exit_code: Some(0),
            log_path: None,
            summary: Some("poke ledger passed".to_string()),
            cwd: None,
            required: Some(true),
            limit: None,
            checks: vec![],
        }))
        .await?;
    let status = server
        .tachi_verify(Parameters(TachiVerifyParams {
            action: TachiVerifyAction::Status,
            format: Some("json".to_string()),
            flow_id: Some(flow_id.clone()),
            pr_ref: None,
            head_sha: Some("poke-head".to_string()),
            check_id: None,
            kind: None,
            command: None,
            commands: Vec::new(),
            status: None,
            exit_code: None,
            log_path: None,
            summary: None,
            cwd: None,
            required: None,
            limit: None,
            checks: vec![],
        }))
        .await?;
    let unrelated = server
        .tachi_verify(Parameters(TachiVerifyParams {
            action: TachiVerifyAction::Status,
            format: Some("json".to_string()),
            flow_id: Some(unrelated_flow_id.clone()),
            pr_ref: None,
            head_sha: Some("poke-head".to_string()),
            check_id: None,
            kind: None,
            command: None,
            commands: Vec::new(),
            status: None,
            exit_code: None,
            log_path: None,
            summary: None,
            cwd: None,
            required: None,
            limit: None,
            checks: vec![],
        }))
        .await?;
    let record_json: Value =
        serde_json::from_str(&record).map_err(|e| format!("parse verify record: {e}"))?;
    let status_json: Value =
        serde_json::from_str(&status).map_err(|e| format!("parse verify status: {e}"))?;
    let unrelated_json: Value =
        serde_json::from_str(&unrelated).map_err(|e| format!("parse unrelated verify: {e}"))?;
    let overall = status_json
        .get("overall")
        .or_else(|| {
            status_json
                .get("verification")
                .and_then(|value| value.get("overall"))
        })
        .and_then(Value::as_str);
    let gate_overall = status_json
        .get("gate")
        .and_then(|value| value.get("overall"))
        .and_then(Value::as_str);
    let unrelated_items = unrelated_json
        .get("counts")
        .and_then(|value| value.get("total"))
        .and_then(Value::as_u64)
        .map(|count| count as usize)
        .or_else(|| {
            unrelated_json
                .get("verification")
                .and_then(|value| value.get("items"))
                .and_then(Value::as_array)
                .map(Vec::len)
        })
        .unwrap_or(0);
    if overall != Some("passed") || gate_overall != Some("passed") || unrelated_items != 0 {
        return Err(format!(
            "verification ledger probe failed: overall={overall:?} gate_overall={gate_overall:?} unrelated_items={unrelated_items} status={status_json} unrelated={unrelated_json}"
        ));
    }
    Ok(json!({
        "name": "verify_ledger",
        "status": "passed",
        "expected": "current flow verification passes and unrelated flow evidence is not reused",
        "observed": {
            "flow_id": flow_id,
            "record": record_json,
            "overall": overall,
            "unrelated_flow_id": unrelated_flow_id,
            "unrelated_items": unrelated_items,
        },
        "repro_steps": [
            "tachi_verify record flow_id=<current>",
            "tachi_verify status flow_id=<current>",
            "tachi_verify status flow_id=<unrelated>"
        ],
    }))
}
