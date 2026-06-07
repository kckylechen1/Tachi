use super::*;
use crate::memory_search_ops::confidence_reinforce::{
    apply_confidence_reinforcement, confidence_increment, vector_similarity_between,
};
use std::collections::HashSet;

const REINFORCEMENT_MIN_SIMILARITY: f64 = 0.75;
const REINFORCEMENT_DUPLICATE_SIMILARITY: f64 = 0.95;

pub(crate) fn path_root(path: &str) -> &str {
    path.trim_matches('/').split('/').next().unwrap_or("")
}

pub(crate) fn is_training_seed(entry: &MemoryEntry) -> bool {
    entry.path == "/sft"
        || entry.path.starts_with("/sft/")
        || entry.topic.eq_ignore_ascii_case("sft-memory")
        || entry.source.eq_ignore_ascii_case("sft_seed")
        || entry
            .metadata
            .get("training_sample")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
}

pub(crate) fn is_newer_than(new_ts: &str, old_ts: &str) -> bool {
    let new = chrono::DateTime::parse_from_rfc3339(new_ts);
    let old = chrono::DateTime::parse_from_rfc3339(old_ts);
    match (new, old) {
        (Ok(new), Ok(old)) => new > old,
        _ => new_ts > old_ts,
    }
}

pub(crate) fn should_supersede(
    new_entry: &MemoryEntry,
    old_entry: &MemoryEntry,
    shared_count: usize,
    _symbolic_score: f64,
) -> bool {
    let same_path = new_entry.path == old_entry.path;
    let same_non_empty_topic =
        !new_entry.topic.trim().is_empty() && new_entry.topic == old_entry.topic;

    matches!(new_entry.category.as_str(), "fact" | "preference")
        && matches!(old_entry.category.as_str(), "fact" | "preference")
        && is_newer_than(&new_entry.timestamp, &old_entry.timestamp)
        && shared_count >= 2
        && (same_path || same_non_empty_topic)
        && path_root(&new_entry.path) == path_root(&old_entry.path)
}

pub(crate) fn should_reinforce(
    new_entry: &MemoryEntry,
    old_entry: &MemoryEntry,
    shared_count: usize,
    similarity: f64,
    supersedes: bool,
) -> bool {
    !supersedes
        && shared_count > 0
        && matches!(new_entry.category.as_str(), "fact" | "preference")
        && matches!(old_entry.category.as_str(), "fact" | "preference")
        && path_root(&new_entry.path) == path_root(&old_entry.path)
        && (REINFORCEMENT_MIN_SIMILARITY..REINFORCEMENT_DUPLICATE_SIMILARITY).contains(&similarity)
}

pub(crate) fn numbers_in_text(text: &str) -> HashSet<String> {
    static NUMBER_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = NUMBER_RE.get_or_init(|| {
        regex::Regex::new(r"\b\d{1,3}(?:,\d{3})*(?:\.\d+)?%?\b|\b\d+(?:\.\d+)?%?\b").unwrap()
    });
    re.find_iter(text)
        .map(|m| m.as_str().replace(',', "").to_ascii_lowercase())
        .collect()
}

pub(crate) fn has_numeric_mismatch(new_entry: &MemoryEntry, old_entry: &MemoryEntry) -> bool {
    let new_numbers = numbers_in_text(&new_entry.text);
    let old_numbers = numbers_in_text(&old_entry.text);
    !new_numbers.is_empty() && !old_numbers.is_empty() && new_numbers != old_numbers
}

pub(crate) fn spawn_auto_linking(
    server: &MemoryServer,
    entry: &MemoryEntry,
    target_db: DbScope,
    named_project: Option<String>,
) {
    if is_training_seed(entry) {
        return;
    }

    let auto_link_server = server.clone();
    let auto_link_id = entry.id.clone();
    let auto_link_entry = entry.clone();
    let auto_link_entities = entry.entities.clone();

    tokio::spawn(async move {
        for entity in &auto_link_entities {
            let query = entity.clone();
            let search_action = |store: &mut MemoryStore| {
                store
                    .search(
                        &query,
                        Some(memory_core::SearchOptions {
                            top_k: 5,
                            // Auto-link is a write-side side effect that probes related memories.
                            // It must not bias ACT-R access stats for entries the user never read.
                            record_access: false,
                            ..Default::default()
                        }),
                    )
                    .map_err(|e| format!("{}", e))
            };

            let search_res = if let Some(ref p) = named_project {
                auto_link_server.with_named_project_store_read(p, search_action)
            } else {
                auto_link_server.with_store_for_scope_read(target_db, search_action)
            };

            if let Ok(results) = search_res {
                for result in results {
                    if result.entry.id == auto_link_id {
                        continue;
                    }
                    if is_training_seed(&result.entry) {
                        continue;
                    }
                    let shared: Vec<String> = result
                        .entry
                        .entities
                        .iter()
                        .filter(|e| auto_link_entities.contains(e))
                        .cloned()
                        .collect();
                    if shared.is_empty() {
                        continue;
                    }

                    let now = chrono::Utc::now().to_rfc3339();
                    let vector_similarity =
                        vector_similarity_between(&auto_link_entry, &result.entry);
                    let supersedes = should_supersede(
                        &auto_link_entry,
                        &result.entry,
                        shared.len(),
                        result.score.symbolic,
                    );
                    let reinforces = vector_similarity.is_some_and(|similarity| {
                        should_reinforce(
                            &auto_link_entry,
                            &result.entry,
                            shared.len(),
                            similarity,
                            supersedes,
                        )
                    });
                    let relation = if supersedes {
                        "supersedes"
                    } else if reinforces {
                        "reinforces"
                    } else {
                        "related_to"
                    };
                    let weight = if supersedes {
                        0.9
                    } else if reinforces {
                        vector_similarity.unwrap_or(0.0)
                    } else {
                        0.5
                    };
                    let edge = memory_core::MemoryEdge {
                        source_id: auto_link_id.clone(),
                        target_id: result.entry.id.clone(),
                        relation: relation.to_string(),
                        weight,
                        metadata: json!({
                            "auto_link": true,
                            "shared_entities": shared,
                            "similarity": vector_similarity,
                            "confidence_increment": reinforces.then(|| confidence_increment(weight)),
                        }),
                        created_at: now.clone(),
                        valid_from: String::new(),
                        // Edges are only closed/expired when supersession is explicitly reversed.
                        valid_to: None,
                    };
                    let save_edge_action = |store: &mut MemoryStore| {
                        store.add_edge(&edge).map_err(|e| format!("{}", e))?;
                        if supersedes {
                            store
                                .connection()
                                .execute(
                                    "UPDATE memories SET superseded_by = ?1, updated_at = ?2 WHERE id = ?3 AND superseded_by IS NULL",
                                    rusqlite::params![auto_link_id, now, result.entry.id],
                                )
                                .map_err(|e| format!("{e}"))?;
                        } else if reinforces {
                            apply_confidence_reinforcement(
                                store,
                                &result.entry.id,
                                confidence_increment(weight),
                                &now,
                            )?;
                        }
                        Ok(())
                    };
                    let _ = if let Some(ref p) = named_project {
                        auto_link_server.with_named_project_store(p, save_edge_action)
                    } else {
                        auto_link_server.with_store_for_scope(target_db, save_edge_action)
                    };
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            path: "/test".into(),
            summary: text[..text.len().min(30)].into(),
            text: text.into(),
            importance: 0.7,
            timestamp: chrono::Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: "".into(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".into(),
            source: "test".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn path_root_extracts_first_segment() {
        assert_eq!(path_root("/project/alpha"), "project");
        assert_eq!(path_root("/wiki/entry"), "wiki");
        assert_eq!(path_root("no-slash"), "no-slash");
        assert_eq!(path_root("/"), "");
    }

    #[test]
    fn is_training_seed_detects_sft_boundaries() {
        let mut by_path = test_entry("sft-path", "training sample");
        by_path.path = "/sft/v4/strict/engineering/1".to_string();
        assert!(is_training_seed(&by_path));

        let mut by_source = test_entry("sft-source", "training sample");
        by_source.source = "sft_seed".to_string();
        assert!(is_training_seed(&by_source));

        let mut by_metadata = test_entry("sft-metadata", "training sample");
        by_metadata.metadata = json!({"training_sample": true});
        assert!(is_training_seed(&by_metadata));

        let live = test_entry("live", "current operational memory");
        assert!(!is_training_seed(&live));
    }

    #[test]
    fn is_newer_than_compares_timestamps() {
        assert!(is_newer_than(
            "2025-01-02T00:00:00Z",
            "2025-01-01T00:00:00Z"
        ));
        assert!(!is_newer_than(
            "2025-01-01T00:00:00Z",
            "2025-01-02T00:00:00Z"
        ));
    }

    #[test]
    fn should_supersede_rejects_cross_path_empty_topic_facts() {
        let mut new_entry = test_entry("new", "release prep summary mentions cleanup");
        let mut old_entry = test_entry("old", "clean-cli dry-run implementation fact");
        new_entry.path = "/scratch/tachi/v1.5-release-prep".to_string();
        old_entry.path = "/scratch/tachi/clean-cli-integration".to_string();
        new_entry.timestamp = "2026-06-06T18:53:45Z".to_string();
        old_entry.timestamp = "2026-06-06T18:47:28Z".to_string();
        new_entry.entities = vec!["Sigil".to_string(), "memory-server".to_string()];
        old_entry.entities = new_entry.entities.clone();

        assert!(!should_supersede(&new_entry, &old_entry, 2, 1.0));
    }

    #[test]
    fn should_supersede_allows_same_path_or_non_empty_topic() {
        let mut new_entry = test_entry("new", "new canonical fact");
        let mut old_entry = test_entry("old", "old canonical fact");
        new_entry.timestamp = "2026-06-06T18:53:45Z".to_string();
        old_entry.timestamp = "2026-06-06T18:47:28Z".to_string();
        new_entry.path = "/scratch/tachi/same".to_string();
        old_entry.path = "/scratch/tachi/same".to_string();
        assert!(should_supersede(&new_entry, &old_entry, 2, 0.0));

        new_entry.path = "/scratch/tachi/new".to_string();
        old_entry.path = "/scratch/tachi/old".to_string();
        new_entry.topic = "release-fact".to_string();
        old_entry.topic = "release-fact".to_string();
        assert!(should_supersede(&new_entry, &old_entry, 2, 0.0));
    }

    #[test]
    fn should_reinforce_requires_vector_similarity_gray_zone() {
        let mut new_entry = test_entry("new", "canonical preference");
        let mut old_entry = test_entry("old", "nearby preference");
        new_entry.path = "/project/a".to_string();
        old_entry.path = "/project/b".to_string();
        new_entry.category = "preference".to_string();
        old_entry.category = "preference".to_string();

        assert!(should_reinforce(&new_entry, &old_entry, 1, 0.82, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 0, 0.82, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 1, 0.60, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 1, 0.97, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 1, 0.82, true));
    }
}
