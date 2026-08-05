pub(crate) mod route_policy;
mod recall_proposal_ops;
mod recall_simulate_ops;

use crate::tool_params::{TachiTuneAction, TachiTuneParams};
use crate::MemoryServer;
use serde_json::{json, Value};

pub(crate) use recall_proposal_ops::{
    handle_recall_config_apply, handle_recall_config_proposals, handle_recall_config_review,
};
pub(crate) use recall_simulate_ops::{
    build_recall_simulation_report, handle_tune_recall_simulate,
};

pub(crate) async fn handle_tachi_tune(
    server: &MemoryServer,
    params: TachiTuneParams,
) -> Result<String, String> {
    if !tachi_hub::tool_visible("tachi_tune", server.active_tool_profile(), None) {
        return Err(
            "tachi_tune is admin-only; route and recall tuning are not available to the active tool profile."
                .to_string(),
        );
    }

    match params.action {
        // Route tuning came from `tachi_task`, whose router shaped every arm's
        // raw JSON through `format_facade_response` on the way out (markdown
        // rendering, status/action fill). Recall tuning came from
        // `tachi_memory`, whose arms each shape their own response. Keep both
        // halves on the formatter they were moved off, so #1426 stays a move.
        TachiTuneAction::RouteSimulate
        | TachiTuneAction::RouteProposals
        | TachiTuneAction::RouteReview
        | TachiTuneAction::RouteApply => {
            let raw = handle_route_action(server, &params)?;
            format_tune_response(params.action.as_str(), raw, params.format.as_deref())
        }
        TachiTuneAction::RecallSimulate => handle_tune_recall_simulate(server, &params).await,
        TachiTuneAction::RecallProposals => handle_recall_config_proposals(server, &params).await,
        TachiTuneAction::RecallReview => handle_recall_config_review(server, &params),
        TachiTuneAction::RecallApply => handle_recall_config_apply(server, &params),
    }
}

fn handle_route_action(
    server: &MemoryServer,
    params: &TachiTuneParams,
) -> Result<String, String> {
    let raw = match params.action {
        TachiTuneAction::RouteSimulate => handle_route_simulation(server, params)?,
        TachiTuneAction::RouteProposals => route_policy::handle_route_policy_proposals(
            server,
            params.limit.unwrap_or(500),
            params.state_filter.as_deref(),
        )?,
        TachiTuneAction::RouteReview => {
            let proposal_id = params
                .proposal_id
                .as_deref()
                .ok_or_else(|| "proposal_id is required when action='route_review'".to_string())?;
            let review_status = params
                .review_status
                .as_deref()
                .ok_or_else(|| "review_status is required when action='route_review'".to_string())?;
            route_policy::handle_route_policy_review(
                server,
                proposal_id,
                review_status,
                params.notes.as_deref(),
            )?
        }
        TachiTuneAction::RouteApply => {
            let proposal_id = params
                .proposal_id
                .as_deref()
                .ok_or_else(|| "proposal_id is required when action='route_apply'".to_string())?;
            route_policy::handle_route_policy_apply(server, proposal_id, params.confirm)?
        }
        other => {
            return Err(format!(
                "handle_route_action called with non-route action '{}'",
                other.as_str()
            ))
        }
    };
    Ok(raw)
}

fn format_tune_response(
    action: &str,
    raw: String,
    format: Option<&str>,
) -> Result<String, String> {
    crate::tools::formatting::format_facade_response(
        &format!("Tachi tune {action}"),
        action,
        &raw,
        format,
        false,
        false,
    )
}

fn handle_route_simulation(
    server: &MemoryServer,
    params: &TachiTuneParams,
) -> Result<String, String> {
    let mut file_paths = params.doc_paths.clone();
    file_paths.extend(params.spec_paths.clone());
    let admission = crate::host_profile::admit_execution_level(params.execution_level);
    if !admission.allowed {
        return serde_json::to_string(&json!({
            "action": "route_simulate",
            "host_admission": admission.to_json(),
        }))
        .map_err(|err| format!("serialize host admission decline: {err}"));
    }
    let raw = route_policy::handle_route_simulation(
        server,
        params.limit.unwrap_or(500),
        params.task.as_deref(),
        params.risk.as_deref(),
        &file_paths,
    )?;
    attach_host_admission(raw, &admission)
}

fn attach_host_admission(
    raw: String,
    admission: &crate::host_profile::HostAdmission,
) -> Result<String, String> {
    let mut value: Value =
        serde_json::from_str(&raw).map_err(|err| format!("parse route payload: {err}"))?;
    let object = value.as_object_mut().ok_or_else(|| {
        "attach host admission: expected route payload to be a JSON object".to_string()
    })?;
    object.insert("host_admission".to_string(), admission.to_json());
    serde_json::to_string(&value).map_err(|err| format!("serialize host admission attach: {err}"))
}
