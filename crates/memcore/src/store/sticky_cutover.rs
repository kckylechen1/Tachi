#[cfg(test)]
mod tests {
    use crate::db::session_claims::{
        insert_agent_identity, record_unverified_admission, AgentIdentity, UnverifiedAdmissionState,
    };
    use crate::{MemoryEntry, MemoryError, MemoryStore};

    fn fixture_store() -> (tempfile::TempDir, String, MemoryStore) {
        let temp = tempfile::tempdir().expect("temp store");
        let path = temp.path().join("tachi-memory.db");
        let path = path.to_string_lossy().into_owned();
        let store = MemoryStore::open_with_context(&path, &crate::DbOpenContext::create_fresh())
            .expect("fresh Product store");
        (temp, path, store)
    }

    fn identity(store: &MemoryStore, id: &str, admission: &str) {
        insert_agent_identity(
            store.connection(),
            &AgentIdentity {
                agent_identity_id: id.to_string(),
                display_name: None,
                seat: None,
                capability_json: None,
                created_at: "2026-08-12T00:00:00Z".to_string(),
            },
        )
        .expect("identity");
        record_unverified_admission(
            store.connection(),
            admission,
            id,
            &format!("connection-{admission}"),
            UnverifiedAdmissionState::SelfAsserted,
        )
        .expect("local admission");
    }

    fn sticky_entry(
        id: &str,
        to: Option<&str>,
        status: &str,
        created_at: &str,
        ttl_days: u32,
    ) -> MemoryEntry {
        let text = format!("legacy body {id}");
        MemoryEntry {
            id: format!("sticky:{id}"),
            path: match to {
                Some(target) => format!("/sticky/to/{target}"),
                None => "/sticky/broadcast".to_string(),
            },
            summary: format!("Sticky from sender-{id}"),
            text: text.clone(),
            importance: 0.6,
            timestamp: created_at.to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "sticky".to_string(),
            topic: "agent-sticky".to_string(),
            keywords: vec!["sticky".to_string()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "extraction".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: Some("ephemeral".to_string()),
            domain: None,
            metadata: serde_json::json!({
                "sticky_id": id,
                "sticky": {
                    "id": id,
                    "from_agent": format!("sender-{id}"),
                    "to": to,
                    "text": text,
                    "created_at": created_at,
                    "ttl_days": ttl_days,
                    "identity_assurance": "session"
                },
                "status": status
            }),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn seed_matrix(store: &mut MemoryStore) {
        for mut entry in [
            sticky_entry("broadcast", None, "unread", "2026-08-12T00:00:00Z", 7),
            sticky_entry(
                "addressed",
                Some("legacy-seat"),
                "unread",
                "2026-08-12T00:00:00Z",
                7,
            ),
            sticky_entry(
                "claimed-cas",
                Some("legacy-seat"),
                "claimed",
                "2026-08-12T00:00:00Z",
                7,
            ),
            sticky_entry(
                "claimed-no-cas",
                Some("legacy-seat"),
                "claimed",
                "2026-08-12T00:00:00Z",
                7,
            ),
            sticky_entry("expired", None, "unread", "2026-07-01T00:00:00Z", 1),
        ] {
            match entry.id.as_str() {
                "sticky:claimed-cas" | "sticky:claimed-no-cas" => {
                    // Base producer `mark_claimed` archives the row after the
                    // independent hard_state claim CAS succeeds.
                    entry.archived = true;
                }
                "sticky:expired" => {
                    // Base producer `mark_expired` mirrors status=expired and
                    // archives the row; cutover must still inventory it.
                    entry.metadata["status"] = serde_json::json!("expired");
                    entry.archived = true;
                }
                _ => {}
            }
            store.upsert(&entry).expect("seed sticky row");
        }

        let mut corrupt = sticky_entry("corrupt", None, "unread", "2026-08-12T00:00:00Z", 7);
        corrupt.metadata = serde_json::json!({"sticky_id":"corrupt","status":"unread"});
        corrupt.archived = true;
        store.upsert(&corrupt).expect("seed corrupt row");

        let mut empty = sticky_entry("empty-fields", None, "unread", "2026-08-12T00:00:00Z", 7);
        empty.metadata["sticky"]["text"] = serde_json::json!("");
        empty.metadata["sticky"]
            .as_object_mut()
            .unwrap()
            .remove("created_at");
        store.upsert(&empty).expect("seed empty/missing fields row");

        store
            .set_state(
                "sticky_claim",
                "claimed-cas",
                r#"{"claimed_by":"reader","claimed_at":"2026-08-12T01:00:00Z"}"#,
            )
            .expect("seed claim CAS");
        store
            .set_state("sticky_claim", "cas-only", "{malformed")
            .expect("seed CAS-only malformed evidence");
    }

    #[test]
    fn plan_is_read_only_deterministic_and_classifies_the_frozen_legacy_matrix() {
        let (_temp, path, mut store) = fixture_store();
        seed_matrix(&mut store);
        let before_changes = store.connection().total_changes();

        let first = store
            .plan_sticky_cutover_at(path.clone(), "2026-08-13T00:00:00Z", |body| {
                format!("scrubbed:{body}")
            })
            .expect("plan");
        let second = store
            .plan_sticky_cutover_at(path, "2026-08-13T00:00:00Z", |body| {
                format!("scrubbed:{body}")
            })
            .expect("repeat plan");

        assert_eq!(first, second, "same snapshot and as-of must be byte-stable");
        assert_eq!(
            store.connection().total_changes(),
            before_changes,
            "planning must execute no write"
        );
        let classes = first
            .rows
            .iter()
            .map(|row| (row.sticky_id.as_str(), row.classification))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(classes["broadcast"], super::LegacyStickyClass::Unread);
        assert_eq!(classes["addressed"], super::LegacyStickyClass::Unread);
        assert_eq!(
            classes["claimed-cas"],
            super::LegacyStickyClass::HistoricalClaimed
        );
        assert_eq!(
            classes["claimed-no-cas"],
            super::LegacyStickyClass::HistoricalClaimed
        );
        assert_eq!(
            classes["expired"],
            super::LegacyStickyClass::HistoricalExpired
        );
        assert_eq!(
            classes["corrupt"],
            super::LegacyStickyClass::HistoricalCorrupt
        );
        assert_eq!(classes["empty-fields"], super::LegacyStickyClass::Corrupt);
        assert_eq!(classes["cas-only"], super::LegacyStickyClass::CasOnly);
        assert!(first
            .rows
            .iter()
            .filter(|row| row.requires_disposition)
            .all(
                |row| row.disposition == super::StickyCutoverDisposition::Unresolved
                    && matches!(row.sticky_id.as_str(), "broadcast" | "addressed")
            ));
        assert!(first
            .rows
            .iter()
            .filter(|row| matches!(
                row.classification,
                super::LegacyStickyClass::HistoricalClaimed
                    | super::LegacyStickyClass::HistoricalExpired
                    | super::LegacyStickyClass::HistoricalCorrupt
            ))
            .all(|row| row.disposition == super::StickyCutoverDisposition::ArchiveOnly));

        let error = store
            .apply_sticky_cutover_with_precommit_receipt(
                &first,
                |body| format!("scrubbed:{body}"),
                |_| Ok(()),
            )
            .expect_err("unresolved unread rows must block every mutation");
        assert!(error.to_string().contains("unresolved"));
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM a2a_envelopes", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );

        let mut resurrection = first.clone();
        resurrection.issuer_agent_identity_id = Some("operator".to_string());
        resurrection.issuer_connection_id = Some("connection-operator".to_string());
        for row in &mut resurrection.rows {
            row.disposition = if row.sticky_id == "claimed-cas" {
                super::StickyCutoverDisposition::Convert {
                    recipient_agent_identity_id: "recipient".to_string(),
                }
            } else if row.classification == super::LegacyStickyClass::Unread {
                super::StickyCutoverDisposition::Discard {
                    reason: "fixture resolves every genuinely unread row".to_string(),
                }
            } else {
                super::StickyCutoverDisposition::ArchiveOnly
            };
        }
        let error = store
            .apply_sticky_cutover_with_precommit_receipt(
                &resurrection,
                |body| format!("scrubbed:{body}"),
                |_| Ok(()),
            )
            .expect_err("an archived historical row must never resurrect as an envelope");
        assert!(error.to_string().contains("non-unread"));
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM a2a_envelopes", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    fn decide(plan: &mut super::StickyCutoverPlan) {
        plan.issuer_agent_identity_id = Some("operator".to_string());
        plan.issuer_connection_id = Some("connection-admission-operator".to_string());
        for row in &mut plan.rows {
            row.disposition = match row.sticky_id.as_str() {
                "broadcast" => super::StickyCutoverDisposition::Convert {
                    recipient_agent_identity_id: "recipient".to_string(),
                },
                "addressed" => super::StickyCutoverDisposition::Discard {
                    reason: "owner adjudicated legacy address".to_string(),
                },
                _ => super::StickyCutoverDisposition::ArchiveOnly,
            };
        }
    }

    fn assert_frozen_plan_mutation_rejected(mutate: impl FnOnce(&mut super::StickyCutoverPlan)) {
        let (_temp, path, mut store) = fixture_store();
        seed_matrix(&mut store);
        identity(&store, "operator", "admission-operator");
        identity(&store, "recipient", "admission-recipient");
        let mut plan = store
            .plan_sticky_cutover_at(path.clone(), "2026-08-13T00:00:00Z", str::to_string)
            .expect("plan");
        decide(&mut plan);
        mutate(&mut plan);
        plan.plan_digest = plan.compute_digest().expect("mutated plan digest");

        let before = std::fs::read(&path).expect("read database before rejected apply");
        let error = store
            .apply_sticky_cutover_with_precommit_receipt(&plan, str::to_string, |_| Ok(()))
            .expect_err("a plan mutation outside the editable decision surface must be refused");
        assert!(error.to_string().contains("source census"), "{error}");
        let after = std::fs::read(&path).expect("read database after rejected apply");
        assert_eq!(
            before, after,
            "rejected source drift must not mutate the database"
        );
    }

    #[test]
    fn apply_rejects_every_mutation_of_the_frozen_source_census_before_writes() {
        assert_frozen_plan_mutation_rejected(|plan| {
            plan.rows
                .iter_mut()
                .find(|row| row.sticky_id == "broadcast")
                .expect("broadcast row")
                .scrubbed_body = Some("attacker-controlled replacement".to_string());
        });
        assert_frozen_plan_mutation_rejected(|plan| {
            let row = plan
                .rows
                .iter_mut()
                .find(|row| row.sticky_id == "broadcast")
                .expect("broadcast row");
            row.created_at = Some("2026-08-12T01:00:00Z".to_string());
            row.expires_at = Some("2026-08-19T01:00:00Z".to_string());
        });
        assert_frozen_plan_mutation_rejected(|plan| {
            plan.rows
                .iter_mut()
                .find(|row| row.sticky_id == "claimed-cas")
                .expect("claimed row")
                .classification = super::LegacyStickyClass::HistoricalExpired;
        });
        assert_frozen_plan_mutation_rejected(|plan| {
            plan.rows.retain(|row| row.sticky_id != "cas-only");
            plan.planned_rows = plan.rows.len();
        });
        assert_frozen_plan_mutation_rejected(|plan| {
            let mut replacement = plan
                .rows
                .iter()
                .find(|row| row.sticky_id == "cas-only")
                .expect("CAS-only row")
                .clone();
            replacement.sticky_id = "cas-only-replacement".to_string();
            replacement.claim_evidence = super::StickyClaimEvidence {
                state: super::StickyClaimEvidenceState::Missing,
                version: None,
                evidence_digest: super::sha256(b"missing"),
            };
            plan.rows.retain(|row| row.sticky_id != "cas-only");
            plan.rows.push(replacement);
            plan.rows.sort_by(|left, right| {
                left.sticky_id
                    .cmp(&right.sticky_id)
                    .then_with(|| left.memory_id.cmp(&right.memory_id))
            });
            plan.planned_rows = plan.rows.len();
        });
    }

    #[test]
    fn apply_is_atomic_cas_bound_and_replay_safe_across_envelope_and_archive() {
        let (_temp, path, mut store) = fixture_store();
        seed_matrix(&mut store);
        identity(&store, "operator", "admission-operator");
        identity(&store, "recipient", "admission-recipient");
        let mut plan = store
            .plan_sticky_cutover_at(path, "2026-08-13T00:00:00Z", |body| {
                format!("scrubbed:{body}")
            })
            .expect("plan");
        decide(&mut plan);

        let injected = store
            .apply_sticky_cutover_with_precommit_receipt(
                &plan,
                |body| format!("scrubbed:{body}"),
                |prepared| {
                    assert_eq!(
                        prepared.receipt.phase,
                        super::StickyCutoverReceiptPhase::Prepared
                    );
                    Err(MemoryError::InvalidArg(
                        "injected receipt persistence failure".to_string(),
                    ))
                },
            )
            .expect_err("precommit receipt failure must roll back DB transaction");
        assert!(injected.to_string().contains("injected receipt"));
        let rolled_back: (i64, i64) = store
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM a2a_envelopes),
                        (SELECT COUNT(*) FROM memories WHERE category='sticky' AND archived=1)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            rolled_back,
            (0, 4),
            "the producer-faithful historical rows stay archived, while every cutover write rolls back"
        );

        let result = store
            .apply_sticky_cutover_with_precommit_receipt(
                &plan,
                |body| format!("scrubbed:{body}"),
                |prepared| {
                    assert_eq!(
                        prepared.receipt.phase,
                        super::StickyCutoverReceiptPhase::Prepared
                    );
                    Ok(())
                },
            )
            .expect("apply");
        assert!(!result.replayed);
        assert_eq!(
            result.receipt.phase,
            super::StickyCutoverReceiptPhase::Committed
        );
        assert_eq!(result.converted, 1);
        assert_eq!(result.discarded, 1);
        assert_eq!(result.archived_only, 6);

        let committed: (i64, i64, String) = store
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM a2a_envelopes),
                        (SELECT COUNT(*) FROM memories WHERE category='sticky' AND archived=1),
                        (SELECT idempotency_key FROM a2a_envelopes)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(committed, (1, 7, "legacy-sticky:broadcast".to_string()));
        let archive_json: String = store
            .connection()
            .query_row(
                "SELECT json_extract(metadata,'$.legacy_sticky_archive')
                 FROM memories WHERE id='sticky:broadcast'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let archive: serde_json::Value =
            serde_json::from_str(&archive_json).expect("archive metadata JSON");
        assert_eq!(archive["claim_evidence"]["state"], "missing");
        assert_eq!(archive["outcome"], "converted");
        let converted_row = result
            .receipt
            .rows
            .iter()
            .find(|row| row.sticky_id == "broadcast")
            .expect("converted receipt row");
        assert!(converted_row.envelope_id.is_some());
        assert!(converted_row.envelope_body_digest.is_some());
        assert!(converted_row.delivery_receipt_id.is_some());
        let committed_json = serde_json::to_string(&result.receipt).expect("receipt JSON");
        for sticky_id in [
            "broadcast",
            "addressed",
            "claimed-cas",
            "claimed-no-cas",
            "expired",
            "corrupt",
            "empty-fields",
        ] {
            assert!(
                !committed_json.contains(&format!("legacy body {sticky_id}")),
                "committed receipt retained legacy body for {sticky_id}"
            );
        }
        assert!(!committed_json.contains("scrubbed:legacy body broadcast"));
        let archived_payload: (String, String) = store
            .connection()
            .query_row(
                "SELECT text,metadata FROM memories WHERE id='sticky:broadcast'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(archived_payload.0.is_empty());
        assert!(!archived_payload.1.contains("legacy body broadcast"));
        let lingering_archive_bodies: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memories
                 WHERE category='sticky' AND (text<>'' OR metadata LIKE '%legacy body%')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lingering_archive_bodies, 0);
        let a2a_body: String = store
            .connection()
            .query_row("SELECT body FROM a2a_envelopes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(a2a_body, "scrubbed:legacy body broadcast");

        let replay = store
            .apply_sticky_cutover_with_precommit_receipt(
                &plan,
                |body| format!("scrubbed:{body}"),
                |_| Ok(()),
            )
            .expect("replay");
        assert!(replay.replayed);
        assert_eq!(replay.receipt, result.receipt);
        let replay_counts: (i64, i64) = store
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM a2a_envelopes),
                        (SELECT COUNT(*) FROM a2a_delivery_receipts)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(replay_counts, (1, 1));
    }

    #[test]
    fn claim_evidence_whitelists_fields_and_binds_raw_digest_without_retaining_value_json() {
        let (_temp, path, mut store) = fixture_store();
        seed_matrix(&mut store);
        identity(&store, "operator", "admission-operator");
        identity(&store, "recipient", "admission-recipient");
        let source_claim = r#"{"claimed_by":"CLAIM_JSON_SECRET","claimed_at":"2026-08-12T01:00:00Z","type":"sticky_claim","state":"claimed","body":"DO_NOT_PERSIST_CLAIM_SECRET"}"#;
        store
            .set_state("sticky_claim", "claimed-cas", source_claim)
            .expect("seed claim with an untrusted extra field");
        {
            let _authorization =
                crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                    .expect("authorize legacy metadata fixture");
            store
                .connection()
                .execute(
                    "UPDATE memories
                     SET metadata=json_set(metadata,'$.claimed_by','MEMORY_METADATA_SECRET',
                                           '$.claimed_at','MEMORY_METADATA_AT_SECRET',
                                           '$.sticky.to','LEGACY_TO_SECRET')
                     WHERE id='sticky:claimed-cas'",
                    [],
                )
                .expect("seed legacy metadata secrets");
        }
        let plan = store
            .plan_sticky_cutover_at(path, "2026-08-13T00:00:00Z", str::to_string)
            .expect("plan");
        let claim = &plan
            .rows
            .iter()
            .find(|row| row.sticky_id == "claimed-cas")
            .expect("claimed row")
            .claim_evidence;
        assert_eq!(claim.state, super::StickyClaimEvidenceState::Valid);
        assert_eq!(
            claim.evidence_digest,
            super::sha256(source_claim.as_bytes()),
            "CAS evidence must bind the raw source by digest without retaining it"
        );
        let plan_json = serde_json::to_string(&plan).expect("plan JSON");
        assert!(
            !plan_json.contains("DO_NOT_PERSIST_CLAIM_SECRET")
                && !plan_json.contains("CLAIM_JSON_SECRET")
                && !plan_json.contains("MEMORY_METADATA_SECRET")
                && !plan_json.contains("MEMORY_METADATA_AT_SECRET")
                && !plan_json.contains("LEGACY_TO_SECRET"),
            "plan must not retain arbitrary legacy claim fields"
        );

        let mut decided = plan.clone();
        decide(&mut decided);
        let changed_claim = r#"{"claimed_by":"CLAIM_JSON_SECRET","claimed_at":"2026-08-12T01:00:00Z","type":"sticky_claim","state":"claimed","body":"CHANGED_CLAIM_SECRET"}"#;
        {
            let _authorization =
                crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                    .expect("authorize claim CAS drift fixture");
            store
                .connection()
                .execute(
                    "UPDATE hard_state SET value_json=?1
                     WHERE namespace='sticky_claim' AND key='claimed-cas'",
                    [changed_claim],
                )
                .expect("mutate raw claim source");
        }
        let error = store
            .apply_sticky_cutover_with_precommit_receipt(&decided, str::to_string, |_| Ok(()))
            .expect_err("raw claim changes must fail independent CAS");
        assert!(error.to_string().contains("claim CAS drift"));

        {
            let _authorization =
                crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                    .expect("authorize claim CAS restore fixture");
            store
                .connection()
                .execute(
                    "UPDATE hard_state SET value_json=?1
                     WHERE namespace='sticky_claim' AND key='claimed-cas'",
                    [source_claim],
                )
                .expect("restore raw claim source");
        }
        let result = store
            .apply_sticky_cutover_with_precommit_receipt(&decided, str::to_string, |prepared| {
                let prepared_json =
                    serde_json::to_string(&prepared.receipt).expect("prepared JSON");
                assert!(
                    !prepared_json.contains("DO_NOT_PERSIST_CLAIM_SECRET")
                        && !prepared_json.contains("CLAIM_JSON_SECRET")
                        && !prepared_json.contains("MEMORY_METADATA_SECRET")
                        && !prepared_json.contains("MEMORY_METADATA_AT_SECRET")
                        && !prepared_json.contains("LEGACY_TO_SECRET"),
                    "prepared receipt must not retain arbitrary legacy claim fields"
                );
                Ok(())
            })
            .expect("apply");
        let committed_json = serde_json::to_string(&result.receipt).expect("committed JSON");
        assert!(
            !committed_json.contains("DO_NOT_PERSIST_CLAIM_SECRET")
                && !committed_json.contains("CLAIM_JSON_SECRET")
                && !committed_json.contains("MEMORY_METADATA_SECRET")
                && !committed_json.contains("MEMORY_METADATA_AT_SECRET")
                && !committed_json.contains("LEGACY_TO_SECRET"),
            "committed receipt must not retain arbitrary legacy claim fields"
        );
        let archive_json: String = store
            .connection()
            .query_row(
                "SELECT json_extract(metadata,'$.legacy_sticky_archive')
                 FROM memories WHERE id='sticky:claimed-cas'",
                [],
                |row| row.get(0),
            )
            .expect("archive JSON");
        assert!(
            !archive_json.contains("DO_NOT_PERSIST_CLAIM_SECRET")
                && !archive_json.contains("CLAIM_JSON_SECRET")
                && !archive_json.contains("MEMORY_METADATA_SECRET")
                && !archive_json.contains("MEMORY_METADATA_AT_SECRET")
                && !archive_json.contains("LEGACY_TO_SECRET"),
            "archive must not retain arbitrary legacy claim fields"
        );
    }

    #[test]
    fn raw_legacy_timestamps_are_not_serialized_in_plan_receipt_or_archive() {
        const CLAIM_CREATED_SECRET: &str = "RAW_CLAIM_CREATED_TIMESTAMP_SECRET";
        const CLAIM_UPDATED_SECRET: &str = "RAW_CLAIM_UPDATED_TIMESTAMP_SECRET";
        const SOURCE_TIMESTAMP_SECRET: &str = "RAW_SOURCE_TIMESTAMP_SECRET";
        const SOURCE_VALID_UNTIL_SECRET: &str = "RAW_SOURCE_VALID_UNTIL_SECRET";

        let (_temp, path, mut store) = fixture_store();
        let entry = sticky_entry(
            "timestamp-secrets",
            None,
            "claimed",
            "2026-08-12T00:00:00Z",
            7,
        );
        store.upsert(&entry).expect("seed timestamp fixture");
        store
            .set_state(
                "sticky_claim",
                "timestamp-secrets",
                r#"{"claimed_by":"reader","claimed_at":"2026-08-12T01:00:00Z"}"#,
            )
            .expect("seed timestamp claim");
        {
            let _authorization =
                crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                    .expect("authorize timestamp fixture");
            store
                .connection()
                .execute(
                    "UPDATE memories SET timestamp=?1,valid_until=?2
                     WHERE id='sticky:timestamp-secrets'",
                    rusqlite::params![SOURCE_TIMESTAMP_SECRET, SOURCE_VALID_UNTIL_SECRET],
                )
                .expect("poison source timestamps");
            store
                .connection()
                .execute(
                    "UPDATE hard_state SET created_at=?1,updated_at=?2
                     WHERE namespace='sticky_claim' AND key='timestamp-secrets'",
                    rusqlite::params![CLAIM_CREATED_SECRET, CLAIM_UPDATED_SECRET],
                )
                .expect("poison claim timestamps");
        }

        let plan = store
            .plan_sticky_cutover_at(path, "2026-08-13T00:00:00Z", str::to_string)
            .expect("plan");
        let plan_json = serde_json::to_string(&plan).expect("plan JSON");
        for secret in [
            CLAIM_CREATED_SECRET,
            CLAIM_UPDATED_SECRET,
            SOURCE_TIMESTAMP_SECRET,
            SOURCE_VALID_UNTIL_SECRET,
        ] {
            assert!(!plan_json.contains(secret), "plan retained {secret}");
        }
        let plan_value: serde_json::Value = serde_json::from_str(&plan_json).expect("plan value");
        let claim_evidence = &plan_value["rows"][0]["claim_evidence"];
        assert!(claim_evidence.get("created_at").is_none());
        assert!(claim_evidence.get("updated_at").is_none());

        let mut prepared_json = None;
        let result = store
            .apply_sticky_cutover_with_precommit_receipt(&plan, str::to_string, |prepared| {
                let serialized = serde_json::to_string(&prepared.receipt).expect("prepared JSON");
                for secret in [
                    CLAIM_CREATED_SECRET,
                    CLAIM_UPDATED_SECRET,
                    SOURCE_TIMESTAMP_SECRET,
                    SOURCE_VALID_UNTIL_SECRET,
                ] {
                    assert!(
                        !serialized.contains(secret),
                        "prepared receipt retained {secret}"
                    );
                }
                prepared_json = Some(serialized);
                Ok(())
            })
            .expect("apply");
        assert!(prepared_json.is_some());
        let committed_json = serde_json::to_string(&result.receipt).expect("committed JSON");
        for secret in [
            CLAIM_CREATED_SECRET,
            CLAIM_UPDATED_SECRET,
            SOURCE_TIMESTAMP_SECRET,
            SOURCE_VALID_UNTIL_SECRET,
        ] {
            assert!(
                !committed_json.contains(secret),
                "committed receipt retained {secret}"
            );
        }
        let archive_json: String = store
            .connection()
            .query_row(
                "SELECT json_extract(metadata,'$.legacy_sticky_archive')
                 FROM memories WHERE id='sticky:timestamp-secrets'",
                [],
                |row| row.get(0),
            )
            .expect("archive JSON");
        for secret in [
            CLAIM_CREATED_SECRET,
            CLAIM_UPDATED_SECRET,
            SOURCE_TIMESTAMP_SECRET,
            SOURCE_VALID_UNTIL_SECRET,
        ] {
            assert!(!archive_json.contains(secret), "archive retained {secret}");
        }
        let archive: serde_json::Value = serde_json::from_str(&archive_json).expect("archive");
        let source_evidence = &archive["source_evidence"];
        assert!(source_evidence.get("timestamp").is_none());
        assert!(source_evidence.get("valid_until").is_none());
        let archived_claim = &archive["claim_evidence"];
        assert!(archived_claim.get("created_at").is_none());
        assert!(archived_claim.get("updated_at").is_none());
    }

    #[test]
    fn legacy_path_is_digest_only_across_plan_receipts_and_archive_and_cas_binds_full_row() {
        const PATH_SECRET: &str = "LEGACY_PATH_SECRET";
        let (_temp, path, mut store) = fixture_store();
        let entry = sticky_entry(
            "path-secret",
            Some(PATH_SECRET),
            "unread",
            "2026-08-12T00:00:00Z",
            7,
        );
        assert_eq!(entry.path, format!("/sticky/to/{PATH_SECRET}"));
        assert_eq!(entry.metadata["sticky"]["to"], PATH_SECRET);
        store.upsert(&entry).expect("seed path secret row");

        let mut plan = store
            .plan_sticky_cutover_at(path.clone(), "2026-08-13T00:00:00Z", str::to_string)
            .expect("plan");
        let path_row = plan
            .rows
            .iter()
            .find(|row| row.sticky_id == "path-secret")
            .expect("path secret row");
        let source_row_digest = path_row.memory_row_digest.clone().expect("row digest");
        let plan_json = serde_json::to_string(&plan).expect("plan JSON");
        assert!(
            !plan_json.contains(PATH_SECRET),
            "plan retained raw legacy path or recipient"
        );

        decide(&mut plan);
        for row in &mut plan.rows {
            if row.sticky_id == "path-secret" {
                row.disposition = super::StickyCutoverDisposition::Discard {
                    reason: "fixture resolves path-secret as unread".to_string(),
                };
            }
        }
        {
            let _authorization =
                crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                    .expect("authorize path CAS fixture");
            store
                .connection()
                .execute(
                    "UPDATE memories SET path=?1 WHERE id='sticky:path-secret'",
                    [format!("/sticky/to/{PATH_SECRET}-changed")],
                )
                .expect("mutate legacy source path");
        }
        let error = store
            .apply_sticky_cutover_with_precommit_receipt(&plan, str::to_string, |_| Ok(()))
            .expect_err("changing legacy path must fail full-row CAS");
        assert!(
            error.to_string().contains("memory CAS drift"),
            "actual CAS error: {error}"
        );

        {
            let _authorization =
                crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                    .expect("authorize path CAS restore fixture");
            store
                .connection()
                .execute(
                    "UPDATE memories SET path=?1 WHERE id='sticky:path-secret'",
                    [format!("/sticky/to/{PATH_SECRET}")],
                )
                .expect("restore legacy source path");
        }
        let mut prepared_json = None;
        let result = store
            .apply_sticky_cutover_with_precommit_receipt(&plan, str::to_string, |prepared| {
                let serialized = serde_json::to_string(&prepared.receipt).expect("prepared JSON");
                assert!(
                    !serialized.contains(PATH_SECRET),
                    "prepared receipt retained raw legacy path or recipient"
                );
                prepared_json = Some(serialized);
                Ok(())
            })
            .expect("apply");
        assert!(prepared_json.is_some());
        let committed_json = serde_json::to_string(&result.receipt).expect("committed JSON");
        assert!(
            !committed_json.contains(PATH_SECRET),
            "committed receipt retained raw legacy path or recipient"
        );
        let archive_json: String = store
            .connection()
            .query_row(
                "SELECT json_extract(metadata,'$.legacy_sticky_archive')
                 FROM memories WHERE id='sticky:path-secret'",
                [],
                |row| row.get(0),
            )
            .expect("archive JSON");
        assert!(
            !archive_json.contains(PATH_SECRET),
            "archive retained raw legacy path or recipient"
        );
        let archive: serde_json::Value = serde_json::from_str(&archive_json).expect("archive");
        assert_eq!(
            archive["source_evidence"]["path_digest"],
            super::sha256(format!("/sticky/to/{PATH_SECRET}").as_bytes())
        );
        assert!(archive["source_evidence"].get("path").is_none());
        let receipt_row = result
            .receipt
            .rows
            .iter()
            .find(|row| row.sticky_id == "path-secret")
            .expect("path secret receipt row");
        assert_eq!(
            receipt_row.source_row_digest.as_deref(),
            Some(source_row_digest.as_str())
        );
    }

    #[test]
    fn apply_rejects_memory_cas_or_physical_database_drift_before_mutation() {
        let (_temp, path, mut store) = fixture_store();
        seed_matrix(&mut store);
        identity(&store, "operator", "admission-operator");
        identity(&store, "recipient", "admission-recipient");
        let mut plan = store
            .plan_sticky_cutover_at(path, "2026-08-13T00:00:00Z", str::to_string)
            .expect("plan");
        decide(&mut plan);
        {
            let _authorization =
                crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                    .expect("authorize sticky cutover drift fixture");
            store
                .connection()
                .execute(
                    "UPDATE memories SET revision=revision+1 WHERE id='sticky:broadcast'",
                    [],
                )
                .unwrap();
        }
        let error = store
            .apply_sticky_cutover_with_precommit_receipt(&plan, str::to_string, |_| Ok(()))
            .expect_err("row drift must fail CAS");
        assert!(error.to_string().contains("CAS"));

        let (_other_temp, _other_path, mut other) = fixture_store();
        let error = other
            .apply_sticky_cutover_with_precommit_receipt(&plan, str::to_string, |_| Ok(()))
            .expect_err("another physical DB must reject the plan");
        assert!(error.to_string().contains("physical DB identity"));
    }
}
// One-shot operator cutover from the retired sticky mailbox into v31 A2A.
//
// The plan freezes both the legacy memory row and its independent claim CAS.
// Apply revalidates both under one `BEGIN IMMEDIATE`, inserts an A2A envelope
// only for an explicitly mapped unread row, and archives the original memory
// in that same transaction.
//
// This stays a specialized, admin-gated module: the source classification,
// independent claim CAS, body-retention rule, and envelope/archive atomicity
// are one retirement contract, not a reusable message-bus or transaction API.
// Its methods are public only because the operator CLI lives in the sibling
// `tachi-server` crate. The transaction-local A2A seam remains crate-private.

use super::super::{MemoryError, MemoryStore};
use crate::db::a2a::{insert_a2a_envelope_in_tx, A2aInsertOutcome, NewA2aEnvelope};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const STICKY_CUTOVER_SCHEMA_VERSION: u32 = 1;
pub const STICKY_CUTOVER_POLICY: &str = "legacy-sticky-to-a2a-v1";
pub const STICKY_CUTOVER_RECEIPT_SCHEMA_VERSION: u32 = 1;
const RECEIPT_NAMESPACE: &str = "a2a_sticky_cutover_receipt";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LegacyStickyClass {
    Unread,
    Claimed,
    Expired,
    Corrupt,
    HistoricalClaimed,
    HistoricalExpired,
    HistoricalCorrupt,
    CasOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StickyClaimEvidenceState {
    Missing,
    Valid,
    Malformed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StickyClaimEvidence {
    pub state: StickyClaimEvidenceState,
    pub version: Option<i64>,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum StickyCutoverDisposition {
    Unresolved,
    Convert { recipient_agent_identity_id: String },
    Discard { reason: String },
    ArchiveOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrozenLegacyStickyRow {
    pub sticky_id: String,
    pub memory_id: Option<String>,
    pub memory_revision: Option<i64>,
    pub memory_row_digest: Option<String>,
    pub source_archived: bool,
    pub created_at: Option<String>,
    pub expires_at: Option<String>,
    pub scrubbed_body: Option<String>,
    pub classification: LegacyStickyClass,
    pub claim_evidence: StickyClaimEvidence,
    pub requires_disposition: bool,
    pub disposition: StickyCutoverDisposition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StickyCutoverPlan {
    pub schema_version: u32,
    pub policy_version: String,
    pub target_db_identity: String,
    pub target_db_physical_identity: String,
    pub as_of: String,
    /// Explicit current admitted operator identity. Planning never infers it.
    pub issuer_agent_identity_id: Option<String>,
    /// Exact current connection for the operator identity. Planning never infers it.
    pub issuer_connection_id: Option<String>,
    pub rows: Vec<FrozenLegacyStickyRow>,
    pub planned_rows: usize,
    /// Hash of the frozen source census. Operator disposition fields are
    /// deliberately excluded because this is the editable plan artifact.
    pub plan_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StickyCutoverReceiptPhase {
    Prepared,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StickyCutoverReceiptRow {
    pub sticky_id: String,
    pub memory_id: Option<String>,
    pub classification: LegacyStickyClass,
    pub claim_evidence: StickyClaimEvidence,
    pub disposition: StickyCutoverDisposition,
    pub before_revision: Option<i64>,
    pub archived_revision: Option<i64>,
    pub source_row_digest: Option<String>,
    pub source_body_digest: Option<String>,
    pub outcome: String,
    pub envelope_id: Option<String>,
    pub envelope_body_digest: Option<String>,
    pub delivery_receipt_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StickyCutoverReceipt {
    pub schema_version: u32,
    pub policy_version: String,
    pub target_db_identity: String,
    pub target_db_physical_identity: String,
    pub plan_digest: String,
    pub decision_digest: String,
    pub applied_at: String,
    pub phase: StickyCutoverReceiptPhase,
    pub rows: Vec<StickyCutoverReceiptRow>,
    pub converted: usize,
    pub discarded: usize,
    pub archived_only: usize,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StickyCutoverApplyResult {
    pub receipt: StickyCutoverReceipt,
    pub converted: usize,
    pub discarded: usize,
    pub archived_only: usize,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct LegacyMemorySnapshot {
    id: String,
    path: String,
    text: String,
    timestamp: String,
    valid_until: Option<String>,
    archived: bool,
    revision: i64,
    metadata: String,
}

#[derive(Debug, Clone)]
struct ClaimRow {
    key: String,
    value_json: String,
    version: i64,
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn canonical(path: &str) -> Result<PathBuf, MemoryError> {
    std::fs::canonicalize(Path::new(path)).map_err(MemoryError::from)
}

fn normalize_timestamp(value: &str, field: &str) -> Result<String, MemoryError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| {
            value
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
        .map_err(|_| MemoryError::InvalidArg(format!("legacy sticky {field} is malformed")))
}

fn memory_row_digest(row: &LegacyMemorySnapshot) -> Result<String, MemoryError> {
    Ok(sha256(&serde_json::to_vec(row)?))
}

fn claim_evidence(claim: Option<&ClaimRow>) -> StickyClaimEvidence {
    let Some(claim) = claim else {
        return StickyClaimEvidence {
            state: StickyClaimEvidenceState::Missing,
            version: None,
            evidence_digest: sha256(b"missing"),
        };
    };
    let parsed = serde_json::from_str::<serde_json::Value>(&claim.value_json).ok();
    let object = parsed.as_ref().and_then(|value| value.as_object());
    let field = |name: &str| {
        object
            .and_then(|object| object.get(name))
            .and_then(|value| value.as_str())
    };
    let claimed_by = field("claimed_by");
    let claimed_at = field("claimed_at");
    let valid = claimed_by.is_some_and(|value| !value.trim().is_empty())
        && claimed_at.is_some_and(|value| DateTime::parse_from_rfc3339(value).is_ok());
    StickyClaimEvidence {
        state: if valid {
            StickyClaimEvidenceState::Valid
        } else {
            StickyClaimEvidenceState::Malformed
        },
        version: Some(claim.version),
        evidence_digest: sha256(claim.value_json.as_bytes()),
    }
}

fn read_legacy_memories(conn: &Connection) -> Result<Vec<LegacyMemorySnapshot>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT id,path,text,timestamp,valid_until,archived,revision,metadata
         FROM memories
         WHERE (category='sticky' OR id LIKE 'sticky:%'
                OR path='/sticky' OR path LIKE '/sticky/%')
         ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(LegacyMemorySnapshot {
                id: row.get(0)?,
                path: row.get(1)?,
                text: row.get(2)?,
                timestamp: row.get(3)?,
                valid_until: row.get(4)?,
                archived: row.get::<_, i64>(5)? != 0,
                revision: row.get(6)?,
                metadata: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn read_claims(conn: &Connection) -> Result<BTreeMap<String, ClaimRow>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT key,value_json,version
         FROM hard_state WHERE namespace='sticky_claim' ORDER BY key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ClaimRow {
            key: row.get(0)?,
            value_json: row.get(1)?,
            version: row.get(2)?,
        })
    })?;
    Ok(rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| (row.key.clone(), row))
        .collect())
}

fn sticky_id_from_snapshot(row: &LegacyMemorySnapshot) -> String {
    let metadata = serde_json::from_str::<serde_json::Value>(&row.metadata).ok();
    metadata
        .as_ref()
        .and_then(|value| value.get("sticky_id"))
        .and_then(|value| value.as_str())
        .or_else(|| {
            metadata
                .as_ref()
                .and_then(|value| value.pointer("/sticky/id"))
                .and_then(|value| value.as_str())
        })
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| row.id.strip_prefix("sticky:").map(str::to_string))
        .unwrap_or_else(|| row.id.clone())
}

fn analyze_memory(
    row: &LegacyMemorySnapshot,
    sticky_id: String,
    evidence: StickyClaimEvidence,
    as_of: DateTime<Utc>,
    scrubber: &mut impl FnMut(&str) -> String,
) -> Result<FrozenLegacyStickyRow, MemoryError> {
    let metadata = serde_json::from_str::<serde_json::Value>(&row.metadata).ok();
    let sticky = metadata.as_ref().and_then(|value| value.get("sticky"));
    let status = metadata
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str());
    let nested_id = sticky
        .and_then(|value| value.get("id"))
        .and_then(|value| value.as_str());
    let from_agent = sticky
        .and_then(|value| value.get("from_agent"))
        .and_then(|value| value.as_str());
    let body = sticky
        .and_then(|value| value.get("text"))
        .and_then(|value| value.as_str());
    let created_raw = sticky
        .and_then(|value| value.get("created_at"))
        .and_then(|value| value.as_str());
    let ttl_days = sticky
        .and_then(|value| value.get("ttl_days"))
        .and_then(|value| value.as_i64());
    let to_value = sticky.and_then(|value| value.get("to"));
    let malformed_legacy_recipient = matches!(
        to_value,
        Some(serde_json::Value::String(value)) if value.trim().is_empty()
    ) || matches!(
        to_value,
        Some(value) if !value.is_null() && !value.is_string()
    );

    let created = created_raw
        .and_then(|value| normalize_timestamp(value, "created_at").ok())
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc));
    let expires = created.zip(ttl_days).and_then(|(created, days)| {
        (days > 0)
            .then(|| created.checked_add_signed(Duration::days(days)))
            .flatten()
    });
    let corrupt = sticky.is_none()
        || nested_id != Some(sticky_id.as_str())
        || from_agent.is_none_or(|value| value.trim().is_empty())
        || body.is_none_or(|value| value.trim().is_empty() || value != row.text)
        || created.is_none()
        || expires.is_none()
        || ttl_days.is_none_or(|days| days <= 0)
        || malformed_legacy_recipient
        || !matches!(status, Some("unread" | "claimed" | "expired"));
    let live_classification = if corrupt || evidence.state == StickyClaimEvidenceState::Malformed {
        LegacyStickyClass::Corrupt
    } else if status == Some("claimed") || evidence.state == StickyClaimEvidenceState::Valid {
        LegacyStickyClass::Claimed
    } else if status == Some("expired") || expires.is_some_and(|expires| expires <= as_of) {
        LegacyStickyClass::Expired
    } else {
        LegacyStickyClass::Unread
    };
    let classification = if row.archived {
        match live_classification {
            LegacyStickyClass::Claimed => LegacyStickyClass::HistoricalClaimed,
            LegacyStickyClass::Expired => LegacyStickyClass::HistoricalExpired,
            LegacyStickyClass::Unread | LegacyStickyClass::Corrupt => {
                LegacyStickyClass::HistoricalCorrupt
            }
            LegacyStickyClass::HistoricalClaimed
            | LegacyStickyClass::HistoricalExpired
            | LegacyStickyClass::HistoricalCorrupt
            | LegacyStickyClass::CasOnly => unreachable!("live classification only"),
        }
    } else {
        live_classification
    };
    let requires_disposition = classification == LegacyStickyClass::Unread;
    Ok(FrozenLegacyStickyRow {
        sticky_id,
        memory_id: Some(row.id.clone()),
        memory_revision: Some(row.revision),
        memory_row_digest: Some(memory_row_digest(row)?),
        source_archived: row.archived,
        created_at: created.map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true)),
        expires_at: expires.map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true)),
        scrubbed_body: (classification == LegacyStickyClass::Unread)
            .then(|| scrubber(body.unwrap_or_default())),
        classification,
        claim_evidence: evidence,
        requires_disposition,
        disposition: if requires_disposition {
            StickyCutoverDisposition::Unresolved
        } else {
            StickyCutoverDisposition::ArchiveOnly
        },
    })
}

impl StickyCutoverPlan {
    pub fn compute_digest(&self) -> Result<String, MemoryError> {
        Ok(sha256(&serde_json::to_vec(
            &self.immutable_source_projection(),
        )?))
    }

    fn immutable_source_projection(&self) -> Self {
        let mut projection = self.clone();
        projection.plan_digest.clear();
        projection.issuer_agent_identity_id = None;
        projection.issuer_connection_id = None;
        for row in &mut projection.rows {
            row.disposition = if row.requires_disposition {
                StickyCutoverDisposition::Unresolved
            } else {
                StickyCutoverDisposition::ArchiveOnly
            };
        }
        projection
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != STICKY_CUTOVER_SCHEMA_VERSION
            || self.policy_version != STICKY_CUTOVER_POLICY
            || self.target_db_identity.trim().is_empty()
            || self.target_db_physical_identity.trim().is_empty()
            || self.planned_rows != self.rows.len()
            || !valid_sha256(&self.plan_digest)
            || self.plan_digest != self.compute_digest()?
        {
            return Err(MemoryError::InvalidArg(
                "sticky cutover plan schema, counts, or digest mismatch".to_string(),
            ));
        }
        normalize_timestamp(&self.as_of, "plan as_of")?;
        let mut ids = BTreeSet::new();
        for row in &self.rows {
            let historical = matches!(
                row.classification,
                LegacyStickyClass::HistoricalClaimed
                    | LegacyStickyClass::HistoricalExpired
                    | LegacyStickyClass::HistoricalCorrupt
            );
            if row.sticky_id.trim().is_empty() || !ids.insert(row.sticky_id.as_str()) {
                return Err(MemoryError::InvalidArg(
                    "sticky cutover plan has blank or duplicate sticky id".to_string(),
                ));
            }
            if row.requires_disposition != (row.classification == LegacyStickyClass::Unread)
                || row.memory_id.is_some() != row.memory_revision.is_some()
                || row.memory_id.is_some() != row.memory_row_digest.is_some()
                || row.classification == LegacyStickyClass::CasOnly && row.memory_id.is_some()
                || historical != row.source_archived
                || row.source_archived && row.memory_id.is_none()
            {
                return Err(MemoryError::InvalidArg(format!(
                    "sticky cutover frozen row is inconsistent: {}",
                    row.sticky_id
                )));
            }
        }
        Ok(())
    }

    fn decision_digest(&self) -> Result<String, MemoryError> {
        let decisions = serde_json::json!({
            "issuer_agent_identity_id": self.issuer_agent_identity_id,
            "issuer_connection_id": self.issuer_connection_id,
            "rows": self.rows.iter().map(|row| serde_json::json!({
                "sticky_id": row.sticky_id,
                "disposition": row.disposition,
            })).collect::<Vec<_>>(),
        });
        Ok(sha256(&serde_json::to_vec(&decisions)?))
    }
}

impl StickyCutoverReceipt {
    pub fn compute_digest(&self) -> Result<String, MemoryError> {
        let mut receipt = self.clone();
        receipt.receipt_digest.clear();
        Ok(sha256(&serde_json::to_vec(&receipt)?))
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != STICKY_CUTOVER_RECEIPT_SCHEMA_VERSION
            || self.policy_version != STICKY_CUTOVER_POLICY
            || !valid_sha256(&self.plan_digest)
            || !valid_sha256(&self.decision_digest)
            || !valid_sha256(&self.receipt_digest)
            || self.receipt_digest != self.compute_digest()?
            || self.rows.len() != self.converted + self.discarded + self.archived_only
        {
            return Err(MemoryError::InvalidArg(
                "sticky cutover receipt schema, counts, or digest mismatch".to_string(),
            ));
        }
        for row in &self.rows {
            let has_source = row.memory_id.is_some();
            if has_source != row.before_revision.is_some()
                || has_source != row.archived_revision.is_some()
                || has_source != row.source_row_digest.as_deref().is_some_and(valid_sha256)
                || has_source != row.source_body_digest.as_deref().is_some_and(valid_sha256)
            {
                return Err(MemoryError::InvalidArg(format!(
                    "sticky cutover receipt source evidence mismatch: {}",
                    row.sticky_id
                )));
            }
            let converted = matches!(row.disposition, StickyCutoverDisposition::Convert { .. });
            if converted
                != row
                    .envelope_id
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                || converted
                    != row
                        .envelope_body_digest
                        .as_deref()
                        .is_some_and(valid_sha256)
                || converted
                    != row
                        .delivery_receipt_id
                        .as_deref()
                        .is_some_and(|value| !value.is_empty())
            {
                return Err(MemoryError::InvalidArg(format!(
                    "sticky cutover receipt envelope evidence mismatch: {}",
                    row.sticky_id
                )));
            }
        }
        Ok(())
    }

    fn into_committed(mut self) -> Result<Self, MemoryError> {
        self.phase = StickyCutoverReceiptPhase::Committed;
        self.receipt_digest.clear();
        self.receipt_digest = self.compute_digest()?;
        Ok(self)
    }
}

fn read_memory_snapshot_by_id(
    conn: &Connection,
    memory_id: &str,
) -> Result<Option<LegacyMemorySnapshot>, MemoryError> {
    conn.query_row(
        "SELECT id,path,text,timestamp,valid_until,archived,revision,metadata
         FROM memories WHERE id=?1",
        [memory_id],
        |row| {
            Ok(LegacyMemorySnapshot {
                id: row.get(0)?,
                path: row.get(1)?,
                text: row.get(2)?,
                timestamp: row.get(3)?,
                valid_until: row.get(4)?,
                archived: row.get::<_, i64>(5)? != 0,
                revision: row.get(6)?,
                metadata: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(MemoryError::from)
}

fn read_claim_by_key(conn: &Connection, sticky_id: &str) -> Result<Option<ClaimRow>, MemoryError> {
    conn.query_row(
        "SELECT key,value_json,version
         FROM hard_state WHERE namespace='sticky_claim' AND key=?1",
        [sticky_id],
        |row| {
            Ok(ClaimRow {
                key: row.get(0)?,
                value_json: row.get(1)?,
                version: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(MemoryError::from)
}

fn freeze_sticky_cutover_source(
    conn: &Connection,
    target_db_identity: String,
    target_db_physical_identity: String,
    as_of: &str,
    scrubber: &mut impl FnMut(&str) -> String,
) -> Result<StickyCutoverPlan, MemoryError> {
    let as_of = normalize_timestamp(as_of, "plan as_of")?;
    let parsed_as_of = DateTime::parse_from_rfc3339(&as_of)
        .expect("normalized timestamp")
        .with_timezone(&Utc);
    let mut claims = read_claims(conn)?;
    let memories = read_legacy_memories(conn)?;
    let mut rows = Vec::with_capacity(memories.len() + claims.len());
    for memory in memories {
        let sticky_id = sticky_id_from_snapshot(&memory);
        let evidence = claim_evidence(claims.remove(&sticky_id).as_ref());
        rows.push(analyze_memory(
            &memory,
            sticky_id,
            evidence,
            parsed_as_of,
            scrubber,
        )?);
    }
    for (sticky_id, claim) in claims {
        rows.push(FrozenLegacyStickyRow {
            sticky_id,
            memory_id: None,
            memory_revision: None,
            memory_row_digest: None,
            source_archived: false,
            created_at: None,
            expires_at: None,
            scrubbed_body: None,
            classification: LegacyStickyClass::CasOnly,
            claim_evidence: claim_evidence(Some(&claim)),
            requires_disposition: false,
            disposition: StickyCutoverDisposition::ArchiveOnly,
        });
    }
    rows.sort_by(|left, right| {
        left.sticky_id
            .cmp(&right.sticky_id)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    let mut plan = StickyCutoverPlan {
        schema_version: STICKY_CUTOVER_SCHEMA_VERSION,
        policy_version: STICKY_CUTOVER_POLICY.to_string(),
        target_db_identity,
        target_db_physical_identity,
        as_of,
        issuer_agent_identity_id: None,
        issuer_connection_id: None,
        planned_rows: rows.len(),
        rows,
        plan_digest: String::new(),
    };
    plan.plan_digest = plan.compute_digest()?;
    plan.validate()?;
    Ok(plan)
}

fn archive_metadata(
    source: &LegacyMemorySnapshot,
    plan: &StickyCutoverPlan,
    row: &FrozenLegacyStickyRow,
    outcome: &str,
    envelope_id: Option<&str>,
) -> Result<String, MemoryError> {
    // The retired memory row must not become a second body-retention system.
    // Keep only closed cutover outcomes, typed source identity, and digests;
    // arbitrary legacy metadata is represented by its digest, never copied.
    let archive = serde_json::json!({
        "schema_version": STICKY_CUTOVER_SCHEMA_VERSION,
        "policy_version": STICKY_CUTOVER_POLICY,
        "plan_digest": plan.plan_digest,
        "sticky_id": row.sticky_id,
        "classification": row.classification,
        "outcome": outcome,
        "envelope_id": envelope_id,
        "source_evidence": {
            "memory_id": source.id,
            "path_digest": sha256(source.path.as_bytes()),
            "was_archived": source.archived,
            "revision": source.revision,
            "row_digest": row.memory_row_digest,
            "metadata_digest": sha256(source.metadata.as_bytes()),
            "body_digest": sha256(source.text.as_bytes()),
        },
        "claim_evidence": row.claim_evidence,
        "archived_at": plan.as_of,
    });
    serde_json::to_string(&serde_json::json!({ "legacy_sticky_archive": archive }))
        .map_err(MemoryError::from)
}

fn verify_frozen_row(
    tx: &Transaction<'_>,
    row: &FrozenLegacyStickyRow,
) -> Result<Option<LegacyMemorySnapshot>, MemoryError> {
    let actual_claim = claim_evidence(read_claim_by_key(tx, &row.sticky_id)?.as_ref());
    if actual_claim != row.claim_evidence {
        return Err(MemoryError::InvalidArg(format!(
            "sticky cutover claim CAS drift: {}",
            row.sticky_id
        )));
    }
    let Some(memory_id) = row.memory_id.as_deref() else {
        let legacy_exists: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM memories
                 WHERE (id=?1 OR json_extract(metadata,'$.sticky_id')=?2
                        OR json_extract(metadata,'$.sticky.id')=?2)
             )",
            params![format!("sticky:{}", row.sticky_id), row.sticky_id],
            |db_row| db_row.get(0),
        )?;
        if legacy_exists {
            return Err(MemoryError::InvalidArg(format!(
                "sticky cutover memory CAS drift: {} appeared",
                row.sticky_id
            )));
        }
        return Ok(None);
    };
    let source = read_memory_snapshot_by_id(tx, memory_id)?.ok_or_else(|| {
        MemoryError::InvalidArg(format!("sticky cutover memory CAS missing: {memory_id}"))
    })?;
    let expected_revision = row.memory_revision.expect("validated plan revision");
    let expected_digest = row
        .memory_row_digest
        .as_deref()
        .expect("validated plan row digest");
    if source.archived != row.source_archived
        || source.revision != expected_revision
        || memory_row_digest(&source)? != expected_digest
    {
        return Err(MemoryError::InvalidArg(format!(
            "sticky cutover memory CAS drift: {memory_id}"
        )));
    }
    Ok(Some(source))
}

fn receipt_from_db(
    conn: &Connection,
    plan_digest: &str,
) -> Result<Option<StickyCutoverReceipt>, MemoryError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value_json FROM hard_state WHERE namespace=?1 AND key=?2",
            params![RECEIPT_NAMESPACE, plan_digest],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| {
        let receipt: StickyCutoverReceipt = serde_json::from_str(&raw)?;
        receipt.validate()?;
        if receipt.phase != StickyCutoverReceiptPhase::Committed {
            return Err(MemoryError::InvalidArg(
                "sticky cutover DB receipt is not committed".to_string(),
            ));
        }
        Ok(receipt)
    })
    .transpose()
}

impl MemoryStore {
    /// Build the read-only operator artifact for the retired sticky surface.
    /// `as_of` is captured once by the caller and persisted in the plan.
    pub fn plan_sticky_cutover_at(
        &self,
        target_db_identity: String,
        as_of: &str,
        mut scrubber: impl FnMut(&str) -> String,
    ) -> Result<StickyCutoverPlan, MemoryError> {
        if !self.profile.includes_product() {
            return Err(MemoryError::InvalidArg(
                "sticky cutover requires a Product database".to_string(),
            ));
        }
        let target_db_physical_identity =
            self.opened_physical_db_identity.clone().ok_or_else(|| {
                MemoryError::InvalidArg(
                    "sticky cutover requires a file-backed physical DB identity".to_string(),
                )
            })?;
        let effective: String = self.conn.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |row| row.get(0),
        )?;
        if canonical(&effective)? != canonical(&target_db_identity)? {
            return Err(MemoryError::InvalidArg(
                "sticky cutover plan target DB mismatch".to_string(),
            ));
        }
        self.verify_opened_physical_db_identity(Path::new(&target_db_identity))?;
        freeze_sticky_cutover_source(
            &self.conn,
            target_db_identity,
            target_db_physical_identity,
            as_of,
            &mut scrubber,
        )
    }

    fn validate_sticky_cutover_target(&self, plan: &StickyCutoverPlan) -> Result<(), MemoryError> {
        let effective: String = self.conn.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |row| row.get(0),
        )?;
        if canonical(&effective)? != canonical(&plan.target_db_identity)?
            || self.opened_physical_db_identity.as_deref()
                != Some(plan.target_db_physical_identity.as_str())
        {
            return Err(MemoryError::InvalidArg(
                "sticky cutover physical DB identity mismatch".to_string(),
            ));
        }
        self.verify_opened_physical_db_identity(Path::new(&plan.target_db_identity))?;
        Ok(())
    }

    /// Apply one frozen operator plan. The callback must durably publish the
    /// prepared filesystem receipt before this method is allowed to commit.
    pub fn apply_sticky_cutover_with_precommit_receipt(
        &mut self,
        plan: &StickyCutoverPlan,
        mut scrubber: impl FnMut(&str) -> String,
        precommit_receipt: impl FnOnce(&StickyCutoverApplyResult) -> Result<(), MemoryError>,
    ) -> Result<StickyCutoverApplyResult, MemoryError> {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        plan.validate()?;
        if !self.profile.includes_product() {
            return Err(MemoryError::InvalidArg(
                "sticky cutover requires a Product database".to_string(),
            ));
        }
        self.validate_sticky_cutover_target(plan)?;
        let decision_digest = plan.decision_digest()?;
        let conversion_present = plan
            .rows
            .iter()
            .any(|row| matches!(row.disposition, StickyCutoverDisposition::Convert { .. }));
        let issuer_identity = plan
            .issuer_agent_identity_id
            .as_deref()
            .filter(|value| !value.trim().is_empty());
        let issuer_connection = plan
            .issuer_connection_id
            .as_deref()
            .filter(|value| !value.trim().is_empty());
        if conversion_present && (issuer_identity.is_none() || issuer_connection.is_none()) {
            return Err(MemoryError::InvalidArg(
                "sticky cutover conversion requires explicit operator AgentIdentity and current connection"
                    .to_string(),
            ));
        }
        for row in &plan.rows {
            match (&row.classification, &row.disposition) {
                (
                    LegacyStickyClass::Unread,
                    StickyCutoverDisposition::Convert {
                        recipient_agent_identity_id,
                    },
                ) if !recipient_agent_identity_id.trim().is_empty() => {}
                (LegacyStickyClass::Unread, StickyCutoverDisposition::Discard { reason })
                    if !reason.trim().is_empty() => {}
                (LegacyStickyClass::Unread, StickyCutoverDisposition::Unresolved) => {
                    return Err(MemoryError::InvalidArg(format!(
                        "sticky cutover unresolved unread row: {}",
                        row.sticky_id
                    )));
                }
                (LegacyStickyClass::Unread, _) => {
                    return Err(MemoryError::InvalidArg(format!(
                        "sticky cutover unread row requires explicit AgentIdentity mapping or discard: {}",
                        row.sticky_id
                    )));
                }
                (_, StickyCutoverDisposition::ArchiveOnly) => {}
                _ => {
                    return Err(MemoryError::InvalidArg(format!(
                        "sticky cutover non-unread row cannot be converted or discarded: {}",
                        row.sticky_id
                    )));
                }
            }
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = receipt_from_db(&tx, &plan.plan_digest)? {
            if receipt.decision_digest != decision_digest {
                return Err(MemoryError::InvalidArg(
                    "sticky cutover replay decision mismatch".to_string(),
                ));
            }
            let result = StickyCutoverApplyResult {
                converted: receipt.converted,
                discarded: receipt.discarded,
                archived_only: receipt.archived_only,
                receipt,
                replayed: true,
            };
            tx.commit()?;
            return Ok(result);
        }

        let current_source = freeze_sticky_cutover_source(
            &tx,
            plan.target_db_identity.clone(),
            plan.target_db_physical_identity.clone(),
            &plan.as_of,
            &mut scrubber,
        )?;

        let mut sources = BTreeMap::new();
        for row in &plan.rows {
            if let Some(source) = verify_frozen_row(&tx, row)? {
                sources.insert(row.sticky_id.clone(), source);
            }
        }
        if plan.immutable_source_projection() != current_source.immutable_source_projection() {
            return Err(MemoryError::InvalidArg(
                "sticky cutover source census drift".to_string(),
            ));
        }
        apply_sticky_cutover_in_tx(
            tx,
            plan,
            decision_digest,
            issuer_identity,
            issuer_connection,
            sources,
            precommit_receipt,
        )
    }
}

fn apply_sticky_cutover_in_tx(
    tx: Transaction<'_>,
    plan: &StickyCutoverPlan,
    decision_digest: String,
    issuer_identity: Option<&str>,
    issuer_connection: Option<&str>,
    sources: BTreeMap<String, LegacyMemorySnapshot>,
    precommit_receipt: impl FnOnce(&StickyCutoverApplyResult) -> Result<(), MemoryError>,
) -> Result<StickyCutoverApplyResult, MemoryError> {
    let mut receipt_rows = Vec::with_capacity(plan.rows.len());
    let mut converted = 0usize;
    let mut discarded = 0usize;
    let mut archived_only = 0usize;
    for row in &plan.rows {
        let source = sources.get(&row.sticky_id);
        let (outcome, envelope_id, envelope_body_digest, delivery_receipt_id) = match &row
            .disposition
        {
            StickyCutoverDisposition::Convert {
                recipient_agent_identity_id,
            } => {
                let request = NewA2aEnvelope {
                    envelope_id: format!("legacy-sticky:{}", sha256(row.sticky_id.as_bytes())),
                    issuer_agent_identity_id: issuer_identity
                        .expect("validated conversion issuer")
                        .to_string(),
                    issuer_connection_id: issuer_connection
                        .expect("validated conversion connection")
                        .to_string(),
                    recipient_agent_identity_id: recipient_agent_identity_id.clone(),
                    subject_ref: format!("a2a_envelope:legacy-sticky:{}", row.sticky_id),
                    body: row.scrubbed_body.clone().ok_or_else(|| {
                        MemoryError::InvalidArg(format!(
                            "sticky cutover unread body unavailable: {}",
                            row.sticky_id
                        ))
                    })?,
                    idempotency_key: format!("legacy-sticky:{}", row.sticky_id),
                    created_at: row.created_at.clone().ok_or_else(|| {
                        MemoryError::InvalidArg(format!(
                            "sticky cutover created_at unavailable: {}",
                            row.sticky_id
                        ))
                    })?,
                    expires_at: row.expires_at.clone().ok_or_else(|| {
                        MemoryError::InvalidArg(format!(
                            "sticky cutover expires_at unavailable: {}",
                            row.sticky_id
                        ))
                    })?,
                };
                let (envelope, delivery_receipt) = match insert_a2a_envelope_in_tx(&tx, &request)? {
                    A2aInsertOutcome::Created { envelope, receipt }
                    | A2aInsertOutcome::Replay { envelope, receipt } => (envelope, receipt),
                };
                converted += 1;
                (
                    "converted",
                    Some(envelope.envelope_id),
                    Some(envelope.body_digest),
                    Some(delivery_receipt.receipt_id),
                )
            }
            StickyCutoverDisposition::Discard { .. } => {
                discarded += 1;
                ("discarded", None, None, None)
            }
            StickyCutoverDisposition::ArchiveOnly => {
                archived_only += 1;
                ("archived_only", None, None, None)
            }
            StickyCutoverDisposition::Unresolved => unreachable!("validated disposition"),
        };
        let archived_revision = if let Some(source) = source {
            let metadata = archive_metadata(source, plan, row, outcome, envelope_id.as_deref())?;
            let changed = tx.execute(
                "UPDATE memories
                 SET archived=1,valid_until=COALESCE(valid_until,?1),updated_at=?1,
                     revision=revision+1,metadata=?2,text=''
                 WHERE id=?3 AND revision=?4 AND archived=?5 AND metadata=?6 AND text=?7",
                params![
                    plan.as_of,
                    metadata,
                    source.id,
                    source.revision,
                    i64::from(source.archived),
                    source.metadata,
                    source.text
                ],
            )?;
            if changed != 1 {
                return Err(MemoryError::InvalidArg(format!(
                    "sticky cutover archive CAS drift: {}",
                    row.sticky_id
                )));
            }
            Some(source.revision + 1)
        } else {
            None
        };
        receipt_rows.push(StickyCutoverReceiptRow {
            sticky_id: row.sticky_id.clone(),
            memory_id: row.memory_id.clone(),
            classification: row.classification,
            claim_evidence: row.claim_evidence.clone(),
            disposition: row.disposition.clone(),
            before_revision: row.memory_revision,
            archived_revision,
            source_row_digest: row.memory_row_digest.clone(),
            source_body_digest: source.map(|source| sha256(source.text.as_bytes())),
            outcome: outcome.to_string(),
            envelope_id,
            envelope_body_digest,
            delivery_receipt_id,
        });
    }

    let mut prepared_receipt = StickyCutoverReceipt {
        schema_version: STICKY_CUTOVER_RECEIPT_SCHEMA_VERSION,
        policy_version: STICKY_CUTOVER_POLICY.to_string(),
        target_db_identity: plan.target_db_identity.clone(),
        target_db_physical_identity: plan.target_db_physical_identity.clone(),
        plan_digest: plan.plan_digest.clone(),
        decision_digest,
        applied_at: plan.as_of.clone(),
        phase: StickyCutoverReceiptPhase::Prepared,
        rows: receipt_rows,
        converted,
        discarded,
        archived_only,
        receipt_digest: String::new(),
    };
    prepared_receipt.receipt_digest = prepared_receipt.compute_digest()?;
    prepared_receipt.validate()?;
    let prepared_result = StickyCutoverApplyResult {
        receipt: prepared_receipt.clone(),
        converted,
        discarded,
        archived_only,
        replayed: false,
    };
    precommit_receipt(&prepared_result)?;

    let committed_receipt = prepared_receipt.into_committed()?;
    tx.execute(
        "INSERT INTO hard_state(namespace,key,value_json,version,created_at,updated_at)
         VALUES (?1,?2,?3,1,?4,?4)",
        params![
            RECEIPT_NAMESPACE,
            plan.plan_digest,
            serde_json::to_string(&committed_receipt)?,
            plan.as_of
        ],
    )?;
    tx.commit()?;
    Ok(StickyCutoverApplyResult {
        receipt: committed_receipt,
        converted,
        discarded,
        archived_only,
        replayed: false,
    })
}
