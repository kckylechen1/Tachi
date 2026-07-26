use crate::{DbScope, MemoryServer};
use memcore::MemoryEntry;

fn should_enqueue_enrichment(_entry: &MemoryEntry) -> bool {
    true
}

fn enrichment_work_pending(
    entry: &MemoryEntry,
    needs_embedding: bool,
    needs_summary: bool,
) -> bool {
    needs_embedding
        || needs_summary
        || crate::enrichment::needs_metadata_enrichment(&entry.keywords, &entry.entities)
        || crate::enrichment::needs_keyword_enrichment(entry)
}

pub(in crate::memory_search_ops::save_memory) fn enqueue_save_enrichment(
    server: &MemoryServer,
    entry: &MemoryEntry,
    needs_embedding: bool,
    needs_summary: bool,
    target_db: DbScope,
    named_project: Option<String>,
    enrichment_revision: i64,
) -> bool {
    if !enrichment_work_pending(entry, needs_embedding, needs_summary)
        || !should_enqueue_enrichment(entry)
    {
        return false;
    }

    let item = crate::enrichment::build_enrichment_item(
        entry,
        needs_embedding,
        needs_summary,
        target_db,
        named_project.clone(),
        None,
        None,
        None,
        enrichment_revision,
    );
    let needs_keyword = item.needs_keyword_enrichment;
    // Write pending BEFORE enqueue so a fast worker's terminal status cannot be
    // overwritten by a late pending stamp (#943). The pending write is also
    // conditional (only None/pending → pending) as belt-and-suspenders.
    if needs_keyword {
        mark_keyword_enrichment_pending(server, &entry.id, target_db, named_project.as_deref());
    }
    server.enqueue_enrichment(item)
}

fn mark_keyword_enrichment_pending(
    server: &MemoryServer,
    id: &str,
    target_db: DbScope,
    named_project: Option<&str>,
) {
    let action = |store: &mut memcore::MemoryStore| {
        store
            .set_keyword_enrichment_pending_if_unset(id)
            .map_err(|e| format!("set keywords_status=pending: {e}"))
    };
    let res = match named_project {
        Some(name) => server.with_named_project_store(name, action),
        None => server.with_store_for_scope(target_db, action),
    };
    if let Err(err) = res {
        tracing::warn!("[enrichment] failed to mark keywords_status=pending for {id}: {err}");
    }
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
    fn should_enqueue_enrichment_always_true() {
        let e = test_entry("enr-1", "test");
        assert!(should_enqueue_enrichment(&e));
    }

    #[test]
    fn enrichment_work_pending_when_metadata_missing() {
        // Hold the global env lock and force flag-off so a parallel flag-on
        // test cannot make keyword enrichment look pending.
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _flag =
            crate::test_support::EnvRestore::remove(crate::enrichment::WRITE_ENRICH_KEYWORDS_ENV);

        let mut e = test_entry("enr-2", "test");
        e.summary = "ready".into();
        e.vector = Some(vec![0.1; 64]);
        e.keywords = vec![];
        e.entities = vec![];
        assert!(enrichment_work_pending(&e, false, false));

        e.keywords = vec!["tag".into()];
        e.entities = vec!["entity".into()];
        // Flag off → no keyword enrichment work once metadata is present.
        assert!(!enrichment_work_pending(&e, false, false));
    }

    #[test]
    fn enrichment_work_pending_when_keyword_flag_on() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _flag = crate::test_support::EnvRestore::set(
            crate::enrichment::WRITE_ENRICH_KEYWORDS_ENV,
            "true",
        );
        let mut e = test_entry("enr-kw", "bilingual keyword probe");
        e.summary = "ready".into();
        e.vector = Some(vec![0.1; 64]);
        e.keywords = vec!["tag".into()];
        e.entities = vec!["entity".into()];
        assert!(
            enrichment_work_pending(&e, false, false),
            "flag-on must enqueue write-side keyword enrichment even when metadata present"
        );

        e.metadata = json!({"enrichment": {"keywords_status": "enriched"}});
        assert!(
            !enrichment_work_pending(&e, false, false),
            "already-enriched keyword status must not re-enqueue"
        );
    }

    #[test]
    fn should_enqueue_enrichment_high_importance_legacy() {
        let mut e = test_entry("enr-1", "test");
        e.importance = 0.5;
        assert!(should_enqueue_enrichment(&e));
    }
}
