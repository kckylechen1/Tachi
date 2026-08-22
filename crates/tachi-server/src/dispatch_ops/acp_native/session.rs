use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};

use super::{NativeAcpPromptOutcome, NativeAcpRunSpec, NativeAcpSession, ACP_SESSION_SCHEMA};
use tachi_params::{ResolvedStaffAssignment, StaffAssignmentRequest};

pub(super) fn resolve_native_acp_session(
    request: &StaffAssignmentRequest,
    assignment: &ResolvedStaffAssignment,
) -> Result<NativeAcpSession, String> {
    if let Ok(explicit) = std::env::var("TACHI_ACP_NATIVE_SESSION") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Ok(NativeAcpSession {
                name: validate_session_name(explicit)?,
                source: "env:TACHI_ACP_NATIVE_SESSION",
            });
        }
    }
    if let Ok(explicit) = std::env::var("TACHI_ACP_SESSION") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Ok(NativeAcpSession {
                name: validate_session_name(explicit)?,
                source: "env:TACHI_ACP_SESSION",
            });
        }
    }

    for (value, source) in [
        (assignment.selected_profile.as_deref(), "dispatch_profile"),
        (request.stage.as_deref(), "stage"),
    ] {
        if let Some(session) = value.and_then(derive_session_from_card_hint) {
            return Ok(NativeAcpSession {
                name: session,
                source,
            });
        }
    }

    Ok(NativeAcpSession {
        name: "scv".to_string(),
        source: "default_builder",
    })
}

fn derive_session_from_card_hint(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.is_empty() {
        None
    } else if lower.contains("poke") || lower.contains("probe") || lower.contains("explore") {
        Some("poke".to_string())
    } else if lower.contains("raven")
        || lower.contains("review")
        || lower.contains("verify")
        || lower.contains("verifier")
    {
        Some("raven".to_string())
    } else if lower.contains("medic") || lower.contains("hotfix") {
        Some("scv-medic".to_string())
    } else if lower.contains("scv")
        || lower.contains("impl")
        || lower.contains("execute")
        || lower.contains("builder")
    {
        Some("scv".to_string())
    } else {
        None
    }
}

fn validate_session_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("native ACP session name cannot be empty".to_string());
    }
    if trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        Ok(trimmed.to_string())
    } else {
        Err(format!(
            "invalid native ACP session name '{trimmed}'. Use only ASCII letters, numbers, '-', '_', or '.'."
        ))
    }
}

pub(super) fn native_acp_session_key(
    agent: &str,
    command: &str,
    args: &[String],
    cwd: &Path,
    session: Option<&NativeAcpSession>,
) -> Value {
    json!({
        "agent": agent,
        "command": command,
        "args": args,
        "cwd": cwd.to_string_lossy(),
        "name": session.map(|session| session.name.clone()),
    })
}

pub(super) fn read_stored_acp_session_id(record_path: &Path) -> Option<String> {
    let record = crate::task_lifecycle::read_json_file(record_path)
        .ok()
        .flatten()?;
    if record
        .get("closed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    record
        .get("acp_session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(super) fn write_native_acp_session_record(
    spec: &NativeAcpRunSpec,
    record_path: &Path,
    distill_path: Option<&PathBuf>,
    outcome: &NativeAcpPromptOutcome,
    stream_path: &Path,
    dispatch_id: &str,
) -> Result<(), String> {
    if let Some(parent) = record_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create native ACP session dir: {err}"))?;
    }
    let existing = crate::task_lifecycle::read_json_file(record_path)
        .ok()
        .flatten();
    let now = Utc::now().to_rfc3339();
    let created_at = existing
        .as_ref()
        .and_then(|value| value.get("created_at"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| now.clone());
    let session_key = native_acp_session_key(
        spec.metadata
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        &spec.command,
        &spec.args,
        &spec.cwd,
        spec.session.as_ref(),
    );
    let record = json!({
        "schema": ACP_SESSION_SCHEMA,
        "session_key": session_key,
        "acp_session_id": outcome.session_id,
        "agent_session_id": outcome.agent_session_id,
        "created_at": created_at,
        "last_used_at": now,
        "last_dispatch_id": dispatch_id,
        "closed": false,
        "event_log": {
            "latest_run_stream": stream_path.to_string_lossy(),
            "raw_message_count": outcome.raw_messages.len(),
        },
        "last_prompt_result": outcome.prompt_result,
        "last_output_preview": crate::dispatch_ops::subprocess::tail_chars(&outcome.output, 2000),
        "distill_path": distill_path.map(|path| path.to_string_lossy().to_string()),
    });
    let body = serde_json::to_vec_pretty(&record)
        .map_err(|err| format!("serialize native ACP session record: {err}"))?;
    crate::utils::write_owner_only_file_atomic(record_path, &body)
        .map_err(|err| format!("write native ACP session record: {err}"))?;

    if let Some(distill_path) = distill_path {
        let distill = format!(
            "# Tachi ACP Session\n\n- schema: {ACP_SESSION_SCHEMA}\n- name: {}\n- acp_session_id: {}\n- agent_session_id: {}\n- command: {} {}\n- cwd: {}\n- last_dispatch_id: {}\n- latest_run_stream: {}\n\n## Last Output\n\n{}\n",
            spec.session
                .as_ref()
                .map(|session| session.name.as_str())
                .unwrap_or("oneshot"),
            outcome.session_id,
            outcome.agent_session_id.as_deref().unwrap_or("none"),
            spec.command,
            spec.args.join(" "),
            spec.cwd.to_string_lossy(),
            dispatch_id,
            stream_path.to_string_lossy(),
            outcome.output.trim(),
        );
        crate::utils::write_owner_only_file_atomic(distill_path, distill.as_bytes())
            .map_err(|err| format!("write native ACP session distill: {err}"))?;
    }
    Ok(())
}
