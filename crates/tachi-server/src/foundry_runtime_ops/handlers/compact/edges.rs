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

/// tachi#1646 offender 2 (#1460 measurement, "keyword-driven relation choice
/// from agent prose"): a substring match against a session's free-form
/// signal text is `DerivedHeuristic` authority, not a verified assertion —
/// it must never be able to select a relation whose activation multiplier
/// (`scorer::graph_relation_activation_weight`, scorer/graph.rs:71-83) sits
/// above the low-multiplier tier (<= 0.70). Before this leaf, the base set
/// unconditionally included `causes` (0.80) and the "fix" keyword group
/// added `fixed_by` (0.80) — both above that line. `causes` is demoted to
/// `follows` (0.70) and `fixed_by` to `references` (0.70); `distilled_from`
/// (0.70, base) and `rejected_because` (0.30, "reject" keyword group) were
/// already at or below the line and are unchanged. No path here reaches
/// `supports` (0.90) or any other relation above 0.70.
fn import_signal_relations(signal_text: &str) -> Vec<&'static str> {
    let lower = signal_text.to_ascii_lowercase();
    let mut relations = vec!["distilled_from", "follows"];
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
        relations.push("references");
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

/// tachi#1646 offender 2: written weight caps at 0.6 under
/// `DerivedHeuristic` authority — a keyword-substring match is not entitled
/// to the same trust as a receipt-backed or structural writer, regardless
/// of which of the (now all <= 0.70-multiplier) relations it picked.
const COMPACT_IMPORT_WEIGHT_CAP: f64 = 0.6;

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
                let (source_id, target_id) = match relation {
                    "distilled_from" | "rejected_because" => (signal.id.clone(), rollup.id.clone()),
                    _ => (rollup.id.clone(), signal.id.clone()),
                };
                if seen.insert((source_id.clone(), target_id.clone(), relation.to_string())) {
                    edges.push(memcore::MemoryEdge {
                        source_id,
                        target_id,
                        relation: relation.to_string(),
                        weight: COMPACT_IMPORT_WEIGHT_CAP,
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
        // tachi#1646: keyword-driven relation choice from agent prose is
        // DerivedHeuristic authority, not a verified assertion.
        for edge in &edges {
            store
                .add_edge_with_provenance(
                    edge,
                    &memcore::db::EdgeProvenance {
                        authority: Some(memcore::db::EdgeAuthority::DerivedHeuristic),
                        ..Default::default()
                    },
                )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// tachi#1646 offender 2: the relation multipliers this door may reach
    /// (`scorer::graph_relation_activation_weight`, scorer/graph.rs:71-83).
    const LOW_MULTIPLIER_RELATIONS: [&str; 4] = [
        "distilled_from",
        "follows",
        "references",
        "rejected_because",
    ];

    fn compact_entry(id: &str, artifact_kind: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/compact".to_string(),
            summary: text.chars().take(30).collect(),
            text: text.to_string(),
            importance: 0.6,
            timestamp: "2026-08-05T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "compact".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: json!({"artifact_kind": artifact_kind}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    /// No keyword combination — including the "fix" and "reject" trigger
    /// groups this function's own base set — may select a relation above
    /// the low-multiplier tier. This is the direct regression test for
    /// offender 2: before this leaf, every signal (regardless of text)
    /// picked `causes` (0.80), and a "fix" keyword additionally picked
    /// `fixed_by` (0.80).
    #[test]
    fn import_signal_relations_never_exceeds_low_multiplier_tier() {
        for text in [
            "plain observation, no trigger words",
            "this fixed a bug and repaired the failure",
            "reject this pattern, avoid it, do not repeat",
            "修复了一个错误",
            "拒绝这个模式",
        ] {
            for relation in import_signal_relations(text) {
                assert!(
                    LOW_MULTIPLIER_RELATIONS.contains(&relation),
                    "import_signal_relations({text:?}) picked {relation:?}, outside the \
                     low-multiplier allowlist {LOW_MULTIPLIER_RELATIONS:?}"
                );
                assert_ne!(
                    relation, "causes",
                    "causes (0.80) must never be reachable here"
                );
                assert_ne!(
                    relation, "fixed_by",
                    "fixed_by (0.80) must never be reachable here"
                );
                assert_ne!(
                    relation, "supports",
                    "supports (0.90) must never be reachable here"
                );
            }
        }
    }

    /// End-to-end through the edge builder: every produced edge caps its
    /// weight at `COMPACT_IMPORT_WEIGHT_CAP` and its relation stays inside
    /// the low-multiplier allowlist, for a signal whose text trips every
    /// keyword group at once.
    #[test]
    fn build_compact_session_import_edges_caps_weight_and_relation_set() {
        let rollup = compact_entry("compact-rollup-1", "compact_rollup", "rollup body");
        let signal = compact_entry(
            "compact-signal-1",
            "durable_signal",
            "this fixed a bug; reject the old approach, avoid it",
        );
        let edges = build_compact_session_import_edges(&[rollup, signal], "2026-08-05T00:00:00Z");
        assert!(
            !edges.is_empty(),
            "keyword-tripping signal must produce edges"
        );
        for edge in &edges {
            assert_eq!(
                edge.weight, COMPACT_IMPORT_WEIGHT_CAP,
                "every compact-import edge weight must be exactly the DerivedHeuristic cap"
            );
            assert!(
                LOW_MULTIPLIER_RELATIONS.contains(&edge.relation.as_str()),
                "edge relation {:?} outside the low-multiplier allowlist",
                edge.relation
            );
        }
    }
}
