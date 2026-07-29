use super::*;

const STANDALONE_FACT_IDENTITY_CONTRACT: &str = "standalone-extracted-fact-v1";

#[derive(serde::Serialize)]
struct StandaloneFactIdentityV1<'a> {
    contract: &'static str,
    target: &'a str,
    source: &'a str,
    text: &'a str,
    topic: &'a str,
    importance_bits: &'a str,
    scope: &'a str,
    keywords: &'a [String],
    entities: &'a [String],
}

#[derive(Debug)]
struct ExistingFactValidationError {
    kind: &'static str,
    message: String,
}

fn normalized_fact_source_identity(source: &str) -> String {
    let source = source.trim();
    if source.is_empty() {
        "extraction".to_string()
    } else {
        source.to_string()
    }
}

fn normalized_identity_values(values: &[String]) -> Vec<String> {
    let mut normalized = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalized_fact_text(text: &str) -> String {
    memcore::noise::scrub_think_tags(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string()
}

fn canonicalize_standalone_fact_entry(entry: &mut MemoryEntry) {
    entry.text = normalized_fact_text(&entry.text);
    entry.topic = entry.topic.trim().to_string();
    entry.importance = memcore::types::normalize_importance(
        entry.importance,
        memcore::types::MemoryCategory::Fact.as_str(),
        false,
    );
    if entry.importance == 0.0 {
        entry.importance = 0.0;
    }
    entry.scope = memcore::types::MemoryScope::normalize(&entry.scope).to_string();
    entry.keywords = normalized_identity_values(&entry.keywords);
    entry.entities = normalized_identity_values(&entry.entities);
    entry.summary = entry.text.chars().take(100).collect();
    entry.path = format!("/{}/{}", entry.scope, entry.topic.replace(' ', "_"));
}

fn standalone_fact_identity_bytes(
    entry: &MemoryEntry,
    source_identity: &str,
    target_identity: &str,
) -> Vec<u8> {
    let importance_bits = format!("{:016x}", entry.importance.to_bits());
    serde_json::to_vec(&StandaloneFactIdentityV1 {
        contract: STANDALONE_FACT_IDENTITY_CONTRACT,
        target: target_identity,
        source: source_identity,
        text: &entry.text,
        topic: &entry.topic,
        importance_bits: &importance_bits,
        scope: &entry.scope,
        keywords: &entry.keywords,
        entities: &entry.entities,
    })
    .expect("serializing the standalone fact v1 string/list payload to Vec cannot fail")
}

fn stable_standalone_fact_id(
    entry: &MemoryEntry,
    source_identity: &str,
    target_identity: &str,
) -> String {
    format!(
        "extract-fact:{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            &standalone_fact_identity_bytes(entry, source_identity, target_identity),
        )
    )
}

fn stamp_standalone_fact_identity(
    entry: &mut MemoryEntry,
    source_identity: &str,
    target_identity: &str,
) -> Result<(), String> {
    let id = stable_standalone_fact_id(entry, source_identity, target_identity);
    let metadata = entry
        .metadata
        .as_object_mut()
        .ok_or_else(|| "fact provenance metadata must be a JSON object".to_string())?;
    metadata.insert(
        "fact_identity_contract".to_string(),
        serde_json::json!(STANDALONE_FACT_IDENTITY_CONTRACT),
    );
    metadata.insert("fact_identity".to_string(), serde_json::json!(id));
    metadata.insert(
        "fact_source_identity".to_string(),
        serde_json::json!(source_identity),
    );
    metadata.insert(
        "fact_target_identity".to_string(),
        serde_json::json!(target_identity),
    );
    entry.id = id;
    Ok(())
}

fn validate_existing_metadata_string(
    existing: &MemoryEntry,
    key: &str,
    expected: &str,
) -> Result<(), ExistingFactValidationError> {
    let Some(actual) = existing
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
    else {
        return Err(ExistingFactValidationError {
            kind: "existing_invalid",
            message: format!("{key} is missing or is not a string"),
        });
    };
    if actual != expected {
        return Err(ExistingFactValidationError {
            kind: "identity_conflict",
            message: format!("{key} does not match the canonical identity"),
        });
    }
    Ok(())
}

fn validate_existing_standalone_fact(
    existing: &MemoryEntry,
    expected_id: &str,
    expected_source_identity: &str,
    expected_target_identity: &str,
) -> Result<(), ExistingFactValidationError> {
    if existing.id != expected_id || existing.category != "fact" || existing.source != "extraction"
    {
        return Err(ExistingFactValidationError {
            kind: "identity_conflict",
            message: "row id/category/source does not match a standalone extracted fact"
                .to_string(),
        });
    }
    validate_existing_metadata_string(
        existing,
        "fact_identity_contract",
        STANDALONE_FACT_IDENTITY_CONTRACT,
    )?;
    validate_existing_metadata_string(existing, "fact_identity", expected_id)?;
    validate_existing_metadata_string(existing, "fact_source_identity", expected_source_identity)?;
    validate_existing_metadata_string(existing, "fact_target_identity", expected_target_identity)?;

    let mut canonical = existing.clone();
    canonicalize_standalone_fact_entry(&mut canonical);
    let recomputed = stable_standalone_fact_id(
        &canonical,
        expected_source_identity,
        expected_target_identity,
    );
    if recomputed != expected_id {
        return Err(ExistingFactValidationError {
            kind: "identity_conflict",
            message: "persisted fact fields do not recompute to the occupied id".to_string(),
        });
    }
    Ok(())
}

fn successful_fact_summary(entry: &MemoryEntry, disposition: &str) -> serde_json::Value {
    serde_json::json!({
        "id": entry.id,
        "path": entry.path,
        "topic": entry.topic,
        "summary": entry.summary,
        "importance": entry.importance,
        "disposition": disposition,
    })
}

fn fact_write_error(
    entry: &MemoryEntry,
    kind: &str,
    error: impl std::fmt::Display,
) -> serde_json::Value {
    serde_json::json!({
        "id": entry.id,
        "path": entry.path,
        "kind": kind,
        "error": error.to_string(),
    })
}

/// Real entry point for LLM atomization. Both the standalone `extract_facts`
/// tool (`tools/pipeline_facade.rs`) and the `tachi_memory(action='extract_facts')`
/// facade action call this same function with the same `ExtractFactsParams`
/// shape (#1043 D1) — there is exactly one atomization pipeline, not a
/// facade-specific single-row imposter.
///
/// On LLM failure the response is `atomized: false` with a `reason` — the
/// degrade behavior is unchanged (no facts saved) but is no longer silent:
/// callers can distinguish "atomizer ran, found nothing" from "atomizer
/// didn't run at all".
pub(crate) async fn handle_extract_facts(
    server: &MemoryServer,
    params: ExtractFactsParams,
) -> Result<String, String> {
    let source = params.source.clone();
    let project = params.project.clone();

    let facts = match server.llm.extract_facts(&params.text).await {
        Ok(facts) => facts,
        Err(err) => return llm_extraction_failed_response(&source, &err),
    };

    save_atomized_facts(server, facts, source, project)
}

/// Degrade-path response when the LLM atomizer itself is unavailable/errors.
/// Behavior is unchanged from before #1043 (no facts saved), but the
/// response is no longer silent about it — `atomized: false` lets a caller
/// tell "the atomizer ran and found nothing" apart from "the atomizer never
/// ran". Split out so this labeling is unit-testable without a live LLM call.
fn llm_extraction_failed_response(source: &str, err: &str) -> Result<String, String> {
    serialize_json(serde_json::json!({
        "status": "failed",
        "atomized": false,
        "reason": "llm_extraction_failed",
        "error": err,
        "source": source,
        "facts_extracted": 0,
        "facts_saved": 0,
        "facts_inserted": 0,
        "facts_existing": 0,
        "facts_failed": 0,
        "facts_dropped": 0
    }))
}

/// Shared save+response-shaping base for a batch of already-atomized facts.
/// Split out of `handle_extract_facts` so the multi-row save behavior is
/// directly unit-testable with a hand-built facts array — no live LLM call
/// required to exercise "N facts in -> N rows out".
pub(crate) fn save_atomized_facts(
    server: &MemoryServer,
    facts: Vec<serde_json::Value>,
    source: String,
    project: Option<String>,
) -> Result<String, String> {
    let (target_db, warning) = if project.is_some() {
        (DbScope::Project, None)
    } else {
        server.resolve_write_scope("project")
    };
    let source_identity = normalized_fact_source_identity(&source);
    let validated_project = project
        .as_deref()
        .map(MemoryServer::validate_named_project)
        .transpose()?;
    let target_identity = match validated_project.as_deref() {
        Some(project_name) => format!("named-project:{project_name}"),
        None => format!("scope:{}", target_db.as_str()),
    };

    if facts.is_empty() {
        return serialize_json(serde_json::json!({
            "status": "completed",
            "atomized": true,
            "source": source,
            "facts_extracted": 0,
            "facts_saved": 0,
            "facts_inserted": 0,
            "facts_existing": 0,
            "facts_failed": 0,
            "facts_dropped": 0
        }));
    }

    let count = facts.len();
    let mut successful_facts = Vec::new();
    let mut dropped: Vec<serde_json::Value> = Vec::new();
    let mut write_errors: Vec<serde_json::Value> = Vec::new();
    let save_facts = |store: &mut memcore::MemoryStore| {
        let mut inserted = 0;
        let mut existing = 0;
        let mut failed = 0;
        for fact in &facts {
            let metadata = crate::provenance::inject_provenance(
                server,
                serde_json::json!({"source": source.clone()}),
                "extract_facts",
                "fact_extraction",
                Some("project"),
                target_db,
                serde_json::json!({
                    "extract_source": source.clone(),
                }),
            );
            // Apply capture_gate filters (min-length and noise assessment) and
            // surface the rejection reason so facts_extracted vs facts_saved
            // gaps are explainable instead of silently vanishing.
            let mut entry = match fact_to_entry_candidate_with_reason(fact, "extraction", metadata)
            {
                Ok(entry) => entry,
                Err(reason) => {
                    dropped.push(serde_json::json!({
                        "reason": reason,
                        "text": fact.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                    }));
                    continue;
                }
            };
            canonicalize_standalone_fact_entry(&mut entry);
            if let Err(error) =
                stamp_standalone_fact_identity(&mut entry, &source_identity, &target_identity)
            {
                failed += 1;
                write_errors.push(fact_write_error(&entry, "write_failed", error));
                continue;
            }
            if is_lazy_source(&entry.source) && entry.importance < 0.5 {
                entry.retention_policy = Some("ephemeral".to_string());
            }
            match store.insert_if_absent(&entry) {
                Ok(memcore::InsertMemoryResult::Inserted) => {
                    inserted += 1;
                    successful_facts.push(successful_fact_summary(&entry, "inserted"));
                }
                Ok(memcore::InsertMemoryResult::Existing) => {
                    match store.get_with_options(&entry.id, true) {
                        Ok(Some(winner)) => match validate_existing_standalone_fact(
                            &winner,
                            &entry.id,
                            &source_identity,
                            &target_identity,
                        ) {
                            Ok(()) => {
                                existing += 1;
                                successful_facts.push(successful_fact_summary(&winner, "existing"));
                            }
                            Err(error) => {
                                failed += 1;
                                write_errors.push(fact_write_error(
                                    &entry,
                                    error.kind,
                                    error.message,
                                ));
                            }
                        },
                        Ok(None) => {
                            failed += 1;
                            write_errors.push(fact_write_error(
                                &entry,
                                "existing_read_failed",
                                "canonical occupant disappeared after insert_if_absent",
                            ));
                        }
                        Err(error) => {
                            failed += 1;
                            write_errors.push(fact_write_error(
                                &entry,
                                "existing_read_failed",
                                error,
                            ));
                        }
                    }
                }
                Err(error) => {
                    failed += 1;
                    write_errors.push(fact_write_error(&entry, "write_failed", error));
                }
            }
        }
        Ok((inserted, existing, failed))
    };
    let (inserted, existing, failed) = if let Some(ref project_name) = project {
        server.with_named_project_store(project_name, save_facts)
    } else {
        server.with_store_for_scope(target_db, save_facts)
    }
    .map_err(|e| format!("DB write failed: {e}"))?;
    debug_assert_eq!(count, inserted + existing + failed + dropped.len());
    debug_assert_eq!(failed, write_errors.len());

    let status = if failed == 0 { "completed" } else { "partial" };
    serialize_json(serde_json::json!({
        "status": status,
        "atomized": true,
        "source": source,
        "db": if target_db == DbScope::Project { "project" } else { "global" },
        "project": project,
        "warning": warning,
        "facts_extracted": count,
        "facts_saved": inserted,
        "facts_inserted": inserted,
        "facts_existing": existing,
        "facts_failed": failed,
        "facts_dropped": dropped.len(),
        "write_errors": write_errors,
        "facts": successful_facts,
        "dropped": dropped,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_server;

    fn fact(text: &str, topic: &str) -> serde_json::Value {
        serde_json::json!({
            "text": text,
            "topic": topic,
            "importance": 0.6,
        })
    }

    fn canonical_identity_for(
        fact: &serde_json::Value,
        source_identity: &str,
        target_identity: &str,
    ) -> (String, Vec<u8>) {
        let mut entry =
            fact_to_entry_candidate_with_reason(fact, "extraction", serde_json::json!({}))
                .expect("accepted fact");
        canonicalize_standalone_fact_entry(&mut entry);
        (
            stable_standalone_fact_id(&entry, source_identity, target_identity),
            standalone_fact_identity_bytes(&entry, source_identity, target_identity),
        )
    }

    #[test]
    fn standalone_fact_v1_has_frozen_name_bytes_and_uuid() {
        let input = fact("Stable fact text for identity.", "ops");
        let (id, bytes) = canonical_identity_for(&input, "source-a", "scope:global");

        assert_eq!(
            String::from_utf8(bytes).expect("identity bytes are UTF-8 JSON"),
            r#"{"contract":"standalone-extracted-fact-v1","target":"scope:global","source":"source-a","text":"Stable fact text for identity.","topic":"ops","importance_bits":"3fe3333333333333","scope":"general","keywords":[],"entities":[]}"#
        );
        assert_eq!(id, "extract-fact:ace9a338-f207-58a3-b88d-8c178ae48648");

        for zero in [0.0, -0.0] {
            let zero_fact = serde_json::json!({
                "text": "Stable fact text for identity.",
                "topic": "ops",
                "importance": zero,
            });
            let (zero_id, zero_bytes) =
                canonical_identity_for(&zero_fact, "source-a", "scope:global");
            assert_eq!(
                String::from_utf8(zero_bytes).expect("zero identity bytes are UTF-8 JSON"),
                r#"{"contract":"standalone-extracted-fact-v1","target":"scope:global","source":"source-a","text":"Stable fact text for identity.","topic":"ops","importance_bits":"0000000000000000","scope":"general","keywords":[],"entities":[]}"#
            );
            assert_eq!(zero_id, "extract-fact:2c9cb338-08c1-5857-9099-8644cecc9697");
        }

        for importance in [0.85, 1.0] {
            let capped_fact = serde_json::json!({
                "text": "Stable fact text for identity.",
                "topic": "ops",
                "importance": importance,
            });
            let (capped_id, capped_bytes) =
                canonical_identity_for(&capped_fact, "source-a", "scope:global");
            assert_eq!(
                String::from_utf8(capped_bytes).expect("capped identity bytes are UTF-8 JSON"),
                r#"{"contract":"standalone-extracted-fact-v1","target":"scope:global","source":"source-a","text":"Stable fact text for identity.","topic":"ops","importance_bits":"3feb333333333333","scope":"general","keywords":[],"entities":[]}"#
            );
            assert_eq!(
                capped_id,
                "extract-fact:b4f643d7-1445-5967-b044-50503f1a5e88"
            );
        }
    }

    #[test]
    fn standalone_fact_v1_normalizes_persisted_equivalence_classes() {
        let first = serde_json::json!({
            "text": "  Durable line one.\r\nDurable line two remains canonical.  ",
            "topic": " ops ",
            "importance": 1.0,
            "scope": "GENERAL",
            "keywords": [" beta ", "alpha", "alpha"],
            "entities": ["Service B", " Service A "],
            "persons": ["Person Z", "Service A"],
        });
        let reordered = serde_json::json!({
            "persons": ["Service A", "Person Z"],
            "entities": ["Service A", "Service B"],
            "keywords": ["alpha", "beta"],
            "scope": "general",
            "importance": 0.9,
            "topic": "ops",
            "text": "Durable line one.\nDurable line two remains canonical.",
        });
        let (first_id, _) = canonical_identity_for(&first, "source-a", "scope:global");
        let (reordered_id, _) = canonical_identity_for(&reordered, "source-a", "scope:global");
        assert_eq!(
            first_id, reordered_id,
            "array order, CRLF, outer whitespace, and importance above the persisted cap must normalize"
        );

        let negative_zero = serde_json::json!({
            "text": "Negative zero identity remains a stable accepted fact.",
            "topic": "math",
            "importance": -0.0,
        });
        let positive_zero = serde_json::json!({
            "text": "Negative zero identity remains a stable accepted fact.",
            "topic": "math",
            "importance": 0.0,
        });
        assert_eq!(
            canonical_identity_for(&negative_zero, "source-a", "scope:global").0,
            canonical_identity_for(&positive_zero, "source-a", "scope:global").0,
            "negative zero must normalize to positive zero before to_bits"
        );
    }

    #[test]
    fn standalone_fact_v1_distinguishes_source_target_and_fact_fields() {
        let base = fact(
            "The durable release window begins every Sunday at 02:00 UTC.",
            "operations",
        );
        let base_id = canonical_identity_for(&base, "source-a", "scope:global").0;
        assert_ne!(
            base_id,
            canonical_identity_for(&base, "source-b", "scope:global").0
        );
        assert_ne!(
            base_id,
            canonical_identity_for(&base, "source-a", "named-project:quant").0
        );

        for changed in [
            serde_json::json!({
                "text": "The durable release window begins every Sunday at 03:00 UTC.",
                "topic": "operations",
                "importance": 0.6,
            }),
            serde_json::json!({
                "text": "The durable release window begins every Sunday at 02:00 UTC.",
                "topic": "Operations",
                "importance": 0.6,
            }),
            serde_json::json!({
                "text": "The durable release window begins every Sunday at 02:00 UTC.",
                "topic": "operations",
                "importance": 0.5,
            }),
            serde_json::json!({
                "text": "The durable release window begins every Sunday at 02:00 UTC.",
                "topic": "operations",
                "importance": 0.6,
                "scope": "project",
            }),
        ] {
            assert_ne!(
                base_id,
                canonical_identity_for(&changed, "source-a", "scope:global").0
            );
        }
    }

    #[test]
    fn named_project_identity_is_exact_and_never_trimmed_for_routing() {
        let server = make_server();
        let error = save_atomized_facts(
            &server,
            vec![fact(
                "A malformed named-project alias must fail before any fact write.",
                "routing",
            )],
            "source-a".to_string(),
            Some(" quant ".to_string()),
        )
        .expect_err("whitespace-padded project identity must fail closed");

        assert!(
            error.contains("Invalid project identity ' quant '"),
            "unexpected project rejection: {error}"
        );
        let empty_error = save_atomized_facts(
            &server,
            Vec::new(),
            "source-a".to_string(),
            Some(" quant ".to_string()),
        )
        .expect_err("even an empty extraction must reject a malformed target identity");
        assert!(empty_error.contains("Invalid project identity ' quant '"));
        let rows = server
            .with_store_for_scope(DbScope::Global, |store| {
                store
                    .get_all(1000)
                    .map(|entries| entries.len())
                    .map_err(|error| error.to_string())
            })
            .expect("count global rows after rejected project route");
        assert_eq!(rows, 0, "a rejected named-project route must not write");
    }

    /// #1043 D1 judgement test 1: 3 independent facts atomize into 3 rows.
    /// Falsifies the regression this ticket names: a facade path that
    /// silently stores the whole source text as ONE entry regardless of how
    /// many facts the atomizer actually found.
    #[test]
    fn three_facts_atomize_into_three_rows_not_one() {
        let server = make_server();
        let count_rows = |server: &crate::MemoryServer| -> usize {
            server
                .with_store_for_scope(DbScope::Global, |store| {
                    store
                        .get_all(1000)
                        .map(|rows| rows.len())
                        .map_err(|e| e.to_string())
                })
                .expect("read back store")
        };
        let before = count_rows(&server);

        let facts = vec![
            fact(
                "The nightly database migration script runs at 2am UTC every day.",
                "ops",
            ),
            fact(
                "Payment retries are capped at three attempts before alerting on-call.",
                "payments",
            ),
            fact(
                "The staging environment shares its Redis instance with QA.",
                "infra",
            ),
        ];

        let body = save_atomized_facts(&server, facts, "test-source".to_string(), None)
            .expect("save_atomized_facts should succeed");
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json body");

        assert_eq!(value["atomized"], serde_json::json!(true));
        assert_eq!(value["facts_extracted"], serde_json::json!(3));
        assert_eq!(
            value["facts_saved"],
            serde_json::json!(3),
            "3 independent facts must save as 3 rows, not collapse to 1: {value}"
        );

        let after = count_rows(&server);
        assert_eq!(
            after - before,
            3,
            "store must gain 3 distinct rows after atomizing 3 facts, gained {}",
            after - before
        );
    }

    #[test]
    fn replay_reports_existing_without_creating_another_fact_row() {
        let server = make_server();
        let input = vec![fact(
            "The canonical deployment window begins at 02:00 UTC every Sunday.",
            "operations",
        )];

        let first = save_atomized_facts(
            &server,
            input.clone(),
            "deployment-policy".to_string(),
            None,
        )
        .expect("first extraction");
        let first: serde_json::Value = serde_json::from_str(&first).expect("first response JSON");
        assert_eq!(first["facts_inserted"], serde_json::json!(1));
        assert_eq!(first["facts_existing"], serde_json::json!(0));
        let id = first["facts"][0]["id"]
            .as_str()
            .expect("inserted fact id")
            .to_string();
        server
            .with_store_for_scope(DbScope::Global, |store| {
                let mut occupant = store
                    .get_with_options(&id, true)
                    .map_err(|error| error.to_string())?
                    .expect("inserted occupant");
                occupant.summary = "summary retained from the canonical occupant".to_string();
                store.upsert(&occupant).map_err(|error| error.to_string())
            })
            .expect("update non-identity occupant field");

        let replay = save_atomized_facts(&server, input, "deployment-policy".to_string(), None)
            .expect("replay extraction");
        let replay: serde_json::Value =
            serde_json::from_str(&replay).expect("replay response JSON");
        assert_eq!(replay["facts_saved"], serde_json::json!(0));
        assert_eq!(replay["facts_inserted"], serde_json::json!(0));
        assert_eq!(replay["facts_existing"], serde_json::json!(1));
        assert_eq!(replay["facts"][0]["id"], serde_json::json!(id));
        assert_eq!(
            replay["facts"][0]["summary"],
            serde_json::json!("summary retained from the canonical occupant"),
            "Existing must return the persisted canonical occupant, not the replay candidate"
        );

        let rows = server
            .with_store_for_scope(DbScope::Global, |store| {
                store
                    .get_all(1000)
                    .map(|entries| entries.len())
                    .map_err(|error| error.to_string())
            })
            .expect("count canonical fact rows");
        assert_eq!(rows, 1, "replay must not create a second fact row");
    }

    #[test]
    fn duplicate_facts_in_one_batch_report_inserted_then_existing() {
        let server = make_server();
        let duplicate = fact(
            "The same accepted fact appears twice in one extraction batch.",
            "dedupe",
        );
        let body = save_atomized_facts(
            &server,
            vec![duplicate.clone(), duplicate],
            "same-batch".to_string(),
            None,
        )
        .expect("same-batch extraction");
        let value: serde_json::Value = serde_json::from_str(&body).expect("response JSON");

        assert_eq!(value["status"], serde_json::json!("completed"));
        assert_eq!(value["facts_inserted"], serde_json::json!(1));
        assert_eq!(value["facts_existing"], serde_json::json!(1));
        assert_eq!(value["facts_saved"], serde_json::json!(1));
        assert_eq!(
            value["facts"][0]["disposition"],
            serde_json::json!("inserted")
        );
        assert_eq!(
            value["facts"][1]["disposition"],
            serde_json::json!("existing")
        );
    }

    #[test]
    fn archived_canonical_fact_remains_the_existing_winner() {
        let server = make_server();
        let input = vec![fact(
            "An archived canonical fact remains authoritative for exact replay.",
            "archive",
        )];
        let first = save_atomized_facts(&server, input.clone(), "archive-source".to_string(), None)
            .expect("first extraction");
        let first: serde_json::Value = serde_json::from_str(&first).expect("first JSON");
        let id = first["facts"][0]["id"]
            .as_str()
            .expect("fact id")
            .to_string();
        server
            .with_store_for_scope(DbScope::Global, |store| {
                assert!(store
                    .archive_memory(&id)
                    .map_err(|error| error.to_string())?);
                Ok(())
            })
            .expect("archive canonical fact");

        let replay = save_atomized_facts(&server, input, "archive-source".to_string(), None)
            .expect("archived replay");
        let replay: serde_json::Value = serde_json::from_str(&replay).expect("replay JSON");
        assert_eq!(replay["facts_inserted"], serde_json::json!(0));
        assert_eq!(replay["facts_existing"], serde_json::json!(1));
        assert_eq!(replay["facts"][0]["id"], serde_json::json!(id.clone()));
        let archived = server
            .with_store_for_scope(DbScope::Global, |store| {
                store
                    .get_with_options(&id, true)
                    .map(|entry| entry.expect("archived winner").archived)
                    .map_err(|error| error.to_string())
            })
            .expect("read archived winner");
        assert!(archived, "replay must not resurrect the archived winner");
    }

    #[test]
    fn identity_conflict_fails_one_item_and_continues_the_batch() {
        let server = make_server();
        let conflict = fact(
            "A deterministic identity conflict must not stop the next fact.",
            "conflict",
        );
        let mut occupied =
            fact_to_entry_candidate_with_reason(&conflict, "extraction", serde_json::json!({}))
                .expect("conflict candidate");
        canonicalize_standalone_fact_entry(&mut occupied);
        occupied.id = stable_standalone_fact_id(&occupied, "conflict-source", "scope:global");
        occupied.source = "manual".to_string();
        occupied.category = "other".to_string();
        occupied.metadata = serde_json::json!({"unrelated": true});
        server
            .with_store_for_scope(DbScope::Global, |store| {
                store
                    .insert_if_absent(&occupied)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .expect("seed unrelated occupant");

        let body = save_atomized_facts(
            &server,
            vec![
                conflict,
                fact(
                    "The later accepted fact must still be attempted and inserted.",
                    "later",
                ),
            ],
            "conflict-source".to_string(),
            None,
        )
        .expect("partial extraction response");
        let value: serde_json::Value = serde_json::from_str(&body).expect("response JSON");
        assert_eq!(value["status"], serde_json::json!("partial"));
        assert_eq!(value["facts_inserted"], serde_json::json!(1));
        assert_eq!(value["facts_existing"], serde_json::json!(0));
        assert_eq!(value["facts_failed"], serde_json::json!(1));
        assert_eq!(value["facts_saved"], serde_json::json!(1));
        assert_eq!(
            value["write_errors"][0]["kind"],
            serde_json::json!("identity_conflict")
        );
        assert_eq!(
            value["facts"].as_array().expect("successful facts").len(),
            1
        );
    }

    #[test]
    fn malformed_existing_identity_metadata_is_not_accepted_as_replay() {
        let server = make_server();
        let input = fact(
            "A canonical occupant missing required identity metadata is invalid.",
            "validation",
        );
        let mut occupied =
            fact_to_entry_candidate_with_reason(&input, "extraction", serde_json::json!({}))
                .expect("accepted occupant");
        canonicalize_standalone_fact_entry(&mut occupied);
        stamp_standalone_fact_identity(&mut occupied, "metadata-source", "scope:global")
            .expect("stamp identity");
        occupied
            .metadata
            .as_object_mut()
            .expect("metadata object")
            .remove("fact_identity_contract");
        server
            .with_store_for_scope(DbScope::Global, |store| {
                store
                    .insert_if_absent(&occupied)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .expect("seed malformed occupant");

        let body = save_atomized_facts(&server, vec![input], "metadata-source".to_string(), None)
            .expect("malformed occupant response");
        let value: serde_json::Value = serde_json::from_str(&body).expect("response JSON");
        assert_eq!(value["facts_existing"], serde_json::json!(0));
        assert_eq!(value["facts_failed"], serde_json::json!(1));
        assert_eq!(
            value["write_errors"][0]["kind"],
            serde_json::json!("existing_invalid")
        );
        let unchanged = server
            .with_store_for_scope(DbScope::Global, |store| {
                store
                    .get_with_options(&occupied.id, true)
                    .map(|entry| entry.expect("malformed occupant remains"))
                    .map_err(|error| error.to_string())
            })
            .expect("read malformed occupant");
        assert!(unchanged.metadata.get("fact_identity_contract").is_none());
    }

    #[test]
    fn forged_existing_metadata_cannot_override_persisted_identity_recomputation() {
        let server = make_server();
        let input = fact(
            "Persisted fact fields, not self-asserted metadata, prove identity.",
            "validation",
        );
        let mut occupied =
            fact_to_entry_candidate_with_reason(&input, "extraction", serde_json::json!({}))
                .expect("accepted occupant");
        canonicalize_standalone_fact_entry(&mut occupied);
        stamp_standalone_fact_identity(&mut occupied, "forged-source", "scope:global")
            .expect("stamp self-asserted identity");
        occupied.text =
            "Different persisted fields make this self-asserted identity a forgery.".to_string();
        occupied.summary = occupied.text.clone();
        server
            .with_store_for_scope(DbScope::Global, |store| {
                store
                    .insert_if_absent(&occupied)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .expect("seed forged occupant");

        let body = save_atomized_facts(&server, vec![input], "forged-source".to_string(), None)
            .expect("forged occupant response");
        let value: serde_json::Value = serde_json::from_str(&body).expect("response JSON");
        assert_eq!(value["facts_existing"], serde_json::json!(0));
        assert_eq!(value["facts_failed"], serde_json::json!(1));
        assert_eq!(
            value["write_errors"][0]["kind"],
            serde_json::json!("identity_conflict")
        );
        let unchanged = server
            .with_store_for_scope(DbScope::Global, |store| {
                store
                    .get_with_options(&occupied.id, true)
                    .map(|entry| entry.expect("forged occupant remains"))
                    .map_err(|error| error.to_string())
            })
            .expect("read forged occupant");
        assert_eq!(unchanged.text, occupied.text);
    }

    #[test]
    fn insert_once_fact_is_not_captured_by_a_jaccard_neighbor() {
        let server = make_server();
        let input = fact(
            "Canonical standalone facts bypass ordinary write-time Jaccard redirects.",
            "jaccard",
        );
        let mut neighbor = fact_to_entry_candidate_with_reason(
            &input,
            "manual",
            serde_json::json!({"neighbor": true}),
        )
        .expect("neighbor entry");
        neighbor.id = "unrelated-jaccard-neighbor".to_string();
        server
            .with_store_for_scope(DbScope::Global, |store| {
                store.upsert(&neighbor).map_err(|error| error.to_string())
            })
            .expect("seed Jaccard neighbor");

        let body = save_atomized_facts(&server, vec![input], "jaccard-source".to_string(), None)
            .expect("canonical extraction");
        let value: serde_json::Value = serde_json::from_str(&body).expect("response JSON");
        let canonical_id = value["facts"][0]["id"].as_str().expect("canonical id");
        assert_ne!(canonical_id, neighbor.id);
        assert_eq!(value["facts_inserted"], serde_json::json!(1));
        let rows = server
            .with_store_for_scope(DbScope::Global, |store| {
                store
                    .get_all(1000)
                    .map(|entries| entries.len())
                    .map_err(|error| error.to_string())
            })
            .expect("count independent rows");
        assert_eq!(rows, 2);
    }

    #[test]
    fn capture_gate_drop_shape_and_reason_remain_compatible() {
        let server = make_server();
        let body = save_atomized_facts(
            &server,
            vec![serde_json::json!({"text": "too short", "topic": "gate"})],
            "capture-gate".to_string(),
            None,
        )
        .expect("capture-gate response");
        let value: serde_json::Value = serde_json::from_str(&body).expect("response JSON");

        assert_eq!(value["status"], serde_json::json!("completed"));
        assert_eq!(value["facts_extracted"], serde_json::json!(1));
        assert_eq!(value["facts_saved"], serde_json::json!(0));
        assert_eq!(value["facts_inserted"], serde_json::json!(0));
        assert_eq!(value["facts_existing"], serde_json::json!(0));
        assert_eq!(value["facts_failed"], serde_json::json!(0));
        assert_eq!(value["facts_dropped"], serde_json::json!(1));
        assert_eq!(
            value["dropped"],
            serde_json::json!([{"reason": "too_short", "text": "too short"}])
        );
    }

    #[test]
    fn empty_and_llm_failure_responses_have_explicit_zero_accounting() {
        let server = make_server();
        let empty = save_atomized_facts(&server, Vec::new(), "empty".to_string(), None)
            .expect("empty extraction");
        let empty: serde_json::Value = serde_json::from_str(&empty).expect("empty JSON");
        let failed = llm_extraction_failed_response("failed", "provider unavailable")
            .expect("failure response");
        let failed: serde_json::Value = serde_json::from_str(&failed).expect("failure JSON");

        for response in [&empty, &failed] {
            assert_eq!(response["facts_inserted"], serde_json::json!(0));
            assert_eq!(response["facts_existing"], serde_json::json!(0));
            assert_eq!(response["facts_failed"], serde_json::json!(0));
            assert_eq!(response["facts_dropped"], serde_json::json!(0));
        }
    }

    /// #1043 D1 judgement test 2: LLM-unavailable degrade path is labeled,
    /// not silent. A caller must be able to tell "the atomizer never ran"
    /// apart from "the atomizer ran and found nothing" from the response
    /// shape alone.
    #[test]
    fn llm_unavailable_labels_atomized_false() {
        let body = llm_extraction_failed_response("test-source", "provider unreachable")
            .expect("failure response always serializes");
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json body");

        assert_eq!(value["status"], serde_json::json!("failed"));
        assert_eq!(value["atomized"], serde_json::json!(false));
        assert_eq!(value["reason"], serde_json::json!("llm_extraction_failed"));
        assert_eq!(value["facts_saved"], serde_json::json!(0));
    }
}
