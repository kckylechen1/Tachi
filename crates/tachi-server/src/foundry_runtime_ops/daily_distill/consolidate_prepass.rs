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
//! [`crate::facade_memory_ops::consolidate_ops::merge_into_for_project`],
//! called directly instead of through the proposal-store round trip. That
//! entry point hard-codes `action="merge_into"` — it cannot reach any other
//! lifecycle action, so this pre-pass has no way to bypass human review for
//! `supersede`/`archive`/`promote_distilled`.

use std::collections::HashMap;

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
        for &dup_idx in &indices[1..] {
            let source_id = group.entries[dup_idx].id.clone();
            match crate::facade_memory_ops::consolidate_ops::merge_into_for_project(
                server,
                project,
                &source_id,
                &survivor_id,
            ) {
                Ok(_) => drop_indices.push(dup_idx),
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

        let entries = vec![
            dup_entry("dup-0", "2026-01-01T00:00:00Z"),
            dup_entry("dup-1", "2026-01-01T00:00:01Z"),
            dup_entry("dup-2", "2026-01-01T00:00:02Z"),
        ];
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

        let mut groups = vec![CandidateGroup {
            group_id: "bounded|dup".to_string(),
            path_prefix: "/project/bounded".to_string(),
            coherence_key: "dup".to_string(),
            entries,
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
}
