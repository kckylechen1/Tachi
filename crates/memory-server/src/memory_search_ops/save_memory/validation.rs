use crate::tool_params::SaveMemoryParams;
use serde_json::json;

pub(in crate::memory_search_ops::save_memory) enum SaveTextValidation {
    Accepted(Option<serde_json::Value>),
    Rejected(String),
}

fn json_response(value: serde_json::Value) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|e| format!("Failed to serialize: {}", e))
}

pub(in crate::memory_search_ops::save_memory) fn validate_save_text(
    params: &SaveMemoryParams,
    safe_text: &str,
) -> Result<SaveTextValidation, String> {
    if !params.force && memory_core::is_noise_text(safe_text) {
        return Ok(SaveTextValidation::Rejected(json_response(json!({
            "saved": false,
            "noise": true,
            "reason": "Text detected as noise (greeting, denial, or meta-question). Not saved.",
            "hint": "Retry with force=true if this is intentional content.",
        }))?));
    }

    // Capture gate (Branch #4): validate domain, path bucket, min-chars, and
    // markdown-dump heuristic. Default mode = Warn (annotate response, write
    // proceeds). TACHI_CAPTURE_GATE=enforce switches to hard rejection.
    let effective_domain = params
        .domain
        .as_deref()
        .filter(|domain| !domain.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            crate::repair::domain::repair_target(
                params.domain.as_deref(),
                &params.path,
                &params.category,
                "mcp",
            )
        });
    let gate_mode = crate::capture_gate::GateMode::from_env();
    let gate_decision = crate::capture_gate::evaluate(
        &crate::capture_gate::GateInput::new(
            safe_text,
            &params.path,
            effective_domain.as_deref(),
            params.force,
        ),
        gate_mode,
    );
    if !gate_decision.accept {
        return Ok(SaveTextValidation::Rejected(json_response(json!({
            "saved": false,
            "rejected_by": "capture_gate",
            "mode": gate_decision.mode,
            "violations": gate_decision.violations,
            "hint": "Set TACHI_CAPTURE_GATE=warn to downgrade these to warnings, or pass force=true on save.",
        }))?));
    }

    Ok(SaveTextValidation::Accepted(
        (!gate_decision.violations.is_empty()).then(|| json!(gate_decision.violations)),
    ))
}
