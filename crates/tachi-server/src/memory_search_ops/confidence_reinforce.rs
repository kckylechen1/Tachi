use crate::memory_search_ops::auto_link::{should_reinforce, should_supersede};
use memcore::{ExpectedMemoryState, MemoryEntry, MemoryStore};
use serde_json::json;
use std::collections::HashSet;

pub(crate) fn confidence_increment(similarity: f64) -> f64 {
    (0.1 * similarity).clamp(0.0, 0.1)
}

#[cfg(test)]
pub(crate) fn apply_confidence_reinforcement(
    store: &mut MemoryStore,
    reinforced_id: &str,
    increment: f64,
    reinforced_at: &str,
) -> Result<(), String> {
    store
        .reinforce_confidence(reinforced_id, increment, reinforced_at)
        .map_err(|e| format!("update confidence reinforcement: {e}"))
}

pub(crate) fn collect_reinforcement_candidates(
    store: &MemoryStore,
    entry: &MemoryEntry,
) -> Result<Vec<MemoryEntry>, String> {
    store
        .entity_overlap_candidates(&entry.id, &entry.entities)
        .map_err(|e| format!("collect confidence reinforcement candidates: {e}"))
}

pub(crate) fn apply_confidence_reinforcement_links(
    store: &mut MemoryStore,
    entry: &MemoryEntry,
) -> Result<usize, String> {
    let Some(entry) = store
        .get(&entry.id)
        .map_err(|error| format!("read committed reinforcement source: {error}"))?
    else {
        return Ok(0);
    };
    if entry.entities.is_empty() || entry.vector.is_none() {
        return Ok(0);
    }

    let mut reinforced = 0usize;
    let mut seen_targets = HashSet::<String>::new();
    for candidate in collect_reinforcement_candidates(store, &entry)? {
        if !seen_targets.insert(candidate.id.clone()) {
            continue;
        }
        let shared: Vec<String> = candidate
            .entities
            .iter()
            .filter(|candidate_entity| entry.entities.contains(candidate_entity))
            .cloned()
            .collect();
        if shared.is_empty() {
            continue;
        }

        let symbolic_score = shared
            .iter()
            .map(|entity| {
                memcore::scorer::symbolic_score(
                    entity,
                    &candidate.text,
                    &candidate.keywords,
                    &candidate.entities,
                )
            })
            .fold(0.0_f64, f64::max);
        let supersedes = should_supersede(&entry, &candidate, shared.len(), symbolic_score);
        let Some(similarity) = vector_similarity_between(&entry, &candidate) else {
            continue;
        };
        if !should_reinforce(&entry, &candidate, shared.len(), similarity, supersedes) {
            continue;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let increment = confidence_increment(similarity);
        let edge = memcore::MemoryEdge {
            source_id: entry.id.clone(),
            target_id: candidate.id.clone(),
            relation: "reinforces".to_string(),
            weight: similarity,
            metadata: json!({
                "auto_link": true,
                "shared_entities": shared,
                "similarity": similarity,
                "confidence_increment": increment,
            }),
            created_at: now.clone(),
            valid_from: String::new(),
            valid_to: None,
        };
        // tachi#1646: vector-similarity `reinforces` edges are a heuristic
        // Tachi computed itself.
        let source_superseded_by = store
            .supersession_target(&entry.id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("reinforcement source disappeared: {}", entry.id))?;
        let target_superseded_by = store
            .supersession_target(&candidate.id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("reinforcement target disappeared: {}", candidate.id))?;
        let committed = store
            .commit_confidence_reinforcement(
                &edge,
                &memcore::db::EdgeProvenance {
                    authority: Some(memcore::db::EdgeAuthority::DerivedHeuristic),
                    ..Default::default()
                },
                increment,
                &now,
                &ExpectedMemoryState::from_entry(&entry, source_superseded_by.as_deref()),
                &ExpectedMemoryState::from_entry(&candidate, target_superseded_by.as_deref()),
            )
            .map_err(|error| error.to_string())?;
        reinforced += usize::from(committed);
    }

    Ok(reinforced)
}

pub(crate) fn vector_similarity_between(
    new_entry: &MemoryEntry,
    old_entry: &MemoryEntry,
) -> Option<f64> {
    let new_vec = new_entry.vector.as_deref()?;
    let old_vec = old_entry.vector.as_deref()?;
    if new_vec.is_empty() || new_vec.len() != old_vec.len() {
        return None;
    }
    let similarity = memcore::scorer::cosine_similarity(new_vec, old_vec);
    if !similarity.is_finite() {
        return None;
    }
    Some(similarity.clamp(0.0, 1.0))
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
            scored_count: 0,
            last_access: None,
            last_use_at: None,
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
    fn confidence_reinforcement_updates_metadata_confidence() {
        let mut store = memcore::MemoryStore::open_in_memory().unwrap();
        let mut old_entry = test_entry("old", "durable supported fact");
        old_entry.metadata = json!({ "confidence": 0.70 });
        store.upsert(&old_entry).unwrap();

        apply_confidence_reinforcement(&mut store, "old", 0.08, "2026-01-01T00:00:00Z").unwrap();

        let updated = store.get("old").unwrap().unwrap();
        let confidence = updated
            .metadata
            .get("confidence")
            .and_then(|value| value.as_f64())
            .unwrap();
        assert!((confidence - 0.78).abs() < 1e-9, "confidence={confidence}");
        assert_eq!(
            updated
                .metadata
                .get("confidence_reinforced_at")
                .and_then(|value| value.as_str()),
            Some("2026-01-01T00:00:00Z")
        );
    }

    #[test]
    fn confidence_reinforcement_falls_back_to_importance() {
        let mut store = memcore::MemoryStore::open_in_memory().unwrap();
        let mut old_entry = test_entry("old", "durable supported fact");
        old_entry.importance = 0.60;
        old_entry.metadata = json!({});
        store.upsert(&old_entry).unwrap();

        apply_confidence_reinforcement(&mut store, "old", 0.10, "2026-01-01T00:00:00Z").unwrap();

        let updated = store.get("old").unwrap().unwrap();
        let confidence = updated
            .metadata
            .get("confidence")
            .and_then(|value| value.as_f64())
            .unwrap();
        assert!((confidence - 0.70).abs() < 1e-9, "confidence={confidence}");
    }

    #[test]
    fn vector_similarity_ignores_non_finite_vectors() {
        let mut new_entry = test_entry("new", "new vector");
        let mut old_entry = test_entry("old", "old vector");
        new_entry.vector = Some(vec![f32::NAN, 1.0]);
        old_entry.vector = Some(vec![1.0, 0.0]);

        assert!(vector_similarity_between(&new_entry, &old_entry).is_none());
    }

    #[test]
    fn apply_confidence_reinforcement_links_creates_reinforces_edge() {
        let mut store = memcore::MemoryStore::open_in_memory().unwrap();
        let mut old_entry = test_entry("old", "Acme deployment policy remains stable");
        old_entry.entities = vec!["Acme".to_string()];
        let mut old_vec = vec![0.0; 1024];
        old_vec[0] = 1.0;
        old_entry.vector = Some(old_vec);
        old_entry.metadata = json!({ "confidence": 0.50 });
        store.upsert(&old_entry).unwrap();

        let mut new_entry = test_entry("new", "Acme deployment policy has another supporting note");
        new_entry.entities = vec!["Acme".to_string()];
        let mut new_vec = vec![0.0; 1024];
        new_vec[0] = 0.8;
        new_vec[1] = 0.6;
        new_entry.vector = Some(new_vec);
        store.upsert(&new_entry).unwrap();

        let count = apply_confidence_reinforcement_links(&mut store, &new_entry).unwrap();
        assert_eq!(count, 1);

        let edges = store
            .get_edges("new", "outgoing", Some("reinforces"))
            .unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_id, "old");

        let updated = store.get("old").unwrap().unwrap();
        let confidence = updated
            .metadata
            .get("confidence")
            .and_then(|value| value.as_f64())
            .unwrap();
        assert!(confidence > 0.50, "confidence={confidence}");
    }
}
