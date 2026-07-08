//! Component governance read model (Issue #796).
//!
//! Persists v0 component records from the governance fixture as a governed
//! Tachi read model under `/components/v0/<component_id>` and exposes low-risk
//! `list`/`show` access via the `tachi_component` facade tool.
//!
//! Records live in the GLOBAL store (they are cross-project governance
//! artifacts, not per-project memories). Relations (`owns`, `consumes`,
//! `blocked_by`, `backflow_candidate`) are seeded as `MemoryEdge` rows.

use crate::MemoryServer;
use memory_core::{MemoryEdge, MemoryEntry};
use serde_json::{json, Value};

pub(crate) const COMPONENT_PATH_PREFIX: &str = "/components/v0/";
pub(crate) const COMPONENT_METADATA_KEY: &str = "component_record";
pub(crate) const GOVERNANCE_SCHEMA_VERSION: &str = "component_governance.v0";
pub(crate) const SEED_NS: &str = "component_governance_seed";
pub(crate) const SEED_KEY: &str = "v0";

/// Path a component record lives at.
pub(crate) fn component_path(component_id: &str) -> String {
    format!("{COMPONENT_PATH_PREFIX}{component_id}")
}

/// Parse the fixture JSON (embedded at compile time) into the records array.
fn fixture_records() -> Vec<Value> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../docs/engineering/architecture/component-governance-v0.fixture.json"
    ))
    .expect("component governance fixture parses");
    fixture
        .get("records")
        .and_then(Value::as_array)
        .expect("fixture has a records array")
        .clone()
}

/// Serialize a JSON value to a string (mirrors evidence_format::json_string).
fn to_json_string(value: &Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("serialize JSON response: {e}"))
}

/// Build a `MemoryEntry` for one component record.
fn entry_for_record(record: &Value) -> MemoryEntry {
    let component_id = record
        .get("component_id")
        .and_then(Value::as_str)
        .expect("component_id")
        .to_string();
    let component_type = record
        .get("component_type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let owner_repo = record
        .get("owner_repo")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let summary = record
        .get("contract_summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let last_verified_at = record
        .get("last_verified_at")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut metadata = serde_json::Map::new();
    metadata.insert(COMPONENT_METADATA_KEY.to_string(), record.clone());
    metadata.insert(
        "governance_schema_version".to_string(),
        json!(GOVERNANCE_SCHEMA_VERSION),
    );

    let mut keywords = vec![component_id.clone()];
    if !component_type.is_empty() {
        keywords.push(component_type.clone());
    }

    let mut entities: Vec<String> = Vec::new();
    if !owner_repo.is_empty() {
        entities.push(owner_repo);
    }

    MemoryEntry {
        id: deterministic_component_id(&component_id),
        path: component_path(&component_id),
        summary: summary.chars().take(100).collect(),
        text: record
            .get("contract_summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        importance: 0.8,
        timestamp: if last_verified_at.is_empty() {
            chrono::Utc::now().to_rfc3339()
        } else {
            last_verified_at.clone()
        },
        valid_from: if last_verified_at.is_empty() {
            chrono::Utc::now().to_rfc3339()
        } else {
            last_verified_at.clone()
        },
        valid_until: None,
        category: "entity".to_string(),
        topic: "component-governance".to_string(),
        keywords,
        persons: Vec::new(),
        entities,
        location: String::new(),
        source: "governance_fixture".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: Value::Object(metadata),
        vector: None,
        retention_policy: Some("permanent".to_string()),
        domain: Some("component-governance".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "pattern".to_string(),
    }
}

/// Deterministic, cross-toolchain-stable entry id for a component record so
/// re-seeds upsert rather than dup. Uses FNV-1a `stable_hash` (guaranteed
/// stable across Rust versions, unlike `DefaultHasher`).
fn deterministic_component_id(component_id: &str) -> String {
    format!(
        "c{}",
        crate::utils::stable_hash(&format!("component-v0-{component_id}"))
    )
}

/// Seed component records + relation edges into the global store, once.
/// Idempotent via a seed-once marker claimed LAST (only after all writes
/// succeed), so a mid-seed failure leaves no marker and the next boot retries.
/// Upserts are idempotent (keyed on entry id) so retries are safe.
/// Returns true if seeded this call.
pub(crate) fn seed_component_records(server: &MemoryServer) -> Result<bool, String> {
    // Check the marker first (read-only) so an already-seeded store short-circuits
    // without touching writes. The marker is only CLAIMED after success below.
    let already_seeded = server.with_global_store_read(|store| {
        store
            .get_state_kv(SEED_NS, SEED_KEY)
            .map(|v| v.is_some())
            .map_err(|e| format!("check component governance seed marker: {e}"))
    })?;
    if already_seeded {
        return Ok(false);
    }

    let records = fixture_records();
    let known_ids: Vec<String> = records
        .iter()
        .filter_map(|r| {
            r.get("component_id")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .collect();

    server.with_global_store(|store| {
        // Upsert each record (idempotent — keyed on entry id).
        for record in &records {
            let entry = entry_for_record(record);
            store
                .upsert(&entry)
                .map_err(|e| format!("upsert component record: {e}"))?;
        }
        // Seed relation edges between known component ids. Edges are upserts
        // (ON CONFLICT source,target,relation), so retries don't duplicate.
        for record in &records {
            let component_id = record
                .get("component_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if component_id.is_empty() {
                continue;
            }
            let entry_id = deterministic_component_id(component_id);
            let is_kernel = record
                .get("component_type")
                .and_then(Value::as_str)
                .map(|t| t == "kernel")
                .unwrap_or(false);
            for consumer in record
                .get("downstream_consumers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if let Some(target_id) = known_ids.iter().find(|c| consumer.contains(c.as_str())) {
                    let relation = if is_kernel { "owns" } else { "consumes" };
                    store.add_edge(&MemoryEdge {
                        source_id: entry_id.clone(),
                        target_id: deterministic_component_id(target_id),
                        relation: relation.to_string(),
                        weight: 1.0,
                        metadata: Value::Null,
                        created_at: chrono::Utc::now().to_rfc3339(),
                        valid_from: chrono::Utc::now().to_rfc3339(),
                        valid_to: None,
                    }).map_err(|e| format!("seed {relation} edge: {e}"))?;
                }
            }
            for drift in record
                .get("known_drift")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let classification = drift
                    .get("classification")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let relation = match classification {
                    "backflow_candidate" => Some("backflow_candidate"),
                    "blocked_fork" => Some("blocked_by"),
                    _ => None,
                };
                if let Some(rel) = relation {
                    // self-edge documenting the drift classification on this record
                    store.add_edge(&MemoryEdge {
                        source_id: entry_id.clone(),
                        target_id: entry_id.clone(),
                        relation: rel.to_string(),
                        weight: 0.5,
                        metadata: json!({"drift": drift.clone()}),
                        created_at: chrono::Utc::now().to_rfc3339(),
                        valid_from: chrono::Utc::now().to_rfc3339(),
                        valid_to: None,
                    }).map_err(|e| format!("seed {rel} drift edge: {e}"))?;
                }
            }
        }
        Ok(())
    })?;

    // Claim the marker LAST, only after all writes succeeded. A mid-seed
    // failure leaves no marker, so the next boot retries the idempotent upserts.
    server.with_global_store(|store| {
        store
            .insert_state_if_absent(SEED_NS, SEED_KEY, "{\"seeded\":true}")
            .map_err(|e| format!("claim component governance seed marker: {e}"))
    })?;

    Ok(true)
}

/// Handle the `tachi_component` facade action.
pub(crate) async fn handle_tachi_component(
    server: &MemoryServer,
    params: crate::tool_params::TachiComponentParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    match action.as_str() {
        "list" => handle_list(server, &params).await,
        "show" => handle_show(server, &params).await,
        "check" => handle_check(server, &params).await,
        other => Err(format!(
            "unknown tachi_component action '{other}'; expected 'list', 'show', or 'check'"
        )),
    }
}

async fn handle_list(
    server: &MemoryServer,
    params: &crate::tool_params::TachiComponentParams,
) -> Result<String, String> {
    let limit = params.limit.unwrap_or(100).min(500);
    let include_archived = params.include_archived.unwrap_or(false);
    let entries = server.with_global_store_read(|store| {
        store
            .list_by_path(COMPONENT_PATH_PREFIX, limit, include_archived)
            .map_err(|e| format!("list component records: {e}"))
    })?;

    let mut compact: Vec<Value> = Vec::new();
    for entry in &entries {
        if let Some(record) = extract_component_record(&entry.metadata) {
            // Optional component_type filter.
            if let Some(want_type) = params.component_type.as_deref() {
                let actual = record
                    .get("component_type")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if actual != want_type {
                    continue;
                }
            }
            let archived = entry.valid_until.is_some();
            compact.push(json!({
                "component_id": record.get("component_id"),
                "component_type": record.get("component_type"),
                "owner_repo": record.get("owner_repo"),
                "summary": entry.summary,
                "archived": archived,
            }));
        }
    }

    if crate::facade_memory_ops::wants_json(params.format.as_deref()) {
        return to_json_string(&json!({
            "status": "completed",
            "count": compact.len(),
            "records": compact,
        }));
    }

    // Markdown rendering.
    let mut lines = Vec::new();
    lines.push(format!("# Component records ({})\n", compact.len()));
    for (idx, rec) in compact.iter().enumerate() {
        let id = rec
            .get("component_id")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let ctype = rec
            .get("component_type")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let owner = rec.get("owner_repo").and_then(Value::as_str).unwrap_or("?");
        let summary = rec.get("summary").and_then(Value::as_str).unwrap_or("");
        let flag = if rec
            .get("archived")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            " [archived]"
        } else {
            ""
        };
        lines.push(format!(
            "{}. `{}` ({}) — {}{flag}\n   {}",
            idx + 1,
            id,
            ctype,
            owner,
            summary
        ));
    }
    Ok(lines.join("\n"))
}

async fn handle_show(
    server: &MemoryServer,
    params: &crate::tool_params::TachiComponentParams,
) -> Result<String, String> {
    let component_id = params
        .component_id
        .as_deref()
        .ok_or_else(|| "component_id is required when action='show'".to_string())?;
    let path = component_path(component_id);

    let entry_opt = server.with_global_store_read(|store| {
        let matches = store
            .list_by_path(&path, 1, true)
            .map_err(|e| format!("show component record: {e}"))?;
        Ok::<_, String>(matches.into_iter().next())
    })?;

    let Some(entry) = entry_opt else {
        return to_json_string(&json!({
            "status": "not_found",
            "component_id": component_id,
        }));
    };

    let record = extract_component_record(&entry.metadata).unwrap_or(Value::Null);
    let archived = entry.valid_until.is_some();

    // Fetch relation edges for this record.
    let edges = server.with_global_store_read(|store| {
        store
            .get_edges(&entry.id, "both", None)
            .map_err(|e| format!("get component edges: {e}"))
    })?;
    let edge_list: Vec<Value> = edges
        .into_iter()
        .map(|e| {
            json!({
                "relation": e.relation,
                "source_id": e.source_id,
                "target_id": e.target_id,
                "metadata": e.metadata,
            })
        })
        .collect();

    if crate::facade_memory_ops::wants_json(params.format.as_deref()) {
        return to_json_string(&json!({
            "status": if archived { "archived" } else { "completed" },
            "component_id": component_id,
            "record": record,
            "edges": edge_list,
        }));
    }

    // Markdown rendering.
    let mut lines = Vec::new();
    let flag = if archived { " [archived]" } else { "" };
    lines.push(format!(
        "# {}{flag}\n",
        record
            .get("component_id")
            .and_then(Value::as_str)
            .unwrap_or(component_id)
    ));
    lines.push(format!(
        "**Type:** {}\n",
        record
            .get("component_type")
            .and_then(Value::as_str)
            .unwrap_or("?")
    ));
    lines.push(format!(
        "**Owner:** {}\n",
        record
            .get("owner_repo")
            .and_then(Value::as_str)
            .unwrap_or("?")
    ));
    if let Some(summary) = record.get("contract_summary").and_then(Value::as_str) {
        lines.push(format!("**Contract:** {summary}\n"));
    }
    if let Some(prereqs) = record.get("upstream_prereqs").and_then(Value::as_array) {
        if !prereqs.is_empty() {
            lines.push("**Upstream prereqs:**".to_string());
            for p in prereqs {
                if let Some(s) = p.as_str() {
                    lines.push(format!("- {s}"));
                }
            }
            lines.push(String::new());
        }
    }
    if let Some(drift) = record.get("known_drift").and_then(Value::as_array) {
        if !drift.is_empty() {
            lines.push("**Known drift:**".to_string());
            for d in drift {
                let area = d.get("area").and_then(Value::as_str).unwrap_or("?");
                let class = d
                    .get("classification")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                lines.push(format!("- `{area}` — {class}"));
            }
            lines.push(String::new());
        }
    }
    if !edge_list.is_empty() {
        lines.push(format!("**Relation edges ({}):**", edge_list.len()));
        for e in &edge_list {
            let rel = e.get("relation").and_then(Value::as_str).unwrap_or("?");
            lines.push(format!("- {rel}"));
        }
    }
    Ok(lines.join("\n"))
}

/// Extract the stored component_record object from a MemoryEntry's metadata.
fn extract_component_record(metadata: &Value) -> Option<Value> {
    metadata
        .get(COMPONENT_METADATA_KEY)
        .filter(|v| v.is_object())
        .cloned()
}

// ─── Issue #797: read-only downstream classifier ─────────────────────────────

/// Classification categories returned by `component check` (Issue #797).
pub(crate) const CATEGORY_KERNEL_DRIFT: &str = "kernel_drift";
pub(crate) const CATEGORY_ALLOWED_ADAPTER_POLICY: &str = "allowed_adapter_policy";
pub(crate) const CATEGORY_BRIDGE: &str = "bridge";
pub(crate) const CATEGORY_FRONTEND_SHELL: &str = "frontend_shell";
pub(crate) const CATEGORY_UNKNOWN: &str = "unknown";

/// Run `git -C <cwd> <args>` and return trimmed stdout (read-only commands only).
fn run_git_readonly(cwd: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| format!("run git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "git {} failed{}",
            args.join(" "),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Normalize a git remote URL to `owner/repo` form (best-effort).
/// Accepts `https://host/owner/repo(.git)`, `git@host:owner/repo(.git)`.
pub(crate) fn normalize_remote_to_owner_repo(url: &str) -> String {
    let url = url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git");
    // ssh form git@host:owner/repo — only if there's no "://" scheme.
    if !url.contains("://") {
        if let Some(colon_idx) = url.find(':') {
            let after = &url[colon_idx + 1..];
            if after.contains('/') {
                return after.to_string();
            }
        }
    }
    // https://host/owner/repo
    if let Some(scheme_idx) = url.find("://") {
        let after_scheme = &url[scheme_idx + 3..];
        if let Some(slash) = after_scheme.find('/') {
            return after_scheme[slash + 1..].to_string();
        }
    }
    url.to_string()
}

/// Read-only classification of a checked-out repo path against declared records.
/// Returns `(category, matched_component_id, evidence_gaps)`.
fn classify_repo(
    records: &[Value],
    repo_path: &std::path::Path,
    scope_component_id: Option<&str>,
) -> (String, Option<String>, Vec<String>) {
    let mut gaps: Vec<String> = Vec::new();

    if !repo_path.exists() {
        gaps.push(format!(
            "repo path does not exist: {}",
            repo_path.display()
        ));
        return (CATEGORY_UNKNOWN.to_string(), None, gaps);
    }

    // Detect git remote (origin). Missing remote is evidence-weak but not fatal.
    let remote = run_git_readonly(repo_path, &["remote", "get-url", "origin"]).ok();
    let remote_normalized = remote.as_deref().map(normalize_remote_to_owner_repo);
    if remote_normalized.is_none() {
        gaps.push("no git origin remote detected (cannot match owner_repo)".to_string());
    }

    let candidates: Vec<&Value> = records
        .iter()
        .filter(|r| {
            scope_component_id
                .map(|sc| {
                    r.get("component_id")
                        .and_then(Value::as_str)
                        .map(|id| id == sc)
                        .unwrap_or(false)
                })
                .unwrap_or(true)
        })
        .collect();

    // Score each candidate: remote-match (strong) ranks above path-only-match (weak).
    // This tie-break prevents a path-sorted record (e.g. tachi-event-projection-bridge,
    // which shares owner_repo kckylechen1/tachi with the kernel) from shadowing the
    // correct record when both could match.
    #[derive(Clone, Copy)]
    enum MatchStrength {
        None,
        PathOnly,
        Remote,
    }
    let mut best: Option<(&Value, MatchStrength)> = None;
    for record in &candidates {
        let owner_repo = record
            .get("owner_repo")
            .and_then(Value::as_str)
            .unwrap_or("");
        let remote_matches = remote_normalized
            .as_deref()
            .map(|rn| rn.eq_ignore_ascii_case(owner_repo))
            .unwrap_or(false);

        // Evidence: do any owner_path subpaths exist under repo_path?
        // owner_path is `;`-separated; skip prose phrases (contain spaces).
        let owner_paths: Vec<&str> = record
            .get("owner_path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.contains(' '))
            .collect();
        let path_matches: Vec<&str> = owner_paths
            .iter()
            .filter(|p| repo_path.join(p).exists())
            .copied()
            .collect();

        let strength = if remote_matches {
            MatchStrength::Remote
        } else if !path_matches.is_empty() {
            MatchStrength::PathOnly
        } else {
            MatchStrength::None
        };

        if matches!(strength, MatchStrength::None) {
            continue;
        }
        // Prefer the strongest match; on ties keep the first (stable).
        // best only ever holds PathOnly or Remote (None is skipped above).
        let stronger = match (&best, strength) {
            (None, _) => true,
            (Some((_, MatchStrength::PathOnly)), MatchStrength::Remote) => true,
            (Some((_, MatchStrength::Remote)), _) => false,
            _ => false,
        };
        if stronger {
            best = Some((record, strength));
        }
    }

    if let Some((record, strength)) = best {
        let component_id = record
            .get("component_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let component_type = record
            .get("component_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let category = match component_type {
            "kernel" => CATEGORY_KERNEL_DRIFT,
            "runtime_adapter" => CATEGORY_ALLOWED_ADAPTER_POLICY,
            "workflow_bridge" => CATEGORY_BRIDGE,
            "frontend_app_shell" => CATEGORY_FRONTEND_SHELL,
            _ => CATEGORY_UNKNOWN,
        };
        // Path-only matches (remote differs or absent) are weaker evidence:
        // surface a gap so the caller knows this is not a confident canonical match
        // (e.g. a fork whose owner_path dirs happen to exist).
        let mut result_gaps: Vec<String> = Vec::new();
        if matches!(strength, MatchStrength::PathOnly) {
            result_gaps.push(
                "matched by owner_path only; git origin remote differs from the declared owner_repo (possible fork / drift)".to_string(),
            );
        }
        return (
            category.to_string(),
            Some(component_id.to_string()),
            result_gaps,
        );
    }

    if let Some(rn) = remote_normalized.as_deref() {
        if scope_component_id.is_some() {
            gaps.push(format!(
                "git origin remote ({rn}) did not match the scoped component_id's owner_repo"
            ));
        } else {
            gaps.push(format!("git origin remote ({rn}) matched no declared owner_repo"));
        }
    }
    gaps.push("no declared component record matched this repo's remote or owner_path".to_string());
    (CATEGORY_UNKNOWN.to_string(), None, gaps)
}

/// Handle `tachi_component(action="check")` — read-only downstream classifier.
async fn handle_check(
    server: &MemoryServer,
    params: &crate::tool_params::TachiComponentParams,
) -> Result<String, String> {
    let repo = params
        .repo
        .as_deref()
        .ok_or_else(|| "repo is required when action='check'".to_string())?;
    let repo_path = std::path::Path::new(repo);

    // Load declared records from the global store (read-only).
    let records: Vec<Value> = server.with_global_store_read(|store| {
        let entries = store
            .list_by_path(COMPONENT_PATH_PREFIX, 500, false)
            .map_err(|e| format!("list component records for check: {e}"))?;
        Ok::<_, String>(
            entries
                .iter()
                .filter_map(|e| extract_component_record(&e.metadata))
                .collect(),
        )
    })?;

    let (category, matched_id, gaps) =
        classify_repo(&records, repo_path, params.component_id.as_deref());

    if crate::facade_memory_ops::wants_json(params.format.as_deref()) {
        return to_json_string(&json!({
            "status": "completed",
            "category": category,
            "matched_component_id": matched_id,
            "repo": repo,
            "evidence_gaps": gaps,
        }));
    }

    let matched = matched_id.as_deref().unwrap_or("(none)");
    let mut out = format!("# Component classification\n\n");
    out.push_str(&format!("**Repo:** `{repo}`\n"));
    out.push_str(&format!("**Category:** `{category}`\n"));
    out.push_str(&format!("**Matched component:** {matched}\n"));
    if !gaps.is_empty() {
        out.push_str("\n**Evidence gaps:**\n");
        for g in &gaps {
            out.push_str(&format!("- {g}\n"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
