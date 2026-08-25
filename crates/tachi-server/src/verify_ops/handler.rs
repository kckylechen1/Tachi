use super::ledger::{read_verification_ledger, record_items};
use super::receipt::{
    recorded_check_ids, render_record_receipt, shape_record_response, shape_run_response,
    shape_status_response, validate_record_params,
};
use super::recent::recent_verification_summaries;
use super::render::{gate_for_status, render_compact_status, render_status};
use super::storage::{empty_ledger, normalize_status};
use super::{run_verification_check, *};
use crate::facade_memory_ops::wants_full_format;

pub(crate) async fn handle_tachi_verify(
    server: &MemoryServer,
    params: TachiVerifyParams,
) -> Result<String, String> {
    use crate::tool_params::TachiVerifyAction;

    let action = params.action;
    let action_str = action.as_str();
    let raw = match action {
        TachiVerifyAction::Start => {
            validate_record_params(&params)?;
            record_items(&params, "pending")?
        }
        TachiVerifyAction::Record => {
            validate_record_params(&params)?;
            let status = normalize_status(params.status.as_deref(), "passed")?;
            record_items(&params, &status)?
        }
        TachiVerifyAction::Run => {
            // #1454 slice 2: server-executed check. Only flow_id + check_kind
            // are read from the caller; argv, head_sha, exit code, and log
            // path are all produced by the executor (verify_ops::run).
            let flow_id = params
                .flow_id
                .as_deref()
                .ok_or_else(|| "flow_id is required for tachi_verify run".to_string())?;
            let check_kind = params
                .check_kind
                .as_deref()
                .ok_or_else(|| "check_kind is required for tachi_verify run".to_string())?;
            run_verification_check(server, flow_id, check_kind, params.timeout_secs).await?
        }
        TachiVerifyAction::Status | TachiVerifyAction::Board => {
            if let Some(flow_id) = params.flow_id.as_deref() {
                let ledger = read_verification_ledger(flow_id)?;
                let gate = gate_for_status(server, &params, ledger.as_ref())?;
                json!({
                    "status": "completed",
                    "action": action_str,
                    "flow_id": flow_id,
                    "verification": ledger.unwrap_or_else(|| empty_ledger(flow_id)),
                    "gate": gate,
                })
            } else {
                json!({
                    "status": "completed",
                    "action": action_str,
                    "runs": recent_verification_summaries(
                        &server.tachi_home_dir(),
                        params.limit.unwrap_or(DEFAULT_STATUS_LIMIT as u32) as usize,
                    ),
                })
            }
        }
    };

    if matches!(
        action,
        TachiVerifyAction::Start | TachiVerifyAction::Record | TachiVerifyAction::Run
    ) {
        let shaped = if matches!(action, TachiVerifyAction::Run) {
            shape_run_response(&raw, &params)
        } else {
            let recorded_ids = recorded_check_ids(&params);
            shape_record_response(&raw, &params, &recorded_ids)
        };
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

    // status / board — F1 compact by default (#527)
    let human = params.format.as_deref().is_some_and(|format| {
        matches!(
            format.to_ascii_lowercase().as_str(),
            "markdown" | "md" | "text" | "plain" | "human"
        )
    });
    if wants_full_format(params.format.as_deref()) {
        if human {
            return Ok(render_status(&raw));
        }
        // format=full → full JSON board
        return serde_json::to_string(&raw).map_err(|e| format!("serialize tachi_verify: {e}"));
    }
    let shaped = shape_status_response(&raw, &params);
    if human {
        return Ok(render_compact_status(&shaped));
    }
    // Default + format=json → compact JSON receipt (no full ledger echo).
    serde_json::to_string(&shaped).map_err(|e| format!("serialize tachi_verify: {e}"))
}
