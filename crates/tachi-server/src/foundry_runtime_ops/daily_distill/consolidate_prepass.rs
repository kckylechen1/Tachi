//! #1043 D3 — consolidate pre-pass for the distill batch.
//!
//! `run_daily_batch_distill` selects candidate groups (buckets of >=3
//! related raw memories sharing a `path_prefix`/`coherence_key`) and sends
//! every entry in each bucket to the distiller as if it were independent
//! evidence. In practice a bucket routinely contains byte-identical
//! duplicate rows (observed: 121 duplicate sync facts, 227 twin pairs across
//! a real library) — sync jobs and retries that wrote the same fact more
//! than once. Feeding the distiller N copies of the same row wastes prompt
//! budget and lets one over-represented fact dominate the synthesized
//! summary.
//!
//! This pre-pass collapses byte-identical `(path, text)` duplicates within
//! each candidate group down to one survivor BEFORE the group is batched
//! and sent to the distiller — selection sees the deduplicated pool, not
//! the raw one.
//!
//! Scope is deliberately narrow and safe for an unattended background pass:
//! only exact-duplicate rows collapse automatically. Near-duplicates (the
//! jaccard-based `merge_into`/`supersede` judgment calls) stay on the
//! human-reviewed `tachi_memory(action='consolidate')` propose/review/apply
//! loop — this pre-pass does not touch that decision logic, it only reuses
//! the same `merge_into` *mutation* (fold keywords/entities into the
//! survivor, supersede + archive the rest) via
//! [`crate::facade_memory_ops::consolidate_ops::merge_into_for_project_with_expected`],
//! called directly instead of through the proposal-store round trip. That
//! entry point hard-codes `action="merge_into"` and binds the selected
//! source/target snapshots before writing — it cannot reach any other
//! lifecycle action, so this pre-pass has no way to bypass human review for
//! `supersede`/`archive`/`promote_distilled`.

use std::collections::HashMap;

use serde_json::Value;
use tachi_foundry::CandidateGroup;

use crate::server_state::MemoryServer;

/// Collapse byte-identical duplicate entries within each candidate group.
/// Mutates `groups` in place (duplicate entries are removed, the newest
/// survivor stays) and returns how many rows were merged away — the
/// `consolidated` count for `DistillBatchReport`.
pub(super) fn consolidate_duplicate_candidates(
    server: &MemoryServer,
    project: Option<&str>,
    groups: &mut [CandidateGroup],
) -> usize {
    groups
        .iter_mut()
        .map(|group| consolidate_one_group(server, project, group))
        .sum()
}

fn consolidate_one_group(
    server: &MemoryServer,
    project: Option<&str>,
    group: &mut CandidateGroup,
) -> usize {
    // Bucket entry indices by (path, exact text). This must stay byte-exact
    // — no `.trim()` or other normalization — because the spec and tests
    // promise byte-identical dedup only; two rows differing solely in
    // leading/trailing whitespace are distinct evidence, not duplicates.
    // Sharing a bucket with >1 member is a true byte-identical duplicate
    // cluster; better to leave a near-duplicate un-merged than to silently
    // fold two non-identical rows together (#1043 D3 terminal review).
    let mut buckets: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (idx, entry) in group.entries.iter().enumerate() {
        let key = (entry.path.clone(), entry.text.clone());
        buckets.entry(key).or_default().push(idx);
    }

    let mut drop_indices: Vec<usize> = Vec::new();
    for mut indices in buckets.into_values() {
        if indices.len() < 2 {
            continue;
        }
        // Keep the newest row (by timestamp; ties broken by id, both
        // deterministic) as the survivor; merge the rest into it.
        indices.sort_by(|&a, &b| {
            let ea = &group.entries[a];
            let eb = &group.entries[b];
            eb.timestamp
                .cmp(&ea.timestamp)
                .then_with(|| ea.id.cmp(&eb.id))
        });
        let survivor_id = group.entries[indices[0]].id.clone();
        let mut survivor_entry = group.entries[indices[0]].clone();
        for &dup_idx in &indices[1..] {
            let source_entry = group.entries[dup_idx].clone();
            let source_id = source_entry.id.clone();
            let expected = memcore::store::immutable_supersession::SupersessionExpectedState::active_unsuperseded(
                &source_entry,
                Some(&survivor_entry),
            );
            match crate::facade_memory_ops::consolidate_ops::merge_into_for_project_with_expected(
                server,
                project,
                &source_id,
                &survivor_id,
                Some(expected),
            ) {
                Ok(result) => {
                    if result
                        .response
                        .get("lifecycle_action")
                        .and_then(Value::as_str)
                        != Some("merge_into")
                    {
                        tracing::warn!(
                            "distill consolidate pre-pass: route-1 returned unexpected action for \
                             {source_id} -> {survivor_id}, continuing with committed survivor snapshot"
                        );
                    }
                    survivor_entry = result.committed_survivor;
                    group.entries[indices[0]] = survivor_entry.clone();
                    drop_indices.push(dup_idx);
                }
                Err(err) => {
                    // Best-effort: leave the row in the pool rather than
                    // silently dropping evidence the merge couldn't apply.
                    tracing::warn!(
                        "distill consolidate pre-pass: merge_into {source_id} -> {survivor_id} \
                         failed, keeping both rows in the pool: {err}"
                    );
                }
            }
        }
        group.entries[indices[0]] = survivor_entry;
    }

    if drop_indices.is_empty() {
        return 0;
    }
    drop_indices.sort_unstable();
    drop_indices.dedup();
    for &idx in drop_indices.iter().rev() {
        group.entries.remove(idx);
    }
    drop_indices.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::MemoryEntry;
    use serde_json::json;

    fn dup_entry(id: &str, timestamp: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/project/bounded/dup".to_string(),
            summary: "duplicate sync fact".to_string(),
            text: "The nightly sync job writes the same fact every run.".to_string(),
            importance: 0.6,
            timestamp: timestamp.to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: "bounded-scan".to_string(),
            keywords: vec!["sync".to_string()],
            persons: Vec::new(),
            entities: vec!["sync-job".to_string()],
            location: String::new(),
            source: "manual".to_string(),
            scope: "project".to_string(),
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

    /// #1043 D3 judgement test: 3 byte-identical rows in one candidate group
    /// collapse to 1 survivor, and the merge count is reported.
    #[test]
    fn three_byte_identical_rows_collapse_to_one_survivor() {
        let temp = tempfile::tempdir().expect("temp consolidate pre-pass db");
        let server = crate::MemoryServer::new(
            temp.path().join("global.db"),
            Some(temp.path().join("project.db")),
        )
        .expect("server");

        let mut oldest = dup_entry("dup-0", "2026-01-01T00:00:00Z");
        oldest.keywords.push("oldest-keyword".to_string());
        oldest.entities.push("oldest-entity".to_string());
        oldest.importance = 0.9;
        let mut middle = dup_entry("dup-1", "2026-01-01T00:00:01Z");
        middle.keywords.push("middle-keyword".to_string());
        middle.entities.push("middle-entity".to_string());
        let newest = dup_entry("dup-2", "2026-01-01T00:00:02Z");
        let entries = vec![oldest, middle, newest];
        server
            .with_project_store(|store| {
                for entry in &entries {
                    store.insert_if_absent(entry).map_err(|e| e.to_string())?;
                    let seeded = store
                        .get_with_options(&entry.id, true)
                        .map_err(|e| e.to_string())?
                        .expect("seeded duplicate exists");
                    assert!(
                        !seeded.archived,
                        "pre-pass fixture {} must start active",
                        entry.id
                    );
                    assert_eq!(
                        store
                            .supersession_target(&entry.id)
                            .map_err(|e| e.to_string())?,
                        Some(None),
                        "pre-pass fixture {} must start unsuperseded",
                        entry.id
                    );
                }
                Ok(())
            })
            .expect("seed duplicate rows");
        let selected_entries = server
            .with_project_store_read(|store| {
                entries
                    .iter()
                    .map(|entry| {
                        store
                            .get(&entry.id)
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| format!("seeded duplicate missing: {}", entry.id))
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .expect("read exact selected snapshots");
        let expected_importance = selected_entries
            .iter()
            .map(|entry| entry.importance)
            .max_by(f64::total_cmp)
            .expect("selected duplicates");

        let mut groups = vec![CandidateGroup {
            group_id: "bounded|dup".to_string(),
            path_prefix: "/project/bounded".to_string(),
            coherence_key: "dup".to_string(),
            entries: selected_entries,
        }];

        let consolidated = consolidate_duplicate_candidates(&server, None, &mut groups);
        assert_eq!(
            consolidated, 2,
            "3 byte-identical rows should merge away 2, leaving 1 survivor"
        );
        assert_eq!(
            groups[0].entries.len(),
            1,
            "selection pool must contain exactly 1 representative row after consolidation"
        );
        assert_eq!(
            groups[0].entries[0].id, "dup-2",
            "the newest row (by timestamp) should survive"
        );
        assert!(groups[0].entries[0]
            .keywords
            .contains(&"oldest-keyword".to_string()));
        assert!(groups[0].entries[0]
            .keywords
            .contains(&"middle-keyword".to_string()));
        assert!(groups[0].entries[0]
            .entities
            .contains(&"oldest-entity".to_string()));
        assert!(groups[0].entries[0]
            .entities
            .contains(&"middle-entity".to_string()));
        assert_eq!(groups[0].entries[0].importance, expected_importance);
        server
            .with_project_store_read(|store| {
                let survivor = store
                    .get("dup-2")
                    .map_err(|error| error.to_string())?
                    .expect("survivor remains materialized");
                assert!(survivor.keywords.contains(&"oldest-keyword".to_string()));
                assert!(survivor.keywords.contains(&"middle-keyword".to_string()));
                assert!(survivor.entities.contains(&"oldest-entity".to_string()));
                assert!(survivor.entities.contains(&"middle-entity".to_string()));
                assert_eq!(survivor.importance, expected_importance);
                Ok(())
            })
            .expect("all duplicate metadata folds reach the final survivor");
    }

    /// tachi#1768 blocker 3: after a successful route-1 merge, the pre-pass must
    /// chain from the committed DB survivor, not from a local reimplementation
    /// of tag canonicalization/revision behavior. The first duplicate below adds
    /// no logical tags to a survivor whose stored tag order is noncanonical, so
    /// route-1 correctly does not upsert the target. A local canonicalized
    /// survivor snapshot would then be stale against the DB row and would refuse
    /// the later metadata-adding duplicate.
    #[test]
    fn committed_survivor_snapshot_chains_across_noop_then_metadata_fold() {
        let temp = tempfile::tempdir().expect("temp consolidate pre-pass db");
        let server = crate::MemoryServer::new(
            temp.path().join("global.db"),
            Some(temp.path().join("project.db")),
        )
        .expect("server");

        let mut later_metadata = dup_entry("chain-later-metadata", "2026-01-01T00:00:00Z");
        later_metadata.keywords = vec!["later-keyword".to_string()];
        later_metadata.entities = vec!["later-entity".to_string()];
        later_metadata.importance = 0.95;

        let mut no_logical_tags = dup_entry("chain-noop-tags", "2026-01-01T00:00:01Z");
        no_logical_tags.keywords = vec!["alpha".to_string(), "zeta".to_string()];
        no_logical_tags.entities = vec!["entity-a".to_string(), "entity-b".to_string()];
        no_logical_tags.importance = 0.6;

        let mut survivor = dup_entry("chain-survivor", "2026-01-01T00:00:02Z");
        survivor.keywords = vec!["zeta".to_string(), "alpha".to_string()];
        survivor.entities = vec!["entity-b".to_string(), "entity-a".to_string()];
        survivor.importance = 0.6;

        let entries = vec![later_metadata, no_logical_tags, survivor];
        server
            .with_project_store(|store| {
                for entry in &entries {
                    store.insert_if_absent(entry).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .expect("seed duplicate chain rows");

        let selected_entries = server
            .with_project_store_read(|store| {
                entries
                    .iter()
                    .map(|entry| {
                        store
                            .get(&entry.id)
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| format!("seeded duplicate missing: {}", entry.id))
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .expect("read exact selected snapshots");
        let selected_survivor = selected_entries
            .iter()
            .find(|entry| entry.id == "chain-survivor")
            .expect("selected survivor snapshot");
        assert_eq!(
            selected_survivor.keywords,
            vec!["zeta".to_string(), "alpha".to_string()],
            "fixture must start with noncanonical survivor keyword order"
        );
        assert_eq!(
            selected_survivor.entities,
            vec!["entity-b".to_string(), "entity-a".to_string()],
            "fixture must start with noncanonical survivor entity order"
        );
        let selected_survivor_revision = selected_survivor.revision;
        let selected_later_importance = selected_entries
            .iter()
            .find(|entry| entry.id == "chain-later-metadata")
            .expect("selected later metadata snapshot")
            .importance;

        let mut groups = vec![CandidateGroup {
            group_id: "bounded|chain".to_string(),
            path_prefix: "/project/bounded".to_string(),
            coherence_key: "chain".to_string(),
            entries: selected_entries,
        }];

        let consolidated = consolidate_duplicate_candidates(&server, None, &mut groups);
        assert_eq!(
            consolidated, 2,
            "the no-op first fold must not stale the later metadata-adding fold"
        );
        assert_eq!(
            groups[0].entries.len(),
            1,
            "all three duplicates must converge to one survivor"
        );
        assert_eq!(groups[0].entries[0].id, "chain-survivor");

        server
            .with_project_store_read(|store| {
                let survivor_after = store
                    .get("chain-survivor")
                    .map_err(|error| error.to_string())?
                    .expect("survivor remains materialized");
                assert_eq!(
                    survivor_after.keywords,
                    vec![
                        "alpha".to_string(),
                        "later-keyword".to_string(),
                        "zeta".to_string()
                    ]
                );
                assert_eq!(
                    survivor_after.entities,
                    vec![
                        "entity-a".to_string(),
                        "entity-b".to_string(),
                        "later-entity".to_string()
                    ]
                );
                assert_eq!(survivor_after.importance, selected_later_importance);
                assert_eq!(
                    survivor_after.revision,
                    selected_survivor_revision + 1,
                    "only the later metadata-adding fold should rewrite the survivor"
                );
                assert_eq!(
                    serde_json::to_value(&groups[0].entries[0]).expect("group entry JSON"),
                    serde_json::to_value(&survivor_after).expect("survivor JSON"),
                    "CandidateGroup must carry the exact committed DB survivor row"
                );

                for duplicate_id in ["chain-noop-tags", "chain-later-metadata"] {
                    let duplicate = store
                        .get_with_options(duplicate_id, true)
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| format!("duplicate missing: {duplicate_id}"))?;
                    assert!(duplicate.archived, "{duplicate_id} must be archived");
                    assert_eq!(
                        store
                            .supersession_target(duplicate_id)
                            .map_err(|error| error.to_string())?,
                        Some(Some("chain-survivor".to_string())),
                        "{duplicate_id} must converge on the survivor"
                    );
                }
                Ok(())
            })
            .expect("verify committed survivor chain");
    }

    /// A group with no duplicates is left untouched — the pre-pass never
    /// removes distinct evidence.
    #[test]
    fn distinct_entries_are_never_merged() {
        let temp = tempfile::tempdir().expect("temp consolidate pre-pass db");
        let server = crate::MemoryServer::new(
            temp.path().join("global.db"),
            Some(temp.path().join("project.db")),
        )
        .expect("server");

        let mut e0 = dup_entry("distinct-0", "2026-01-01T00:00:00Z");
        e0.text = "Fact A: distinct content.".to_string();
        let mut e1 = dup_entry("distinct-1", "2026-01-01T00:00:01Z");
        e1.text = "Fact B: different distinct content.".to_string();
        let entries = vec![e0, e1];
        server
            .with_project_store(|store| {
                for entry in &entries {
                    store.upsert(entry).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .expect("seed distinct rows");

        let mut groups = vec![CandidateGroup {
            group_id: "bounded|distinct".to_string(),
            path_prefix: "/project/bounded".to_string(),
            coherence_key: "distinct".to_string(),
            entries,
        }];

        let consolidated = consolidate_duplicate_candidates(&server, None, &mut groups);
        assert_eq!(consolidated, 0);
        assert_eq!(groups[0].entries.len(), 2);
    }

    /// #1043 D3 terminal-review judgement test: the dedup key is byte-exact.
    /// `"x"` and `"x "` differ only in trailing whitespace and must NOT be
    /// treated as duplicates — trimming would silently merge two rows the
    /// spec promises are compared byte-for-byte.
    #[test]
    fn whitespace_only_difference_is_never_merged() {
        let temp = tempfile::tempdir().expect("temp consolidate pre-pass db");
        let server = crate::MemoryServer::new(
            temp.path().join("global.db"),
            Some(temp.path().join("project.db")),
        )
        .expect("server");

        let mut e0 = dup_entry("ws-0", "2026-01-01T00:00:00Z");
        e0.text = "x".to_string();
        let mut e1 = dup_entry("ws-1", "2026-01-01T00:00:01Z");
        e1.text = "x ".to_string();
        let entries = vec![e0, e1];
        server
            .with_project_store(|store| {
                for entry in &entries {
                    store.upsert(entry).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .expect("seed whitespace-variant rows");

        let mut groups = vec![CandidateGroup {
            group_id: "bounded|ws".to_string(),
            path_prefix: "/project/bounded".to_string(),
            coherence_key: "ws".to_string(),
            entries,
        }];

        let consolidated = consolidate_duplicate_candidates(&server, None, &mut groups);
        assert_eq!(
            consolidated, 0,
            "\"x\" and \"x \" must not be treated as byte-identical duplicates"
        );
        assert_eq!(groups[0].entries.len(), 2);
    }

    /// tachi#1671: the automated route-1 pre-pass binds the exact source and
    /// target rows it selected before calling the semantic supersession route.
    /// If a live row drifts after selection, the route refuses and keeps both
    /// rows in the distill pool instead of applying a stale merge.
    #[test]
    fn stale_selected_duplicate_snapshot_is_not_merged_or_dropped() {
        let temp = tempfile::tempdir().expect("temp consolidate pre-pass db");
        let server = crate::MemoryServer::new(
            temp.path().join("global.db"),
            Some(temp.path().join("project.db")),
        )
        .expect("server");

        let source = dup_entry("stale-source", "2026-01-01T00:00:00Z");
        let survivor = dup_entry("stale-survivor", "2026-01-01T00:00:01Z");
        server
            .with_project_store(|store| {
                store.upsert(&source).map_err(|e| e.to_string())?;
                store.upsert(&survivor).map_err(|e| e.to_string())?;
                Ok(())
            })
            .expect("seed selected duplicates");
        let entries = server
            .with_project_store_read(|store| {
                Ok(vec![
                    store
                        .get(&source.id)
                        .map_err(|error| error.to_string())?
                        .expect("selected source"),
                    store
                        .get(&survivor.id)
                        .map_err(|error| error.to_string())?
                        .expect("selected survivor"),
                ])
            })
            .expect("capture exact selected snapshots");
        server
            .with_project_store(|store| {
                let mut drifted = entries[0].clone();
                drifted.text = "The live row changed after pre-pass selection.".to_string();
                drifted.summary = "drifted duplicate source".to_string();
                store.upsert(&drifted).map_err(|e| e.to_string())?;
                Ok(())
            })
            .expect("drift selected duplicate after snapshot");

        let mut groups = vec![CandidateGroup {
            group_id: "bounded|stale".to_string(),
            path_prefix: "/project/bounded".to_string(),
            coherence_key: "stale".to_string(),
            entries,
        }];

        let consolidated = consolidate_duplicate_candidates(&server, None, &mut groups);
        assert_eq!(
            consolidated, 0,
            "stale expected source/target state must refuse the route-1 merge"
        );
        assert_eq!(
            groups[0].entries.len(),
            2,
            "a refused stale merge must not drop the source from the pool"
        );
        server
            .with_project_store_read(|store| {
                let source_after = store
                    .get_with_options("stale-source", true)
                    .map_err(|e| e.to_string())?
                    .expect("source remains");
                assert!(
                    !source_after.archived,
                    "stale expected-state refusal must leave source active"
                );
                assert_eq!(
                    store
                        .supersession_target("stale-source")
                        .map_err(|e| e.to_string())?,
                    Some(None),
                    "stale expected-state refusal must write no supersession edge"
                );
                Ok(())
            })
            .expect("verify stale refusal wrote nothing");
    }
}
