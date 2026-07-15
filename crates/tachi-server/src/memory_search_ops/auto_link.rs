use crate::memory_search_ops::confidence_reinforce::{
    apply_confidence_reinforcement, confidence_increment, vector_similarity_between,
};
use crate::{DbScope, MemoryServer};
use memcore::{MemoryEntry, MemoryStore};
use serde_json::json;
use std::collections::HashSet;
use std::time::{Duration, Instant};

const REINFORCEMENT_MIN_SIMILARITY: f64 = 0.75;
const REINFORCEMENT_DUPLICATE_SIMILARITY: f64 = 0.95;

// ---------------------------------------------------------------------------
// tachi#1097 PERF-T3 S1 — Auto-link phase-attribution receipt.
//
// Pure observation: the four skip points (self-id :143, training-seed :146,
// no-shared-entities :153, neither-supersede-nor-reinforce :175) are NOT
// changed — we only count them. The edge write at :210-234 is NOT changed —
// we only time it. Read time = per-entity `with_*_store_read` searches
// (auto_link.rs:136/:138); write time = per-edge `with_*_store` writes
// (:231/:233). No new DB I/O, no quality logic touched.
//
// Per #1097 D2 this receipt carries ONLY the existing `entry.id` (the save
// response's UUID, captured into the spawned closure at auto_link.rs:110 —
// no query/trace id is minted, none exists in the codebase). Per D5 it
// contains no entity names (entities are the search queries here, so naming
// them would log query text), no memory content, no DB path.
//
// The receipt is emitted from INSIDE the spawned task because the save
// handler returns `"auto_link": "pending"` (handler.rs:261-262) without
// awaiting it — the closure is the only place the finished counts exist.
// ---------------------------------------------------------------------------

/// Per-call receipt for one `spawn_auto_linking` invocation, emitted via
/// `tracing::info!` from inside the spawned task. See the module-level
/// honesty rules above for what is and is not populated.
#[derive(Debug, Clone)]
pub struct AutoLinkReceipt {
    /// The save-generated UUID of the entry whose entities drove this
    /// auto-link pass. Already in the save response (handler.rs:47-49 /
    /// :261-262), contains no content; carried here per #1097 D2 so a log
    /// reader can pair this receipt with its save response without minting
    /// a new trace/operation id (none exists codebase-wide).
    pub entry_id: String,
    /// `entry.entities.iter().cloned().collect::<HashSet>().len()` —
    /// deduplicated entity count driving the per-entity search loop.
    pub entity_count: usize,
    /// Number of per-entity searches actually executed. Equals
    /// `entity_count` on the current code path (one search per entity) but
    /// counted separately so future short-circuit logic does not silently
    /// mis-state the executed work.
    pub searches_executed: usize,
    /// Number of search results examined across all per-entity searches
    /// (the inner `for result in results` loop at auto_link.rs:142).
    pub candidates_examined: usize,
    /// Number of edges for which `save_edge_action` was actually invoked
    /// (auto_link.rs:230-234) — every write attempt.
    pub edges_attempted: usize,
    /// Number of edge writes that returned `Ok(())`.
    pub edges_written: usize,
    /// Skip-counter breakdown covering the four `continue` points in the
    /// per-hit loop (auto_link.rs:143/146/153/175). Their sum plus
    /// `edges_attempted` equals `candidates_examined`.
    pub skipped_self_id: usize,
    pub skipped_training_seed: usize,
    pub skipped_no_shared_entities: usize,
    pub skipped_no_supersede_or_reinforce: usize,
    /// Wall time inside per-entity `with_*_store_read` search calls
    /// (auto_link.rs:136/:138).
    pub read_elapsed: Duration,
    /// Wall time inside per-edge `with_*_store` write calls
    /// (auto_link.rs:231/:233).
    pub write_elapsed: Duration,
    /// Wall time of the whole spawned task end-to-end.
    pub total_elapsed: Duration,
}

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

/// Unique entities present in both lists (order-independent). Used so duplicate
/// labels in either entry cannot inflate `shared_count` past the fog floor.
pub(crate) fn unique_shared_entities(a: &[String], b: &[String]) -> Vec<String> {
    let set_a: HashSet<&str> = a.iter().map(String::as_str).collect();
    let mut out: HashSet<String> = HashSet::new();
    for e in b {
        if set_a.contains(e.as_str()) {
            out.insert(e.clone());
        }
    }
    out.into_iter().collect()
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
    // Dedup source entities so duplicate labels cannot inflate shared_count
    // past the #773 related_to fog floor (Gemini #905 review).
    let auto_link_entities: HashSet<String> = entry.entities.iter().cloned().collect();
    let auto_link_entity_list: Vec<String> = auto_link_entities.iter().cloned().collect();

    tokio::spawn(async move {
        // #1097 S1 phase-attribution counters (pure observation — no
        // behavioral change to the four skip points or the edge write).
        let mut searches_executed = 0_usize;
        let mut candidates_examined = 0_usize;
        let mut edges_attempted = 0_usize;
        let mut edges_written = 0_usize;
        let mut skipped_self_id = 0_usize;
        let mut skipped_training_seed = 0_usize;
        let mut skipped_no_shared_entities = 0_usize;
        let mut skipped_no_supersede_or_reinforce = 0_usize;
        let mut read_elapsed = Duration::ZERO;
        let mut write_elapsed = Duration::ZERO;
        let task_start = Instant::now();

        for entity in &auto_link_entity_list {
            let query = entity.clone();
            let search_action = |store: &mut MemoryStore| {
                store
                    .search(
                        &query,
                        Some(memcore::SearchOptions {
                            top_k: 5,
                            // Auto-link is a write-side side effect that probes related memories.
                            // It must not bias ACT-R access stats for entries the user never read.
                            record_access: false,
                            ..Default::default()
                        }),
                    )
                    .map_err(|e| format!("{}", e))
            };

            let read_timer = Instant::now();
            let search_res = if let Some(ref p) = named_project {
                auto_link_server.with_named_project_store_read(p, search_action)
            } else {
                auto_link_server.with_store_for_scope_read(target_db, search_action)
            };
            read_elapsed += read_timer.elapsed();
            searches_executed += 1;

            if let Ok(results) = search_res {
                for result in results {
                    candidates_examined += 1;
                    if result.entry.id == auto_link_id {
                        skipped_self_id += 1;
                        continue;
                    }
                    if is_training_seed(&result.entry) {
                        skipped_training_seed += 1;
                        continue;
                    }
                    // Unique shared entities only — duplicate entity labels must not
                    // count as multi-entity agreement for related_to/supersede.
                    let shared =
                        unique_shared_entities(&auto_link_entity_list, &result.entry.entities);
                    if shared.is_empty() {
                        skipped_no_shared_entities += 1;
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
                    if !supersedes && !reinforces {
                        // tachi#773 item 2: auto_link no longer emits `related_to` at
                        // all. Entity co-occurrence without a supersede/reinforce
                        // signal is query-time recoverable (shared-entity search)
                        // and isn't worth a persisted fog edge — and the memcore
                        // edge-write choke point (relation_ontology) would reject
                        // `related_to` on new writes anyway (item 1).
                        skipped_no_supersede_or_reinforce += 1;
                        continue;
                    }
                    let relation = if supersedes {
                        "supersedes"
                    } else {
                        "reinforces"
                    };
                    let weight = if supersedes {
                        0.9
                    } else {
                        vector_similarity.unwrap_or(0.0)
                    };
                    let edge = memcore::MemoryEdge {
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
                                .mark_superseded_closing_validity(
                                    &result.entry.id,
                                    &auto_link_id,
                                    &now,
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
                    edges_attempted += 1;
                    let write_timer = Instant::now();
                    let write_res = if let Some(ref p) = named_project {
                        auto_link_server.with_named_project_store(p, save_edge_action)
                    } else {
                        auto_link_server.with_store_for_scope(target_db, save_edge_action)
                    };
                    write_elapsed += write_timer.elapsed();
                    if write_res.is_ok() {
                        edges_written += 1;
                    }
                }
            }
        }

        // Emit the receipt from inside the spawned task — the save handler
        // returned `"auto_link": "pending"` (handler.rs:261-262) long before
        // this point, so this closure is the only place the finished counts
        // exist. Fields are whitelisted per #1097 D5: no entity names
        // (entities ARE the search queries here), no memory content, no DB
        // path. The `auto_link_id` is the save-generated UUID already in the
        // save response, so a log reader pairs receipt↔save without minting
        // a new trace id (none exists codebase-wide, per D2).
        let receipt = AutoLinkReceipt {
            entry_id: auto_link_id.clone(),
            entity_count: auto_link_entity_list.len(),
            searches_executed,
            candidates_examined,
            edges_attempted,
            edges_written,
            skipped_self_id,
            skipped_training_seed,
            skipped_no_shared_entities,
            skipped_no_supersede_or_reinforce,
            read_elapsed,
            write_elapsed,
            total_elapsed: task_start.elapsed(),
        };
        tracing::info!(
            entry_id = %receipt.entry_id,
            entity_count = receipt.entity_count,
            searches_executed = receipt.searches_executed,
            candidates_examined = receipt.candidates_examined,
            edges_attempted = receipt.edges_attempted,
            edges_written = receipt.edges_written,
            skipped_self_id = receipt.skipped_self_id,
            skipped_training_seed = receipt.skipped_training_seed,
            skipped_no_shared_entities = receipt.skipped_no_shared_entities,
            skipped_no_supersede_or_reinforce = receipt.skipped_no_supersede_or_reinforce,
            read_elapsed_us = receipt.read_elapsed.as_micros() as u64,
            write_elapsed_us = receipt.write_elapsed.as_micros() as u64,
            total_elapsed_us = receipt.total_elapsed.as_micros() as u64,
            "auto_link phase receipt (tachi#1097 S1)"
        );
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
        new_entry.entities = vec!["Sigil".to_string(), "tachi-server".to_string()];
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

    #[test]
    fn unique_shared_entities_dedups_duplicate_labels() {
        // Discrimination: ["sigil","sigil"] ∩ ["sigil"] must count as 1,
        // not 2 — otherwise the fog floor is bypassed by noisy entity lists.
        let a = vec!["sigil".into(), "sigil".into()];
        let b = vec!["sigil".into(), "sigil".into(), "sigil".into()];
        let shared = unique_shared_entities(&a, &b);
        assert_eq!(shared.len(), 1);

        let a2 = vec!["sigil".into(), "tachi-server".into(), "sigil".into()];
        let b2 = vec!["tachi-server".into(), "sigil".into()];
        let shared2 = unique_shared_entities(&a2, &b2);
        assert_eq!(shared2.len(), 2);
    }

    /// tachi#773 item 2: auto_link must never select `related_to` as the
    /// emitted relation string. This mirrors `spawn_auto_linking`'s decision
    /// logic (supersedes -> "supersedes", reinforces -> "reinforces",
    /// otherwise -> skip entirely) without the async/store plumbing, so it
    /// stays a fast unit test. Red pre-#773-item-2: this same shape of
    /// decision used to fall through to `Some("related_to")` whenever
    /// entities were shared without a supersede/reinforce signal.
    fn auto_link_emitted_relation(supersedes: bool, reinforces: bool) -> Option<&'static str> {
        if supersedes {
            Some("supersedes")
        } else if reinforces {
            Some("reinforces")
        } else {
            None
        }
    }

    #[test]
    fn auto_link_never_emits_related_to() {
        for supersedes in [true, false] {
            for reinforces in [true, false] {
                let relation = auto_link_emitted_relation(supersedes, reinforces);
                assert_ne!(
                    relation,
                    Some("related_to"),
                    "auto_link must never select related_to (supersedes={supersedes}, reinforces={reinforces})"
                );
            }
        }
        // Neither signal fires -> no edge at all (query-time recoverable via
        // shared-entity search instead of a persisted fog edge).
        assert_eq!(auto_link_emitted_relation(false, false), None);
    }
}
