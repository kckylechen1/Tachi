use super::ledger::{read_verification_ledger, record_items};
use super::recent::recent_verification_summaries;
use super::receipt::{recorded_check_ids, render_record_receipt, shape_record_response, validate_record_params};
use super::render::{gate_for_status, render_status};
use super::storage::{empty_ledger, normalize_status};
use super::*;
use crate::facade_memory_ops::wants_full_format;

pub(crate) async fn handle_tachi_verify(
    _server: &MemoryServer,
    params: TachiVerifyParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    let raw = match action.as_str() {
        "start" => {
            validate_record_params(&params)?;
            record_items(&params, "pending")?
        }
        "record" => {
            validate_record_params(&params)?;
            let status = normalize_status(params.status.as_deref(), "passed")?;
            record_items(&params, &status)?
        }
        "status" | "board" => {
            if let Some(flow_id) = params.flow_id.as_deref() {
                let ledger = read_verification_ledger(flow_id)?;
                let gate = gate_for_status(&params, ledger.as_ref())?;
                json!({
                    "status": "completed",
                    "action": action,
                    "flow_id": flow_id,
                    "verification": ledger.unwrap_or_else(|| empty_ledger(flow_id)),
                    "gate": gate,
                })
            } else {
                json!({
                    "status": "completed",
                    "action": action,
                    "runs": recent_verification_summaries(params.limit.unwrap_or(DEFAULT_STATUS_LIMIT as u32) as usize),
                })
            }
        }
        _ => {
            return Err(format!(
                "Invalid action '{}'. Use 'start', 'record', 'status', or 'board'.",
                params.action
            ))
        }
    };

    if matches!(action.as_str(), "start" | "record") {
        let recorded_ids = recorded_check_ids(&params);
        let shaped = shape_record_response(&raw, &params, &recorded_ids);
        if params
            .format
            .as_deref()
            .is_some_and(|format| format.eq_ignore_ascii_case("json"))
            || wants_full_format(params.format.as_deref())
        {
            return serde_json::to_string(&shaped)
                .map_err(|e| format!("serialize tachi_verify: {e}"));
        }
        return Ok(render_record_receipt(&shaped));
    }

    if params
        .format
        .as_deref()
        .is_some_and(|format| format.eq_ignore_ascii_case("json"))
    {
        serde_json::to_string(&raw).map_err(|e| format!("serialize tachi_verify: {e}"))
    } else {
        Ok(render_status(&raw))
    }
}