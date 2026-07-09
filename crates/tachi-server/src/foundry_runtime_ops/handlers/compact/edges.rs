use crate::server_state::{DbScope, MemoryServer};
use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore};
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;

fn compact_artifact_kind(entry: &MemoryEntry) -> Option<&str> {
    entry
        .metadata
        .get("artifact_kind")
        .and_then(serde_json::Value::as_str)
}

fn import_signal_relations(signal_text: &str) -> Vec<&'static str> {
    let lower = signal_text.to_ascii_lowercase();
    let mut relations = vec!["distilled_from", "causes"];
    if [
        "fix",
        "fixed",
        "repair",
        "error",
        "failed",
        "failure",
        "bug",
        "panic",
        "exception",
        "修复",
        "错误",
        "失败",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        relations.push("fixed_by");
    }
    if [
        "reject", "rejected", "avoid", "do not", "don't", "never", "拒绝", "避免", "不要", "禁止",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        relations.push("rejected_because");
    }
    relations
}

fn build_compact_session_import_edges(
    entries: &[MemoryEntry],
    created_at: &str,
) -> Vec<memcore::MemoryEdge> {
    let rollups = entries
        .iter()
        .filter(|entry| compact_artifact_kind(entry) == Some("compact_rollup"))
        .collect::<Vec<_>>();
    let signals = entries
        .iter()
        .filter(|entry| compact_artifact_kind(entry) == Some("durable_signal"))
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut seen = HashSet::new();

    for signal in signals {
        for rollup in &rollups {
            for relation in import_signal_relations(&signal.text) {
                let (source_id, target_id, weight) = match relation {
                    "distilled_from" | "rejected_because" => {
                        (signal.id.clone(), rollup.id.clone(), 0.8)
                    }
                    "fixed_by" => (rollup.id.clone(), signal.id.clone(), 0.85),
                    _ => (rollup.id.clone(), signal.id.clone(), 0.7),
                };
                if seen.insert((source_id.clone(), target_id.clone(), relation.to_string())) {
                    edges.push(memcore::MemoryEdge {
                        source_id,
                        target_id,
                        relation: relation.to_string(),
                        weight,
                        metadata: json!({
                            "source": "compact_session_memory",
                            "artifact_kind": "durable_signal",
                        }),
                        created_at: created_at.to_string(),
                        valid_from: created_at.to_string(),
                        valid_to: None,
                    });
                }
            }
        }
    }

    edges
}

pub(in crate::foundry_runtime_ops::handlers::compact) fn persist_compact_session_import_edges(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&PathBuf>,
    entries: &[MemoryEntry],
) -> Result<usize, String> {
    let created_at = Utc::now().to_rfc3339();
    let edges = build_compact_session_import_edges(entries, &created_at);
    if edges.is_empty() {
        return Ok(0);
    }
    let save_edges = |store: &mut MemoryStore| {
        for edge in &edges {
            store
                .add_edge(edge)
                .map_err(|e| format!("Failed to save compact_session_memory edge: {e}"))?;
        }
        Ok(edges.len())
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store(project_name, save_edges)
    } else if let Some(db_path) = db_path {
        server.with_path_store(db_path, save_edges)
    } else {
        server.with_store_for_scope(target_db, save_edges)
    }
}
