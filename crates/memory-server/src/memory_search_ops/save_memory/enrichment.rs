use crate::{DbScope, MemoryServer};
use memory_core::MemoryEntry;

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

    server.enqueue_enrichment(crate::enrichment::build_enrichment_item(
        entry,
        needs_embedding,
        needs_summary,
        target_db,
        named_project,
        None,
        None,
        None,
        enrichment_revision,
    ));
    true
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
    fn should_enqueue_enrichment_always_true() {
        let e = test_entry("enr-1", "test");
        assert!(should_enqueue_enrichment(&e));
    }

    #[test]
    fn enrichment_work_pending_when_metadata_missing() {
        let mut e = test_entry("enr-2", "test");
        e.summary = "ready".into();
        e.vector = Some(vec![0.1; 64]);
        e.keywords = vec![];
        e.entities = vec![];
        assert!(enrichment_work_pending(&e, false, false));

        e.keywords = vec!["tag".into()];
        e.entities = vec!["entity".into()];
        assert!(!enrichment_work_pending(&e, false, false));
    }

    #[test]
    fn should_enqueue_enrichment_high_importance_legacy() {
        let mut e = test_entry("enr-1", "test");
        e.importance = 0.5;
        assert!(should_enqueue_enrichment(&e));
    }
}
