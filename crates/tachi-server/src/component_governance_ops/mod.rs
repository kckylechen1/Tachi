//! Component governance read model (Issues #796–#799).
//!
//! Persists v0 component records from the governance fixture as a governed
//! Tachi read model under `/components/v0/<component_id>` and exposes low-risk
//! `list`/`show`/`check`/`plan` access via the `tachi_component` facade tool.
//! Briefing/status surfaces matching records for the active workspace (#799).
//!
//! Records live in the GLOBAL store (they are cross-project governance
//! artifacts, not per-project memories). Relations (`owns`, `consumes`,
//! `blocked_by`, `backflow_candidate`) are seeded as `MemoryEdge` rows.

use crate::MemoryServer;
use memcore::{ComponentGovernanceRelation, MemoryEdge, MemoryEntry};
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
                    let relation = if is_kernel {
                        ComponentGovernanceRelation::Owns
                    } else {
                        ComponentGovernanceRelation::Consumes
                    };
                    store
                        .add_component_governance_edge(
                            &MemoryEdge {
                                source_id: entry_id.clone(),
                                target_id: deterministic_component_id(target_id),
                                relation: relation.as_str().to_string(),
                                weight: 1.0,
                                metadata: Value::Null,
                                created_at: chrono::Utc::now().to_rfc3339(),
                                valid_from: chrono::Utc::now().to_rfc3339(),
                                valid_to: None,
                            },
                            relation,
                        )
                        .map_err(|e| format!("seed {} edge: {e}", relation.as_str()))?;
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
                    "backflow_candidate" => Some(ComponentGovernanceRelation::BackflowCandidate),
                    "blocked_fork" => Some(ComponentGovernanceRelation::BlockedBy),
                    _ => None,
                };
                if let Some(rel) = relation {
                    // self-edge documenting the drift classification on this record
                    store
                        .add_component_governance_edge(
                            &MemoryEdge {
                                source_id: entry_id.clone(),
                                target_id: entry_id.clone(),
                                relation: rel.as_str().to_string(),
                                weight: 0.5,
                                metadata: json!({"drift": drift.clone()}),
                                created_at: chrono::Utc::now().to_rfc3339(),
                                valid_from: chrono::Utc::now().to_rfc3339(),
                                valid_to: None,
                            },
                            rel,
                        )
                        .map_err(|e| format!("seed {} drift edge: {e}", rel.as_str()))?;
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
        "plan" => handle_plan(server, &params).await,
        other => Err(format!(
            "unknown tachi_component action '{other}'; expected 'list', 'show', 'check', or 'plan'"
        )),
    }
}

/// Load declared component records from the global store (read-only).
fn load_component_records(server: &MemoryServer) -> Result<Vec<Value>, String> {
    server.with_global_store_read(|store| {
        let entries = store
            .list_by_path(COMPONENT_PATH_PREFIX, 500, false)
            .map_err(|e| format!("list component records: {e}"))?;
        Ok(entries
            .iter()
            .filter_map(|e| extract_component_record(&e.metadata))
            .collect())
    })
}

/// Find a record by exact component_id.
fn find_record_by_id<'a>(records: &'a [Value], component_id: &str) -> Option<&'a Value> {
    records.iter().find(|r| {
        r.get("component_id")
            .and_then(Value::as_str)
            .map(|id| id == component_id)
            .unwrap_or(false)
    })
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
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
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
        gaps.push(format!("repo path does not exist: {}", repo_path.display()));
        return (CATEGORY_UNKNOWN.to_string(), None, gaps);
    }

    // Callers may pass a package directory instead of the checkout root. Resolve
    // it before evaluating the fixture's repo-relative owner paths, otherwise a
    // valid Tachi checkout such as `crates/` loses its canonical path evidence.
    let checkout_root = run_git_readonly(repo_path, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .unwrap_or_else(|| repo_path.to_path_buf());

    // Detect git remote (origin). Missing remote is evidence-weak but not fatal.
    let remote = run_git_readonly(&checkout_root, &["remote", "get-url", "origin"]).ok();
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
    // Within either rank, prefer the record with more matching owner paths. This
    // prevents a documentation-only reference on a downstream record from
    // shadowing the canonical kernel surface in a source checkout.
    #[derive(Clone, Copy)]
    enum MatchStrength {
        None,
        PathOnly(usize),
        Remote(usize),
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
            .filter(|p| checkout_root.join(p).exists())
            .copied()
            .collect();

        let strength = if remote_matches {
            MatchStrength::Remote(path_matches.len())
        } else if !path_matches.is_empty() {
            MatchStrength::PathOnly(path_matches.len())
        } else {
            MatchStrength::None
        };

        if matches!(strength, MatchStrength::None) {
            continue;
        }
        // Prefer the strongest match, then the most specific path evidence.
        // On an exact tie keep the first result (stable). `best` only ever
        // holds PathOnly or Remote because None is skipped above.
        let stronger = match (&best, strength) {
            (None, _) => true,
            (Some((_, MatchStrength::PathOnly(_))), MatchStrength::Remote(_)) => true,
            (Some((_, MatchStrength::Remote(_))), MatchStrength::PathOnly(_)) => false,
            (
                Some((_, MatchStrength::Remote(best_paths))),
                MatchStrength::Remote(candidate_paths),
            ) => candidate_paths > *best_paths,
            (
                Some((_, MatchStrength::PathOnly(best_paths))),
                MatchStrength::PathOnly(candidate_paths),
            ) => candidate_paths > *best_paths,
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
        if matches!(strength, MatchStrength::PathOnly(_)) {
            result_gaps.push(if remote_normalized.is_some() {
                "matched by owner_path only; git origin remote differs from the declared owner_repo (possible fork / drift)".to_string()
            } else {
                "matched by owner_path only; git origin remote is unavailable (possible fork / drift)".to_string()
            });
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
            gaps.push(format!(
                "git origin remote ({rn}) matched no declared owner_repo"
            ));
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
    let records = load_component_records(server)?;

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

// ─── Issue #798: read-only cutover planner ───────────────────────────────────

pub(crate) const OUTCOME_PULL: &str = "pull";
pub(crate) const OUTCOME_ADAPT: &str = "adapt";
pub(crate) const OUTCOME_BACKFLOW: &str = "backflow";
pub(crate) const OUTCOME_DELETE_RETIRE: &str = "delete_retire";

/// Days after which a governance record's last_verified_at is labeled stale (#799).
pub(crate) const GOVERNANCE_STALE_AFTER_DAYS: i64 = 14;

fn string_array_field(record: &Value, key: &str) -> Vec<String> {
    record
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn plan_item(outcome: &str, action: &str, detail: &str, source: &str) -> Value {
    json!({
        "outcome": outcome,
        "action": action,
        "detail": detail,
        "source": source,
    })
}

/// Resolve the plan `--to` target: filesystem path, component_id, or owner_repo.
fn resolve_plan_target(
    records: &[Value],
    to: &str,
) -> (Option<Value>, String, Option<String>, Vec<String>) {
    let path = std::path::Path::new(to);
    if path.exists() {
        let (category, matched_id, gaps) = classify_repo(records, path, None);
        let target = matched_id
            .as_deref()
            .and_then(|id| find_record_by_id(records, id))
            .cloned();
        return (target, category, matched_id, gaps);
    }

    // Exact component_id match.
    if let Some(record) = find_record_by_id(records, to) {
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
        return (
            Some(record.clone()),
            category.to_string(),
            Some(to.to_string()),
            Vec::new(),
        );
    }

    // owner_repo match (case-insensitive). Prefer non-bridge when ambiguous.
    let owner_matches: Vec<&Value> = records
        .iter()
        .filter(|r| {
            r.get("owner_repo")
                .and_then(Value::as_str)
                .map(|owner| owner.eq_ignore_ascii_case(to) || owner.ends_with(&format!("/{to}")))
                .unwrap_or(false)
        })
        .collect();
    if !owner_matches.is_empty() {
        let mut gaps = Vec::new();
        if owner_matches.len() > 1 {
            gaps.push(format!(
                "{} records share owner_repo matching '{}'; picked first by type priority",
                owner_matches.len(),
                to
            ));
        }
        let preferred = owner_matches
            .iter()
            .find(|r| r.get("component_type").and_then(Value::as_str) != Some("workflow_bridge"))
            .or(owner_matches.first())
            .copied();
        if let Some(record) = preferred {
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
            let id = record
                .get("component_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            return (Some(record.clone()), category.to_string(), id, gaps);
        }
    }

    (
        None,
        CATEGORY_UNKNOWN.to_string(),
        None,
        vec![format!(
            "plan --to '{to}' matched no checkout path, component_id, or owner_repo"
        )],
    )
}

/// RomanBath defaults to frontend shell unless drift proves product-owned memory policy.
fn romanbath_shell_note(target: Option<&Value>, category: &str) -> Option<String> {
    let record = target?;
    let id = record
        .get("component_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if id != "romanbath-frontend-app-shell" && category != CATEGORY_FRONTEND_SHELL {
        return None;
    }
    let has_product_memory_policy = record
        .get("known_drift")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|d| {
            let class = d
                .get("classification")
                .and_then(Value::as_str)
                .unwrap_or("");
            let area = d.get("area").and_then(Value::as_str).unwrap_or("");
            let summary = d.get("summary").and_then(Value::as_str).unwrap_or("");
            class == "accepted_local_policy"
                && (area.contains("memory")
                    || summary.to_ascii_lowercase().contains("memory policy")
                    || summary
                        .to_ascii_lowercase()
                        .contains("product-owned memory"))
        });
    if has_product_memory_policy {
        Some(
            "RomanBath shows accepted_local_policy memory-adjacent drift; treat as product-owned memory policy only for those declared areas — shell remains non-kernel."
                .to_string(),
        )
    } else {
        Some(
            "RomanBath treated as frontend/app shell unless evidence proves product-owned memory policy; UI/persona stay local."
                .to_string(),
        )
    }
}

/// Build ordered cutover checklist items from source + optional target records.
fn build_cutover_items(source: &Value, target: Option<&Value>) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();

    for prereq in string_array_field(source, "upstream_prereqs") {
        items.push(plan_item(
            OUTCOME_PULL,
            "satisfy_upstream_prereq",
            &prereq,
            "upstream_prereqs",
        ));
    }

    // Target-side gates that must hold before pull (consumer view of the source).
    if let Some(t) = target {
        for prereq in string_array_field(t, "upstream_prereqs") {
            items.push(plan_item(
                OUTCOME_PULL,
                "satisfy_target_gate",
                &prereq,
                "target.upstream_prereqs",
            ));
        }
        // Hypermem-specific gates: aliases / direct-reader / trading policy.
        let target_id = t.get("component_id").and_then(Value::as_str).unwrap_or("");
        if target_id == "hypermemory-trading-adapter"
            || t.get("owner_repo")
                .and_then(Value::as_str)
                .is_some_and(|r| {
                    r.contains("Quant_Analyzer") || r.to_ascii_lowercase().contains("hyper")
                })
        {
            items.push(plan_item(
                OUTCOME_PULL,
                "gate_aliases",
                "Compatibility aliases (hypermemory_*/legacy facade names) must be adapter shims only — not permanent upstream API",
                "hypermem.aliases",
            ));
            items.push(plan_item(
                OUTCOME_PULL,
                "gate_direct_reader",
                "Direct table readers must be shimmed and retired rather than preserved as a fork",
                "hypermem.direct_reader",
            ));
            items.push(plan_item(
                OUTCOME_ADAPT,
                "gate_trading_policy",
                "A-share freshness and session decay remain downstream trading policy through reviewed hooks",
                "hypermem.trading_policy",
            ));
        }
        if target_id == "zeroclaw-chat-memory-adapter"
            || t.get("owner_repo")
                .and_then(Value::as_str)
                .is_some_and(|r| r.to_ascii_lowercase().contains("zeroclaw"))
        {
            items.push(plan_item(
                OUTCOME_PULL,
                "gate_chat_agent_adapter",
                "Generic chat-agent adapter contract must stay product-agnostic (no RomanBath fields)",
                "zeroclaw.chat_agent_adapter",
            ));
            items.push(plan_item(
                OUTCOME_PULL,
                "gate_event_projection",
                "Event projection bridge must record recall/reflection lifecycle without product sync",
                "zeroclaw.event_projection",
            ));
        }
    }

    for variation in string_array_field(source, "allowed_variation") {
        items.push(plan_item(
            OUTCOME_ADAPT,
            "keep_allowed_variation",
            &variation,
            "allowed_variation",
        ));
    }
    for forbidden in string_array_field(source, "forbidden_variation") {
        items.push(plan_item(
            OUTCOME_DELETE_RETIRE,
            "reject_forbidden_variation",
            &forbidden,
            "forbidden_variation",
        ));
    }

    let drift_sources: Vec<(&str, Option<&Value>)> = vec![
        ("source.known_drift", Some(source)),
        ("target.known_drift", target),
    ];
    for (label, rec) in drift_sources {
        let Some(record) = rec else { continue };
        for drift in record
            .get("known_drift")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let area = drift.get("area").and_then(Value::as_str).unwrap_or("?");
            let class = drift
                .get("classification")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let summary = drift.get("summary").and_then(Value::as_str).unwrap_or("");
            let detail = if summary.is_empty() {
                format!("{area} ({class})")
            } else {
                format!("{area}: {summary}")
            };
            match class {
                "accepted_local_policy" => items.push(plan_item(
                    OUTCOME_ADAPT,
                    "retain_accepted_local_policy",
                    &detail,
                    label,
                )),
                "backflow_candidate" => items.push(plan_item(
                    OUTCOME_BACKFLOW,
                    "propose_backflow",
                    &detail,
                    label,
                )),
                "retire_delete" => items.push(plan_item(
                    OUTCOME_DELETE_RETIRE,
                    "delete_or_retire",
                    &detail,
                    label,
                )),
                "blocked_fork" => items.push(plan_item(
                    OUTCOME_DELETE_RETIRE,
                    "block_or_retire_fork",
                    &detail,
                    label,
                )),
                "unknown" => items.push(plan_item(
                    OUTCOME_ADAPT,
                    "inspect_unknown_drift",
                    &detail,
                    label,
                )),
                _ => {}
            }
        }
    }

    for candidate in string_array_field(source, "backflow_candidates") {
        items.push(plan_item(
            OUTCOME_BACKFLOW,
            "evaluate_backflow_candidate",
            &candidate,
            "backflow_candidates",
        ));
    }
    if let Some(t) = target {
        for candidate in string_array_field(t, "backflow_candidates") {
            items.push(plan_item(
                OUTCOME_BACKFLOW,
                "evaluate_target_backflow_candidate",
                &candidate,
                "target.backflow_candidates",
            ));
        }
    }

    items
}

fn group_plan_outcomes(items: &[Value]) -> Vec<Value> {
    let order = [
        OUTCOME_PULL,
        OUTCOME_ADAPT,
        OUTCOME_BACKFLOW,
        OUTCOME_DELETE_RETIRE,
    ];
    order
        .iter()
        .filter_map(|outcome| {
            let group: Vec<&Value> = items
                .iter()
                .filter(|i| i.get("outcome").and_then(Value::as_str) == Some(*outcome))
                .collect();
            if group.is_empty() {
                None
            } else {
                Some(json!({
                    "outcome": outcome,
                    "count": group.len(),
                    "items": group,
                }))
            }
        })
        .collect()
}

/// Handle `tachi_component(action="plan")` — read-only cutover checklist (#798).
async fn handle_plan(
    server: &MemoryServer,
    params: &crate::tool_params::TachiComponentParams,
) -> Result<String, String> {
    let from = params
        .component_id
        .as_deref()
        .ok_or_else(|| "component_id is required when action='plan' (--from)".to_string())?;
    let to = params.repo.as_deref().ok_or_else(|| {
        "repo is required when action='plan' (--to path, component_id, or owner_repo)".to_string()
    })?;

    let records = load_component_records(server)?;
    let Some(source) = find_record_by_id(&records, from) else {
        return to_json_string(&json!({
            "status": "not_found",
            "component_id": from,
            "message": format!("unknown source component_id '{from}'"),
        }));
    };

    let (target, category, matched_id, mut gaps) = resolve_plan_target(&records, to);
    let items = build_cutover_items(source, target.as_ref());
    let outcomes = group_plan_outcomes(&items);
    let shell_note = romanbath_shell_note(target.as_ref(), &category);
    if let Some(note) = &shell_note {
        // Surface as an evidence note, not a hard gap.
        gaps.push(format!("policy_note: {note}"));
    }

    let freshness = record_freshness(source);
    let target_freshness = target.as_ref().map(record_freshness);

    let body = json!({
        "status": "completed",
        "from": {
            "component_id": from,
            "component_type": source.get("component_type"),
            "owner_repo": source.get("owner_repo"),
            "freshness": freshness,
        },
        "to": {
            "input": to,
            "matched_component_id": matched_id,
            "category": category,
            "component_type": target.as_ref().and_then(|t| t.get("component_type").cloned()),
            "owner_repo": target.as_ref().and_then(|t| t.get("owner_repo").cloned()),
            "freshness": target_freshness,
        },
        "outcomes": outcomes,
        "items": items,
        "evidence_gaps": gaps,
        "romanbath_note": shell_note,
        "non_goals": [
            "no automatic code changes",
            "no automatic repo sync",
            "no package-manager behavior",
        ],
    });

    if crate::facade_memory_ops::wants_json(params.format.as_deref()) {
        return to_json_string(&body);
    }

    let mut out = format!("# Component cutover plan\n\n");
    out.push_str(&format!("**From:** `{from}`\n"));
    out.push_str(&format!("**To:** `{to}`"));
    if let Some(id) = matched_id.as_deref() {
        out.push_str(&format!(" → matched `{id}` ({category})"));
    } else {
        out.push_str(&format!(" → category `{category}`"));
    }
    out.push_str("\n\n");
    if let Some(note) = shell_note {
        out.push_str(&format!("_{note}_\n\n"));
    }
    for group in &outcomes {
        let outcome = group.get("outcome").and_then(Value::as_str).unwrap_or("?");
        out.push_str(&format!("## Outcome: `{outcome}`\n"));
        if let Some(group_items) = group.get("items").and_then(Value::as_array) {
            for item in group_items {
                let action = item.get("action").and_then(Value::as_str).unwrap_or("?");
                let detail = item.get("detail").and_then(Value::as_str).unwrap_or("");
                out.push_str(&format!("- **{action}:** {detail}\n"));
            }
        }
        out.push('\n');
    }
    if !gaps.is_empty() {
        out.push_str("## Evidence gaps / notes\n");
        for g in &gaps {
            out.push_str(&format!("- {g}\n"));
        }
    }
    out.push_str("\n_Read-only plan: no code changes, sync, or package operations._\n");
    Ok(out)
}

// ─── Issue #799: briefing/status integration ─────────────────────────────────

/// Freshness label for a governance record (registry evidence, not memory truth).
fn record_freshness(record: &Value) -> Value {
    let last = record
        .get("last_verified_at")
        .and_then(Value::as_str)
        .unwrap_or("");
    if last.is_empty() {
        return json!({
            "state": "unknown",
            "last_verified_at": null,
            "label": "unverified — do not treat as current truth",
        });
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(last)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|_| {
            // Accept date-only timestamps from fixtures.
            chrono::NaiveDate::parse_from_str(last, "%Y-%m-%d").map(|d| {
                d.and_hms_opt(0, 0, 0)
                    .map(|ndt| ndt.and_utc())
                    .unwrap_or_else(chrono::Utc::now)
            })
        });
    match parsed {
        Ok(when) => {
            let age_days = (chrono::Utc::now() - when).num_days();
            if age_days > GOVERNANCE_STALE_AFTER_DAYS {
                json!({
                    "state": "stale",
                    "last_verified_at": last,
                    "age_days": age_days,
                    "label": format!("stale ({age_days}d > {GOVERNANCE_STALE_AFTER_DAYS}d) — re-verify before treating as current"),
                })
            } else {
                json!({
                    "state": "current",
                    "last_verified_at": last,
                    "age_days": age_days,
                    "label": "registry-verified (governance fixture; not memory-derived)",
                })
            }
        }
        Err(_) => json!({
            "state": "unknown",
            "last_verified_at": last,
            "label": "unparseable last_verified_at — treat as unknown",
        }),
    }
}

fn compact_drift_summary(record: &Value) -> Vec<Value> {
    record
        .get("known_drift")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let class = d.get("classification").and_then(Value::as_str)?;
            if class == "none" {
                return None;
            }
            Some(json!({
                "area": d.get("area"),
                "classification": class,
                "summary": d.get("summary"),
            }))
        })
        .collect()
}

/// Resolve the active workspace git root for governance matching (read-only).
fn resolve_workspace_git_root() -> Option<std::path::PathBuf> {
    for var in ["TACHI_PROJECT_ROOT", "TACHI_WORKSPACE_ROOT"] {
        if let Ok(value) = std::env::var(var) {
            if value.is_empty() {
                continue;
            }
            let candidate = std::path::PathBuf::from(value);
            if candidate.join(".git").exists() {
                return Some(candidate);
            }
            // Walk up from the env path in case it points mid-tree.
            let mut dir = candidate;
            loop {
                if dir.join(".git").exists() {
                    return Some(dir);
                }
                if !dir.pop() {
                    break;
                }
            }
        }
    }
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Match component records relevant to the current workspace / project context.
///
/// Returns a JSON array of matching component summaries with freshness labels.
/// Used by briefing (#799) and status. Never invents memory-derived claims —
/// only surfaces declared registry records.
pub(crate) fn component_governance_context(
    server: &MemoryServer,
    project: Option<&str>,
    repo_path: Option<&std::path::Path>,
) -> Result<Value, String> {
    let records = load_component_records(server)?;
    if records.is_empty() {
        return Ok(json!({
            "status": "empty",
            "matches": [],
            "note": "no component governance records seeded",
        }));
    }

    let path = repo_path
        .map(std::path::Path::to_path_buf)
        .or_else(resolve_workspace_git_root);

    let mut matches: Vec<Value> = Vec::new();
    let mut evidence_gaps: Vec<String> = Vec::new();

    if let Some(ref root) = path {
        let (category, matched_id, gaps) = classify_repo(&records, root, None);
        evidence_gaps.extend(gaps);
        if let Some(id) = matched_id.as_deref() {
            if let Some(record) = find_record_by_id(&records, id) {
                matches.push(component_match_summary(
                    record,
                    &category,
                    "repo_remote_or_path",
                ));
            }
        }
        // Also surface other records that list this path's origin as a consumer
        // context when the remote matches a known owner_repo of a related component.
        if let Ok(remote) = run_git_readonly(root, &["remote", "get-url", "origin"]) {
            let owner = normalize_remote_to_owner_repo(&remote);
            for record in &records {
                let id = record
                    .get("component_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if matches
                    .iter()
                    .any(|m| m.get("component_id").and_then(Value::as_str) == Some(id))
                {
                    continue;
                }
                let owner_repo = record
                    .get("owner_repo")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if owner_repo.eq_ignore_ascii_case(&owner) {
                    let component_type = record
                        .get("component_type")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let cat = match component_type {
                        "kernel" => CATEGORY_KERNEL_DRIFT,
                        "runtime_adapter" => CATEGORY_ALLOWED_ADAPTER_POLICY,
                        "workflow_bridge" => CATEGORY_BRIDGE,
                        "frontend_app_shell" => CATEGORY_FRONTEND_SHELL,
                        _ => CATEGORY_UNKNOWN,
                    };
                    matches.push(component_match_summary(record, cat, "shared_owner_repo"));
                }
            }
        }
    }

    // Project-name hint (named library) — match owner_repo suffix or component_id.
    if let Some(proj) = project.filter(|p| !p.is_empty()) {
        let proj_lc = proj.to_ascii_lowercase();
        for record in &records {
            let id = record
                .get("component_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if matches
                .iter()
                .any(|m| m.get("component_id").and_then(Value::as_str) == Some(id))
            {
                continue;
            }
            let owner = record
                .get("owner_repo")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let id_lc = id.to_ascii_lowercase();
            if owner.ends_with(&format!("/{proj_lc}"))
                || owner.contains(&proj_lc)
                || id_lc.contains(&proj_lc)
            {
                let component_type = record
                    .get("component_type")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let cat = match component_type {
                    "kernel" => CATEGORY_KERNEL_DRIFT,
                    "runtime_adapter" => CATEGORY_ALLOWED_ADAPTER_POLICY,
                    "workflow_bridge" => CATEGORY_BRIDGE,
                    "frontend_app_shell" => CATEGORY_FRONTEND_SHELL,
                    _ => CATEGORY_UNKNOWN,
                };
                matches.push(component_match_summary(record, cat, "project_name_hint"));
            }
        }
    }

    Ok(json!({
        "status": "completed",
        "authority": "governance_registry",
        "note": "Registry records only — stale/unknown must not be presented as memory-derived current truth",
        "workspace_path": path.as_ref().map(|p| p.display().to_string()),
        "project": project,
        "matches": matches,
        "evidence_gaps": evidence_gaps,
    }))
}

fn component_match_summary(record: &Value, category: &str, match_reason: &str) -> Value {
    let freshness = record_freshness(record);
    let drift = compact_drift_summary(record);
    let blocked: Vec<Value> = record
        .get("known_drift")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|d| {
            matches!(
                d.get("classification").and_then(Value::as_str),
                Some("blocked_fork")
            )
        })
        .cloned()
        .collect();
    json!({
        "component_id": record.get("component_id"),
        "component_type": record.get("component_type"),
        "owner_repo": record.get("owner_repo"),
        "category": category,
        "match_reason": match_reason,
        "freshness": freshness,
        "upstream_prereqs": record.get("upstream_prereqs").cloned().unwrap_or_else(|| json!([])),
        "known_drift": drift,
        "blocked_forks": blocked,
        "backflow_candidates": record.get("backflow_candidates").cloned().unwrap_or_else(|| json!([])),
        "last_checked_ref": record.get("last_checked_ref"),
    })
}

/// Compact warning lines for status/briefing when governance is stale or blocked.
pub(crate) fn component_governance_warning_lines(context: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(matches) = context.get("matches").and_then(Value::as_array) else {
        return lines;
    };
    for m in matches {
        let id = m.get("component_id").and_then(Value::as_str).unwrap_or("?");
        let state = m
            .get("freshness")
            .and_then(|f| f.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if state == "stale" || state == "unknown" {
            let label = m
                .get("freshness")
                .and_then(|f| f.get("label"))
                .and_then(Value::as_str)
                .unwrap_or(state);
            lines.push(format!("component governance `{id}` is {state}: {label}"));
        }
        if let Some(blocked) = m.get("blocked_forks").and_then(Value::as_array) {
            for b in blocked {
                let area = b.get("area").and_then(Value::as_str).unwrap_or("?");
                lines.push(format!(
                    "component `{id}` blocked_fork: {area} — do not cut over until resolved"
                ));
            }
        }
        if let Some(drift) = m.get("known_drift").and_then(Value::as_array) {
            for d in drift.iter().take(3) {
                let class = d
                    .get("classification")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if class == "backflow_candidate" || class == "retire_delete" {
                    let area = d.get("area").and_then(Value::as_str).unwrap_or("?");
                    lines.push(format!("component `{id}` {class}: {area}"));
                }
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests;
