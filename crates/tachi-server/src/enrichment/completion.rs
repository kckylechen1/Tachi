use super::{AdmittedEnrichmentIntentV1, MemoryEntry};

pub(super) fn admitted_enrichment_is_terminal(
    entry: &MemoryEntry,
    intent: &AdmittedEnrichmentIntentV1,
) -> bool {
    let Some(enrichment) = entry.metadata.get("enrichment") else {
        return false;
    };
    if enrichment.get("failed_stage").is_some() {
        return false;
    }
    let status = enrichment
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    (!intent.needs_embedding || status.contains("embedded"))
        && (!intent.needs_summary || status.contains("summarized"))
        && (!intent.needs_metadata
            || status.contains("metadata")
            || memcore::store::enrichment::metadata_extraction_attempted(&entry.metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::MemoryStore;

    fn intent(revision: i64) -> AdmittedEnrichmentIntentV1 {
        AdmittedEnrichmentIntentV1 {
            version: 1,
            job_id: "job".into(),
            idempotency_key: "key".into(),
            entry_id: "row".into(),
            revision,
            target_db: "global".into(),
            named_project: None,
            needs_embedding: true,
            needs_summary: true,
            needs_metadata: true,
            owner_token: None,
        }
    }

    fn complete_stage(store: &mut MemoryStore, stage: &str, revision: i64) {
        let vector = [0.5_f32; 1024];
        let entities = ["entity".into()];
        assert!(store
            .update_enrichment_fields(
                "row",
                (stage == "summary").then_some("generated summary"),
                (stage == "embedding").then_some(vector.as_slice()),
                None,
                (stage == "metadata").then_some(entities.as_slice()),
                revision
            )
            .unwrap());
    }

    #[test]
    fn real_terminal_consumer_observes_accumulated_fields_failure_and_body_change() {
        for stages in [
            ["embedding", "summary", "metadata"],
            ["metadata", "embedding", "summary"],
        ] {
            let mut store = MemoryStore::open_in_memory().unwrap();
            let entry: MemoryEntry = serde_json::from_value(serde_json::json!({
                "id": "row", "path": "/facts", "text": "original body",
                "timestamp": "2026-10-01T00:00:00Z"
            }))
            .unwrap();
            store.upsert(&entry).unwrap();
            let admitted = intent(1);
            for (index, stage) in stages.into_iter().enumerate() {
                complete_stage(&mut store, stage, 1);
                let saved = store.get("row").unwrap().unwrap();
                assert_eq!(
                    admitted_enrichment_is_terminal(&saved, &admitted),
                    index == 2
                );
            }
            assert!(store
                .record_enrichment_failure_if_revision("row", "embedding", "401 unauthorized", 1)
                .unwrap());
            complete_stage(&mut store, "metadata", 1);
            assert!(!admitted_enrichment_is_terminal(
                &store.get("row").unwrap().unwrap(),
                &admitted
            ));
            complete_stage(&mut store, "embedding", 1);
            assert!(admitted_enrichment_is_terminal(
                &store.get("row").unwrap().unwrap(),
                &admitted
            ));

            let mut changed = store.get("row").unwrap().unwrap();
            changed.text = "replacement body".into();
            store.upsert(&changed).unwrap();
            let changed = store.get("row").unwrap().unwrap();
            let next = intent(changed.revision);
            assert!(!admitted_enrichment_is_terminal(&changed, &next));
            complete_stage(&mut store, "metadata", changed.revision);
            assert!(!admitted_enrichment_is_terminal(
                &store.get("row").unwrap().unwrap(),
                &next
            ));
        }
    }
}
