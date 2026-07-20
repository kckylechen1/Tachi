//! Evidence-backed RecallConfig proposal review/apply loop.

use super::evidence_format::{json_string, wants_json};
use super::recall_simulate_ops::build_recall_simulation_report;
use crate::tool_params::*;
use crate::MemoryServer;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const RECALL_CONFIG_PROPOSAL_NS: &str = "recall_config_proposals";
const EPSILON: f64 = 0.000_001;

pub(crate) async fn handle_recall_config_proposals(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let generated = if has_eval_input(params) {
        let simulation = build_recall_simulation_report(server, params).await?;
        let proposals = build_proposals_from_simulation(&simulation, params.force)?;
        persist_generated_proposals(server, proposals)?;
        Some(simulation)
    } else {
        None
    };

    let proposals = list_proposals(server, params.state_filter.as_deref())?;
    let response = json!({
        "status": "completed",
        "action": "recall_proposals",
        "kind": "recall_config",
        "read_only": false,
        "requires_human_approval": true,
        "generated": generated.as_ref().map(|simulation| json!({
            "source": "recall_simulate",
            "case_count": simulation["case_count"],
            "top_k": simulation["top_k"],
            "rerank": simulation["rerank"],
        })),
        "count": proposals.len(),
        "proposals": proposals,
        "next_actions": [
            "tachi_memory(action='review_recall_proposal', proposal_id=..., review_status='approved')",
            "tachi_memory(action='apply_recall_proposals', proposal_id=..., confirm=true) after approval; restart daemon to load new RecallConfig"
        ],
    });

    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format_recall_proposals_markdown(&response))
}

pub(crate) fn handle_recall_config_review(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let proposal_id = required_proposal_id(params)?;
    let status = match params
        .review_status
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "approved" | "approve" => "approved",
        "rejected" | "reject" => "rejected",
        other => {
            return Err(format!(
                "Invalid review_status '{}'. Expected approved|rejected",
                other
            ))
        }
    };
    let reviewed_at = Utc::now().to_rfc3339();
    let updated = server.with_global_store(|store| {
        let (raw, _version) = store
            .get_state_kv(RECALL_CONFIG_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load recall config proposal: {e}"))?
            .ok_or_else(|| format!("recall config proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse recall config proposal: {e}"))?;
        value["status"] = json!(status);
        value["review"] = json!({
            "status": status,
            "note": params.notes.clone(),
            "reviewed_at": reviewed_at,
        });
        // `hard_state` TTL (#1342 follow-up): `rejected` is terminal — the
        // proposal will never be applied — so it gets a 30-day TTL here.
        // `approved` is NOT terminal (still awaits `handle_recall_config_apply`),
        // so it must stay TTL-less until that terminal write.
        if status == "rejected" {
            value["expires_at"] = json!((Utc::now() + Duration::days(30)).to_rfc3339());
        }
        let next = serde_json::to_string(&value)
            .map_err(|e| format!("serialize recall config review: {e}"))?;
        store
            .set_state(RECALL_CONFIG_PROPOSAL_NS, proposal_id, &next)
            .map_err(|e| format!("persist recall config review: {e}"))?;
        Ok(value)
    })?;

    let response = json!({
        "status": "completed",
        "action": "review_recall_proposal",
        "proposal_id": proposal_id,
        "proposal": updated,
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format!(
        "Tachi recall proposal review\nstatus: completed\nproposal_id: `{proposal_id}`\nreview_status: {status}"
    ))
}

pub(crate) fn handle_recall_config_apply(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let proposal_id = required_proposal_id(params)?;
    if !params.confirm {
        return Err(
            "apply_recall_proposals requires confirm=true after human approval; no config.env changes applied"
                .to_string(),
        );
    }

    let raw = server.with_global_store_read(|store| {
        store
            .get_state_kv(RECALL_CONFIG_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load recall config proposal: {e}"))?
            .map(|(raw, _version)| raw)
            .ok_or_else(|| format!("recall config proposal not found: {proposal_id}"))
    })?;
    let mut proposal: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse recall config proposal: {e}"))?;
    let status = proposal
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    if status != "approved" {
        return Err(format!(
            "recall config proposal {proposal_id} must be approved before apply; current status={status}"
        ));
    }
    let config_env = parse_config_env_patch(&proposal)?;
    if config_env.is_empty() {
        return Err(format!(
            "recall config proposal {proposal_id} has no TACHI_RECALL_* config_env values"
        ));
    }

    let app_home = crate::cli_client::app_home_from_global_db(&server.global_db_path_buf());
    let config_env_path = app_home.join("config.env");
    upsert_recall_config_env(&config_env_path, &config_env)?;

    let applied_at = Utc::now().to_rfc3339();
    proposal["status"] = json!("applied");
    proposal["applied_at"] = json!(applied_at);
    // `hard_state` TTL (#1342 follow-up): `applied` is terminal — the config
    // patch already landed — so this write gets a 30-day TTL.
    proposal["expires_at"] = json!((Utc::now() + Duration::days(30)).to_rfc3339());
    proposal["apply_result"] = json!({
        "config_env_path": config_env_path.display().to_string(),
        "updated_keys": config_env.keys().cloned().collect::<Vec<_>>(),
        "restart_required": true,
        "note": "RecallConfig is loaded once at process startup; restart the daemon/MCP server to apply these values.",
    });
    let proposal_for_store = proposal.clone();
    server.with_global_store(|store| {
        let next = serde_json::to_string(&proposal_for_store)
            .map_err(|e| format!("serialize applied recall config proposal: {e}"))?;
        store
            .set_state(RECALL_CONFIG_PROPOSAL_NS, proposal_id, &next)
            .map_err(|e| format!("persist applied recall config proposal: {e}"))
    })?;

    let response = json!({
        "status": "completed",
        "action": "apply_recall_proposals",
        "proposal_id": proposal_id,
        "config_env_path": config_env_path.display().to_string(),
        "updated_keys": config_env.keys().cloned().collect::<Vec<_>>(),
        "restart_required": true,
        "proposal": proposal,
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format!(
        "Tachi recall proposal apply\nstatus: completed\nproposal_id: `{proposal_id}`\nupdated_keys: {}\nrestart_required: true",
        config_env.keys().cloned().collect::<Vec<_>>().join(", ")
    ))
}

fn has_eval_input(params: &TachiMemoryParams) -> bool {
    params
        .text
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
        || params.metadata.as_ref().is_some_and(|value| match value {
            Value::Array(items) => !items.is_empty(),
            Value::Object(map) => {
                map.contains_key("cases")
                    || map.contains_key("eval_cases")
                    || map.contains_key("case")
            }
            _ => false,
        })
}

fn build_proposals_from_simulation(
    simulation: &Value,
    include_non_improving: bool,
) -> Result<Vec<Value>, String> {
    let variants = simulation["variants"]
        .as_array()
        .ok_or_else(|| "recall simulation response missing variants".to_string())?;
    let Some(current) = variants
        .iter()
        .find(|variant| variant.get("name").and_then(Value::as_str) == Some("current"))
    else {
        return Err("recall simulation response missing current variant".to_string());
    };

    let current_recall = metric(current, "recall_at_k");
    let current_mrr = metric(current, "mrr");
    let mut out = Vec::new();
    for variant in variants {
        let name = variant
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("variant");
        if name == "current" {
            continue;
        }
        let proposed_recall = metric(variant, "recall_at_k");
        let proposed_mrr = metric(variant, "mrr");
        let recall_delta = proposed_recall - current_recall;
        let mrr_delta = proposed_mrr - current_mrr;
        let improved = variant_improves(recall_delta, mrr_delta);
        let config_env = variant
            .get("config_env")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if config_env.is_empty() || (!improved && !include_non_improving) {
            continue;
        }
        let config_env_value = Value::Object(config_env.clone());
        let id = format!(
            "recall_config:{}:{}",
            sanitize_key(name),
            stable_hex_hash(&config_env_value.to_string())
        );
        let recommendation = if improved {
            "metric_improved"
        } else {
            "forced_candidate"
        };
        let rationale = if improved {
            format!(
                "Recall replay variant {name} improved recall_at_k by {:.3} and MRR by {:.3}.",
                recall_delta, mrr_delta
            )
        } else {
            format!(
                "Recall replay variant {name} was forced into the review queue with recall_at_k delta {:.3} and MRR delta {:.3}.",
                recall_delta, mrr_delta
            )
        };
        out.push(json!({
            "proposal_id": id,
            "kind": "recall_config",
            "status": "pending",
            "requires_human_approval": true,
            "created_or_refreshed_at": Utc::now().to_rfc3339(),
            "variant": name,
            "config_env": config_env_value,
            "baseline_metrics": current["metrics"],
            "proposed_metrics": variant["metrics"],
            "metric_delta": {
                "recall_at_k": round6(recall_delta),
                "mrr": round6(mrr_delta),
            },
            "recommendation": recommendation,
            "evidence": {
                "source": "recall_simulate",
                "case_count": simulation["case_count"],
                "top_k": simulation["top_k"],
                "rerank": simulation["rerank"],
                "variant_cases": variant["cases"],
            },
            "apply": {
                "target": "~/.tachi/config.env or TACHI_HOME/config.env",
                "restart_required": true,
            },
            "rationale": rationale,
        }));
    }
    Ok(out)
}

fn persist_generated_proposals(server: &MemoryServer, proposals: Vec<Value>) -> Result<(), String> {
    if proposals.is_empty() {
        return Ok(());
    }
    server.with_global_store(|store| {
        for proposal in proposals {
            let id = proposal["proposal_id"]
                .as_str()
                .ok_or_else(|| "recall config proposal missing proposal_id".to_string())?
                .to_string();
            let mut next = proposal;
            if let Some((existing, _version)) =
                store
                    .get_state_kv(RECALL_CONFIG_PROPOSAL_NS, &id)
                    .map_err(|e| format!("load recall config proposal: {e}"))?
            {
                if let Ok(existing_json) = serde_json::from_str::<Value>(&existing) {
                    let existing_status = existing_json
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("pending");
                    if existing_status != "pending" {
                        next["status"] = json!(existing_status);
                    }
                    if let Some(review) = existing_json.get("review") {
                        next["review"] = review.clone();
                    }
                    if let Some(applied_at) = existing_json.get("applied_at") {
                        next["applied_at"] = applied_at.clone();
                    }
                    if let Some(apply_result) = existing_json.get("apply_result") {
                        next["apply_result"] = apply_result.clone();
                    }
                }
            }
            let raw = serde_json::to_string(&next)
                .map_err(|e| format!("serialize recall config proposal: {e}"))?;
            store
                .set_state(RECALL_CONFIG_PROPOSAL_NS, &id, &raw)
                .map_err(|e| format!("persist recall config proposal: {e}"))?;
        }
        Ok(())
    })
}

fn list_proposals(
    server: &MemoryServer,
    status_filter: Option<&str>,
) -> Result<Vec<Value>, String> {
    let desired = status_filter
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all")
        .map(|value| value.to_ascii_lowercase());
    let records = server.with_global_store_read(|store| {
        store
            .list_state(RECALL_CONFIG_PROPOSAL_NS)
            .map_err(|e| format!("list recall config proposals: {e}"))
    })?;
    let mut out = Vec::new();
    for row in records {
        let mut value: Value = serde_json::from_str(&row.value_json)
            .unwrap_or_else(|_| json!({ "proposal_id": row.key, "raw": row.value_json }));
        value["state_version"] = json!(row.version);
        value["updated_at"] = json!(row.updated_at);
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if desired.as_deref().is_some_and(|wanted| wanted != status) {
            continue;
        }
        out.push(value);
    }
    Ok(out)
}

fn parse_config_env_patch(proposal: &Value) -> Result<BTreeMap<String, String>, String> {
    let config_env = proposal
        .get("config_env")
        .and_then(Value::as_object)
        .ok_or_else(|| "recall config proposal missing config_env".to_string())?;
    let mut out = BTreeMap::new();
    for (key, value) in config_env {
        if !key.starts_with("TACHI_RECALL_") {
            return Err(format!(
                "recall config proposal contains non-recall config key: {key}"
            ));
        }
        let Some(value) = value.as_str() else {
            return Err(format!(
                "recall config proposal config_env.{key} must be a string"
            ));
        };
        out.insert(key.clone(), value.to_string());
    }
    Ok(out)
}

fn upsert_recall_config_env(path: &Path, values: &BTreeMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create config.env parent {}: {e}", parent.display()))?;
    }
    let existing = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("read config.env {}: {err}", path.display())),
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut lines = Vec::new();
    for line in existing.lines() {
        let trimmed = line.trim_start();
        let Some((raw_key, _raw_value)) = trimmed.split_once('=') else {
            lines.push(line.to_string());
            continue;
        };
        let key = raw_key.trim();
        if let Some(value) = values.get(key) {
            lines.push(format!("{key}={value}"));
            seen.insert(key.to_string());
        } else {
            lines.push(line.to_string());
        }
    }
    for (key, value) in values {
        if !seen.contains(key) {
            lines.push(format!("{key}={value}"));
        }
    }
    let mut body = lines.join("\n");
    body.push('\n');
    let tmp = tmp_path_for(path);
    std::fs::write(&tmp, body).map_err(|e| format!("write temp config.env: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("replace config.env {}: {e}", path.display())
    })
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(tmp)
}

fn required_proposal_id(params: &TachiMemoryParams) -> Result<&str, String> {
    params
        .proposal_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "proposal_id is required".to_string())
}

fn metric(variant: &Value, name: &str) -> f64 {
    variant["metrics"][name].as_f64().unwrap_or(0.0)
}

fn variant_improves(recall_delta: f64, mrr_delta: f64) -> bool {
    recall_delta > EPSILON || (recall_delta.abs() <= EPSILON && mrr_delta > EPSILON)
}

fn sanitize_key(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "variant".to_string()
    } else {
        out
    }
}

fn stable_hex_hash(input: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn format_recall_proposals_markdown(response: &Value) -> String {
    let mut out = vec![
        "Tachi recall proposals".to_string(),
        format!(
            "status: {}",
            response
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed")
        ),
        format!(
            "count: {}",
            response.get("count").and_then(Value::as_u64).unwrap_or(0)
        ),
    ];
    if let Some(proposals) = response.get("proposals").and_then(Value::as_array) {
        for proposal in proposals {
            let id = proposal
                .get("proposal_id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let status = proposal
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let variant = proposal
                .get("variant")
                .and_then(Value::as_str)
                .unwrap_or("-");
            out.push(format!("- `{id}` status={status} variant={variant}"));
        }
    }
    out.join("\n")
}
