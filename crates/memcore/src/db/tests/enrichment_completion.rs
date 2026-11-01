use super::{json, make_entry};
use crate::MemoryStore;

fn apply_stage(store: &mut MemoryStore, stage: &str, revision: i64) {
    let vector = [0.25_f32; 1024];
    let keywords = ["generated-keyword".into()];
    let entities = ["generated-entity".into()];
    assert!(store
        .update_enrichment_fields(
            "row",
            (stage == "summary").then_some("generated summary"),
            (stage == "embedding").then_some(vector.as_slice()),
            (stage == "metadata").then_some(keywords.as_slice()),
            (stage == "metadata").then_some(entities.as_slice()),
            revision,
        )
        .unwrap());
}

fn metadata(store: &MemoryStore) -> serde_json::Value {
    store.get("row").unwrap().unwrap().metadata
}

#[test]
fn failure_without_completed_stages_preserves_nonobject_metadata() {
    for stage in ["embedding", "keywords"] {
        for error in ["provider unavailable", "401 unauthorized"] {
            let mut store = MemoryStore::open_in_memory().unwrap();
            for (index, original) in [
                json!(["keep", {"nested": null}]),
                json!("opaque metadata"),
                json!(7),
                json!(true),
                serde_json::Value::Null,
            ]
            .into_iter()
            .enumerate()
            {
                let id = format!("metadata-{index}");
                let mut entry = make_entry(&id, "preserve legacy metadata root");
                entry.metadata = original.clone();
                store.upsert(&entry).unwrap();
                assert!(store
                    .record_enrichment_failure_if_revision(&id, stage, error, 1)
                    .unwrap());
                assert_eq!(
                    store.get(&id).unwrap().unwrap().metadata,
                    original,
                    "failure branch {stage}/{error} must preserve the metadata root"
                );
            }
        }
    }
}

#[test]
fn partial_enrichment_accumulates_in_both_orders_and_on_repeated_stages() {
    for stages in [
        ["embedding", "summary", "metadata"],
        ["metadata", "summary", "embedding"],
    ] {
        let mut store = MemoryStore::open_in_memory().unwrap();
        let mut entry = make_entry("row", "original body");
        entry.metadata = json!({"owner": "kept", "enrichment": {
            "invocations": {"summary": {"receipt": "kept"}},
            "metadata_status": "attempted"
        }});
        store.upsert(&entry).unwrap();
        for stage in stages {
            apply_stage(&mut store, stage, 1);
        }
        for _ in 0..2 {
            apply_stage(&mut store, "metadata", 1);
            let saved = store.get("row").unwrap().unwrap();
            assert_eq!(
                saved.metadata["enrichment"]["status"],
                "embedded+summarized+metadata"
            );
            assert!(saved.metadata["enrichment"]
                .get("partial_success_status")
                .is_none());
            assert_eq!(saved.metadata["owner"], "kept");
            assert_eq!(
                saved.metadata["enrichment"]["invocations"],
                entry.metadata["enrichment"]["invocations"]
            );
            assert_eq!(saved.metadata["enrichment"]["metadata_status"], "attempted");
            assert_eq!(saved.revision, 1);
            assert_eq!(saved.summary, "generated summary");
            assert_eq!(saved.entities, ["generated-entity"]);
            assert_eq!(saved.vector.as_deref(), Some([0.25_f32; 1024].as_slice()));
        }
    }
}

#[test]
fn failed_enrichment_retains_prior_success_and_accumulates_until_recovery() {
    for failed_stage in ["embedding", "keywords"] {
        for error in ["provider unavailable", "401 unauthorized"] {
            let mut store = MemoryStore::open_in_memory().unwrap();
            store.upsert(&make_entry("row", "original body")).unwrap();
            apply_stage(&mut store, "summary", 1);
            assert!(store
                .record_enrichment_failure_if_revision("row", failed_stage, error, 1)
                .unwrap());
            let failed = metadata(&store);
            assert_eq!(failed["enrichment"]["status"], "failed");
            assert_eq!(failed["enrichment"]["partial_success_status"], "summarized");
            let partial_stage = if failed_stage == "embedding" {
                "metadata"
            } else {
                "embedding"
            };
            apply_stage(&mut store, partial_stage, 1);
            let partial = metadata(&store);
            assert_eq!(partial["enrichment"]["status"], "failed");
            assert_eq!(
                partial["enrichment"]["partial_success_status"],
                if failed_stage == "embedding" {
                    "summarized+metadata"
                } else {
                    "embedded+summarized"
                }
            );
            for key in [
                "failed_stage",
                "last_error",
                "last_failure_at",
                "retry",
                "keywords_status",
            ] {
                assert_eq!(
                    partial["enrichment"].get(key),
                    failed["enrichment"].get(key),
                    "preserve {key}"
                );
            }
            let repair_stage = if failed_stage == "embedding" {
                "embedding"
            } else {
                "metadata"
            };
            apply_stage(&mut store, repair_stage, 1);
            let recovered = metadata(&store);
            assert_eq!(
                recovered["enrichment"]["status"],
                "embedded+summarized+metadata"
            );
            for key in [
                "failed_stage",
                "last_failure_at",
                "retry",
                "partial_success_status",
            ] {
                assert!(recovered["enrichment"].get(key).is_none(), "clear {key}");
            }
        }
    }
}

#[test]
fn rewritten_body_invalidates_completion_but_keeps_receipts_attempts_and_cas() {
    for write_mode in ["upsert", "revision", "validated"] {
        for failed in [false, true] {
            let mut store = MemoryStore::open_in_memory().unwrap();
            let mut entry = make_entry("row", "original body");
            entry.metadata = json!({"owner": "kept", "enrichment": {
                "status": if failed { "failed" } else { "embedded+summarized+metadata" },
                "partial_success_status": "embedded+summarized",
                "invocations": {"summary": {"receipt": "historical"}},
                "metadata_status": "attempted"
            }});
            if failed {
                entry.metadata["enrichment"]["failed_stage"] = json!("embedding");
                entry.metadata["enrichment"]["last_error"] = json!("401 unauthorized");
                entry.metadata["enrichment"]["retry"] = json!({"kind": "auth", "attempts": 1});
            }
            store.upsert(&entry).unwrap();
            // Compare the body actually stored after scrubbing, not raw input.
            entry.text = "<think>discarded</think>original body".into();
            store.upsert(&entry).unwrap();
            let unchanged = store.get("row").unwrap().unwrap();
            assert_eq!(unchanged.text, "original body");
            assert_eq!(
                unchanged.metadata["enrichment"],
                entry.metadata["enrichment"]
            );

            let mut returned_metadata = None;
            if write_mode == "upsert" {
                let mut changed = unchanged.clone();
                changed.text = "replacement body".into();
                store.upsert(&changed).unwrap();
            } else if write_mode == "validated" {
                let mut changed = unchanged.clone();
                changed.text = "replacement body".into();
                returned_metadata = Some(
                    store
                        .upsert_with_validated_reference_mutations(
                            &changed,
                            None,
                            &serde_json::Map::new(),
                            &[],
                        )
                        .unwrap()
                        .1,
                );
            } else {
                assert!(store
                    .update_with_revision(
                        "row",
                        "replacement body",
                        &unchanged.summary,
                        &unchanged.source,
                        &unchanged.metadata,
                        None,
                        unchanged.revision
                    )
                    .unwrap());
            }
            let changed = store.get("row").unwrap().unwrap();
            if let Some(returned) = returned_metadata {
                assert_eq!(
                    returned, changed.metadata,
                    "save payload must match committed metadata"
                );
            }
            assert!(changed.metadata["enrichment"]
                .get("partial_success_status")
                .is_none());
            assert_eq!(
                changed.metadata["enrichment"]["invocations"],
                entry.metadata["enrichment"]["invocations"]
            );
            assert_eq!(
                changed.metadata["enrichment"]["metadata_status"],
                "attempted"
            );
            assert_eq!(changed.metadata["owner"], "kept");
            if failed {
                for key in ["status", "failed_stage", "last_error", "retry"] {
                    assert_eq!(
                        changed.metadata["enrichment"].get(key),
                        entry.metadata["enrichment"].get(key)
                    );
                }
            } else {
                assert!(changed.metadata["enrichment"].get("status").is_none());
            }
            apply_stage(&mut store, "metadata", changed.revision);
            let current = metadata(&store);
            assert_eq!(
                current["enrichment"][if failed {
                    "partial_success_status"
                } else {
                    "status"
                }],
                "metadata"
            );

            let snapshot = || {
                store
                    .connection()
                    .query_row(
                        "SELECT text, summary, metadata, updated_at FROM memories WHERE id='row'",
                        [],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                            ))
                        },
                    )
                    .unwrap()
            };
            let before_stale = snapshot();
            assert!(!store
                .record_enrichment_failure_if_revision("row", "summary", "late", unchanged.revision)
                .unwrap());
            assert_eq!(snapshot(), before_stale);
            assert!(!store
                .update_enrichment_fields(
                    "row",
                    Some("stale summary"),
                    None,
                    None,
                    None,
                    unchanged.revision
                )
                .unwrap());
            let after_stale: (String, String, String, String) = store
                .connection()
                .query_row(
                    "SELECT text, summary, metadata, updated_at FROM memories WHERE id='row'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(after_stale, before_stale);
        }
    }
}
