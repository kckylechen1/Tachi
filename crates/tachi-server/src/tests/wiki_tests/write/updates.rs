use super::*;
use crate::tool_params::SaveMemoryParams;

#[test]
fn raw_memory_connection_cannot_promote_rows_into_trusted_namespaces() {
    let cases = [
        ("wiki-path", "UPDATE memories SET path = '/wiki/agent/forged' WHERE id = 'wiki-path'"),
        ("wiki-root", "UPDATE memories SET path = '/wiki' WHERE id = 'wiki-root'"),
        ("wiki-path-null-domain", "UPDATE memories SET path = '/wiki/agent/null-domain', domain = NULL WHERE id = 'wiki-path-null-domain'"),
        ("wiki-source-alias", "UPDATE memories SET source = 'WiKi' WHERE id = 'wiki-source-alias'"),
        ("wiki-category-alias", "UPDATE memories SET category = 'WiKi' WHERE id = 'wiki-category-alias'"),
        ("wiki-domain-alias", "UPDATE memories SET domain = 'WiKi' WHERE id = 'wiki-domain-alias'"),
        ("guide-category", "UPDATE memories SET category = 'guide' WHERE id = 'guide-category'"),
        ("precedent-path", "UPDATE memories SET path = '/precedents/project/forged' WHERE id = 'precedent-path'"),
        ("precedent-candidate-path", "UPDATE memories SET path = '/precedent_candidates/project/forged' WHERE id = 'precedent-candidate-path'"),
        ("precedent-domain", "UPDATE memories SET domain = 'precedent' WHERE id = 'precedent-domain'"),
        ("recall-cache-topic", "UPDATE memories SET topic = 'recall_rerank_cache' WHERE id = 'recall-cache-topic'"),
    ];
    // Each case needs its own body. `make_entry` gives every row the same
    // text, and memcore's write-time near-duplicate merge (token-Jaccard
    // > 0.9, `memcore::db::memory_crud::merge_into_jaccard_candidate`) folds
    // identical bodies into the first row, stamping the other ten
    // `superseded_by` before this test ever runs. `MemoryStore::get` still
    // returns superseded rows, so the assertions below stayed green while
    // ten of the eleven raw-SQL promotion attacks were aimed at dead rows —
    // i.e. the trust boundary was only ever exercised once. Distinct bodies
    // keep all eleven rows live so each attack hits a real classifier-bearing
    // row.
    let entries = cases
        .iter()
        .map(|(id, _)| {
            let mut entry = make_entry(id);
            entry.text = format!("classifier attack case {id}");
            entry
        })
        .collect::<Vec<_>>();
    let (server, _home) = seed_wiki_project_entries(entries);

    server
        .with_named_project_store("wiki", |store| {
            for (id, sql) in cases {
                let before = store.get(id).map_err(|error| error.to_string())?.unwrap();
                let write = store.connection().execute(sql, []);
                let after = store.get(id).map_err(|error| error.to_string())?.unwrap();
                let lifecycle = crate::tool_params::derive_wiki_lifecycle(
                    &after.metadata,
                    &after.path,
                );
                assert!(
                    write.is_err(),
                    "raw classifier mutation succeeded: id={id}, path={}, source={}, category={}, domain={:?}, topic={}, wiki={}, lifecycle={}",
                    after.path,
                    after.source,
                    after.category,
                    after.domain,
                    after.topic,
                    memcore::is_wiki_entry(&after),
                    lifecycle.as_str(),
                );
                assert_eq!(after.path, before.path, "path changed for {id}");
                assert_eq!(after.source, before.source, "source changed for {id}");
                assert_eq!(after.category, before.category, "category changed for {id}");
                assert_eq!(after.domain, before.domain, "domain changed for {id}");
                assert_eq!(after.topic, before.topic, "topic changed for {id}");
            }

            for sql in [
                "INSERT INTO memories (id, text, timestamp) VALUES ('raw-insert-missing', 'forged', '2026-07-26T00:00:00Z')",
                "INSERT INTO memories (id, path, text, timestamp, domain) VALUES ('raw-insert-null', '/wiki/agent/null', 'forged', '2026-07-26T00:00:00Z', NULL)",
            ] {
                assert!(
                    store.connection().execute(sql, []).is_err(),
                    "raw insert entered a trusted namespace: {sql}"
                );
            }
            Ok(())
        })
        .expect("exercise raw classifier attacks");
}

#[tokio::test]
async fn tachi_wiki_write_update_tombstones_legacy_source_refs() {
    let mut legacy = make_entry("legacy-wiki-row");
    legacy.path = "/wiki/agent/tachi/legacy-refs".to_string();
    legacy.topic = "legacy-refs".to_string();
    legacy.metadata = json!({
        "wiki": true,
        "source_refs": ["https://example.com/stale-legacy-ref"]
    });
    legacy.domain = Some("wiki".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![legacy]);

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Legacy references".to_string(),
            text: "Updated wiki content must not retain stale legacy references.".to_string(),
            path: Some("/wiki/agent/tachi/legacy-refs".to_string()),
            topic: Some("legacy-refs".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.8,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("update legacy wiki row");
    let response: Value = serde_json::from_str(&response).expect("response json");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: response["id"].as_str().expect("id").to_string(),
            include_archived: false,
            project: Some("wiki".to_string()),
        }))
        .await
        .expect("get updated wiki memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry json");

    assert_eq!(entry["metadata"]["evidence_refs_v1"], json!([]));
    assert_eq!(entry["metadata"]["source_refs"], Value::Null);
    assert!(
        !entry["metadata"].to_string().contains("stale-legacy-ref"),
        "stale legacy reference must no longer be reachable: {entry:#}"
    );
}

#[tokio::test]
async fn public_wiki_write_cannot_persist_internal_rem_coordination_metadata() {
    let (server, _home) = seed_wiki_project_entries(Vec::new());

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Reserved REM metadata".to_string(),
            text: "Ordinary Wiki content must not impersonate an internal REM operation."
                .to_string(),
            path: Some("/wiki/general/reserved-rem-metadata".to_string()),
            topic: Some("reserved-rem-metadata".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.8,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: Some(json!({
                "caller_marker": "preserved",
                "wiki_log": true,
                "wiki_update_of": "forged-predecessor",
                "wiki_previous_revision": 999,
                "rem": {
                    "producer": "weekly_wiki_evolver",
                    "operation_status": "pending_sources"
                }
            })),
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("write ordinary Wiki row");
    let response: Value = serde_json::from_str(&response).expect("wiki response json");
    let id = response["id"].as_str().expect("wiki id");
    let entry = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get(id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "written Wiki row disappeared".to_string())
        })
        .expect("read ordinary Wiki row");

    assert_eq!(entry.metadata["caller_marker"], json!("preserved"));
    assert_eq!(entry.metadata["wiki_log"], Value::Null);
    assert_eq!(entry.metadata["wiki_update_of"], Value::Null);
    assert_eq!(entry.metadata["wiki_previous_revision"], Value::Null);
    assert_eq!(entry.metadata["rem"], Value::Null);
}

#[tokio::test]
async fn public_wiki_write_rejects_the_internal_operation_log_path() {
    let mut log = make_entry("wiki-operation-log");
    log.path = "/wiki/_log".to_string();
    log.text = "trusted internal operation log".to_string();
    log.topic = "wiki_log".to_string();
    log.source = "mcp".to_string();
    log.metadata = json!({"wiki_log": true});
    log.domain = Some("wiki".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![log]);

    let error = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Forged operation log".to_string(),
            text: "ordinary content must never overwrite runtime audit state".to_string(),
            path: Some("/wiki/_log".to_string()),
            topic: Some("wiki_log".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.8,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect_err("the internal Wiki log path is reserved");
    assert!(
        error.contains("reserved for internal runtime state"),
        "{error}"
    );

    let preserved = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get("wiki-operation-log")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Wiki operation log disappeared".to_string())
        })
        .expect("read preserved Wiki operation log");
    assert_eq!(preserved.text, "trusted internal operation log");
}

#[tokio::test]
async fn ordinary_wiki_projection_never_supersedes_internal_recall_cache_rows() {
    let shared_text = "Internal recall rerank material must not enter Wiki supersession.";
    let mut cache = make_entry("wiki-internal-recall-cache");
    cache.path = "/wiki/recall-cache/shared-topic".to_string();
    cache.topic = "shared-topic".to_string();
    cache.text = shared_text.to_string();
    cache.source = "foundry_recall_rerank_cache".to_string();
    cache.metadata = json!({"recall_cache": true});
    cache.domain = Some("wiki".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![cache]);

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Shared topic".to_string(),
            text: shared_text.to_string(),
            path: Some("/wiki/general/shared-topic".to_string()),
            topic: Some("shared-topic".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.8,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("write ordinary Wiki row beside internal recall cache");
    let response: Value = serde_json::from_str(&response).expect("Wiki response JSON");
    assert_eq!(response["wiki_write_mode"], "created");
    assert_eq!(response["wiki_duplicates_superseded"], 0);

    let superseded_by: Option<String> = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = 'wiki-internal-recall-cache'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("read recall-cache supersession state");
    assert_eq!(superseded_by, None);
}

#[cfg(unix)]
#[tokio::test]
async fn ordinary_wiki_write_rejects_a_cached_store_after_its_path_is_replaced() {
    let (server, home) = seed_wiki_project_entries(Vec::new());
    let wiki_db = home.temp_home.join(".tachi/projects/wiki/memory.db");
    let replacement = home.temp_home.join("replacement-wiki.db");
    drop(
        MemoryStore::open(replacement.to_str().expect("UTF-8 replacement path"))
            .expect("initialize replacement Wiki DB"),
    );
    std::fs::rename(&replacement, &wiki_db).expect("replace cached Wiki DB path");

    let error = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Detached store".to_string(),
            text: "A detached cached connection must never report this write as durable."
                .to_string(),
            path: Some("/wiki/general/detached-store".to_string()),
            topic: Some("detached-store".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.8,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect_err("cached detached Wiki handle must fail closed");
    assert!(error.contains("physical identity check failed"), "{error}");
}

#[tokio::test]
async fn generic_save_cannot_mint_a_wiki_rem_operation_row() {
    let (server, _home) = seed_wiki_project_entries(Vec::new());
    let error = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "A generic save must not mint an internal REM operation.".to_string(),
            summary: String::new(),
            path: "/wiki/drafts/generic-rem-spoof".to_string(),
            importance: 0.7,
            category: "experience".to_string(),
            topic: "generic-rem-spoof".to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: Some("Wiki-Rem:generic-spoof".to_string()),
            force: true,
            auto_link: false,
            project: Some("wiki".to_string()),
            project_explicit: true,
            retention_policy: Some("permanent".to_string()),
            domain: Some("wiki".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({
                "rem": {
                    "producer": "weekly_wiki_evolver",
                    "operation_status": "pending_sources",
                    "sources": []
                }
            })),
            emit_continuity: false,
        }))
        .await
        .expect_err("generic save must reject the internal REM id namespace");

    assert!(error.contains("reserved 'wiki-rem:' namespace"), "{error}");
}

#[tokio::test]
async fn ordinary_wiki_write_ignores_exact_matching_rem_draft() {
    let (server, _home) = seed_wiki_project_entries(Vec::new());
    let mut rem_draft = make_entry("wiki-rem:exact-duplicate");
    rem_draft.path = "/wiki/drafts/exact-duplicate-boundary".to_string();
    rem_draft.topic = "exact-duplicate-boundary".to_string();
    rem_draft.text = "An internal REM draft is not an ordinary Wiki duplicate winner.".to_string();
    rem_draft.source = "wiki".to_string();
    rem_draft.metadata = json!({
        "wiki": true,
        "artifact_kind": "draft",
        "lifecycle": "pending_review",
        "rem": {
            "producer": "weekly_wiki_evolver",
            "operation_id": rem_draft.id.clone(),
            "operation_status": "pending_sources"
        }
    });
    server
        .with_named_project_store("wiki", |store| {
            store
                .with_immutable_supersession_transaction(|operation| {
                    operation
                        .insert_rem_operation_if_absent(&rem_draft)
                        .map(|_| ())
                })
                .map_err(|error| error.to_string())
        })
        .expect("seed internal REM draft");

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Ordinary exact duplicate boundary".to_string(),
            text: rem_draft.text.clone(),
            path: Some(rem_draft.path.clone()),
            topic: Some(rem_draft.topic.clone()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.8,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("write ordinary Wiki row beside REM draft");
    let response: Value = serde_json::from_str(&response).expect("wiki response JSON");

    assert_eq!(response["wiki_write_mode"], json!("created"));
    assert_ne!(response["id"], json!(rem_draft.id));
    assert!(response["status"]
        .as_str()
        .is_some_and(|status| status.starts_with("saved")));
}

#[tokio::test]
async fn generic_idless_save_cannot_select_or_mutate_a_matching_rem_operation() {
    let (server, _home) = seed_wiki_project_entries(Vec::new());
    let mut rem_draft = make_entry("wiki-rem:generic-duplicate-boundary");
    rem_draft.path = "/wiki/drafts/generic-duplicate-boundary".to_string();
    rem_draft.topic = "generic-duplicate-boundary".to_string();
    rem_draft.text = "An internal REM operation cannot win generic save deduplication.".to_string();
    rem_draft.source = "wiki".to_string();
    rem_draft.metadata = json!({
        "artifact_kind": "draft",
        "review_status": "pending",
        "rem": {
            "producer": "weekly_wiki_evolver",
            "operation_id": rem_draft.id.clone(),
            "operation_status": "pending_sources"
        }
    });
    server
        .with_named_project_store("wiki", |store| {
            store
                .with_immutable_supersession_transaction(|operation| {
                    operation
                        .insert_rem_operation_if_absent(&rem_draft)
                        .map(|_| ())
                })
                .map_err(|error| error.to_string())
        })
        .expect("seed internal REM operation");

    let response = server
        .save_memory(Parameters(SaveMemoryParams {
            text: rem_draft.text.clone(),
            summary: String::new(),
            path: rem_draft.path.clone(),
            importance: 0.7,
            category: "experience".to_string(),
            topic: rem_draft.topic.clone(),
            keywords: vec!["generic".to_string()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: Some("wiki".to_string()),
            project_explicit: true,
            retention_policy: Some("permanent".to_string()),
            domain: Some("wiki".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
        }))
        .await
        .expect("generic save must create an ordinary row");
    let response: Value = serde_json::from_str(&response).expect("save response json");
    assert_ne!(response["status"], "duplicate");
    assert_ne!(response["id"], rem_draft.id);

    let stored_rem = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get(&rem_draft.id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "REM operation disappeared".to_string())
        })
        .expect("read REM operation");
    assert!(stored_rem.keywords.is_empty());
    assert_eq!(
        stored_rem.metadata["rem"]["operation_status"],
        "pending_sources"
    );
}

#[tokio::test]
async fn tachi_wiki_write_updates_existing_path_in_place() {
    let (server, _home) = seed_wiki_project_entries(Vec::new());

    let first = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "TrendLock Rule".to_string(),
            text: "TrendLock should protect a live trend from premature exits when the validation rule still holds.".to_string(),
            path: Some("/wiki/agent/tachi/trendlock".to_string()),
            topic: Some("trendlock".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["trendlock".to_string()],
            entities: vec!["TrendLock".to_string()],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("first wiki write");
    let first_json: Value = serde_json::from_str(&first).expect("first json");
    let first_id = first_json["id"].as_str().expect("first id").to_string();

    let second = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "TrendLock Rule".to_string(),
            text: "The revised canonical policy uses deliberately different wording.".to_string(),
            path: Some("/wiki//agent/tachi/trendlock/".to_string()),
            topic: Some("trendlock revised".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["trendlock".to_string()],
            entities: vec!["TrendLock".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("second wiki write");
    let second_json: Value = serde_json::from_str(&second).expect("second json");

    assert_eq!(second_json["id"], json!(first_id));
    assert_eq!(second_json["wiki_write_mode"], json!("updated"));

    let active_count: i64 = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE path = '/wiki/agent/tachi/trendlock' AND archived = 0 AND superseded_by IS NULL",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())
        })
        .expect("count active wiki rows");
    assert_eq!(active_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_exact_path_updates_stamp_the_immediate_predecessor_revision() {
    let mut canonical = make_entry("concurrent-update-lineage");
    canonical.path = "/wiki/general/concurrent-update-lineage".to_string();
    canonical.topic = "concurrent update lineage".to_string();
    canonical.metadata = json!({"wiki": true});
    canonical.domain = Some("wiki".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![canonical.clone()]);
    let writer_one = MemoryServer::new(server.global_db_path_buf(), server.project_db_path_buf())
        .expect("open first Wiki update writer");
    let writer_two = MemoryServer::new(server.global_db_path_buf(), server.project_db_path_buf())
        .expect("open second Wiki update writer");
    let _barrier = crate::memory_search_ops::save_memory::install_pre_upsert_path_barrier(
        &canonical.path,
        std::sync::Arc::new(std::sync::Barrier::new(2)),
    );

    let write = |writer: MemoryServer, title: &'static str, text: &'static str| {
        let path = canonical.path.clone();
        tokio::spawn(async move {
            writer
                .tachi_wiki_write(Parameters(WikiWriteParams {
                    title: title.to_string(),
                    text: text.to_string(),
                    path: Some(path),
                    topic: Some("concurrent update lineage".to_string()),
                    summary: None,
                    category: "experience".to_string(),
                    keywords: vec![],
                    entities: vec![],
                    importance: 0.8,
                    scope: "global".to_string(),
                    retention_policy: "permanent".to_string(),
                    domain: None,
                    project: None,
                    metadata: None,
                    force: true,
                    references: vec![],
                    include_patterns: false,
                    pattern_query: None,
                    pattern_top_k: None,
                }))
                .await
        })
    };
    let first = write(
        writer_one,
        "Concurrent update A",
        "First concurrent writer updates the same canonical Wiki path.",
    );
    let second = write(
        writer_two,
        "Concurrent update B",
        "Second concurrent writer updates the same canonical Wiki path.",
    );
    let mut responses = Vec::new();
    for task in [first, second] {
        let raw = task
            .await
            .expect("Wiki update writer task")
            .expect("Wiki update writer response");
        responses.push(serde_json::from_str::<Value>(&raw).expect("Wiki update response JSON"));
    }
    let mut previous_revisions = responses
        .iter()
        .map(|response| {
            response["wiki_previous_revision"]
                .as_i64()
                .expect("transaction-derived predecessor revision")
        })
        .collect::<Vec<_>>();
    previous_revisions.sort_unstable();
    assert_eq!(previous_revisions, vec![1, 2]);
    assert!(responses
        .iter()
        .all(|response| response["wiki_write_mode"] == "updated"));

    let stored = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get(&canonical.id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "canonical Wiki row disappeared".to_string())
        })
        .expect("read final canonical Wiki row");
    assert_eq!(stored.revision, 3);
    assert_eq!(stored.metadata["wiki_update_of"], canonical.id);
    assert_eq!(stored.metadata["wiki_previous_revision"], 2);
}

#[tokio::test]
async fn tachi_wiki_write_does_not_relocate_a_same_topic_cross_parent_row() {
    let mut ops = make_entry("ops-mcp");
    ops.path = "/wiki/ops/mcp".to_string();
    ops.topic = "mcp".to_string();
    ops.text = "Operations-specific MCP deployment guidance.".to_string();
    ops.metadata = json!({"wiki": true});
    ops.domain = Some("wiki".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![ops.clone()]);

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "MCP Engineering Contract".to_string(),
            text: "Engineering-specific MCP protocol and schema guidance.".to_string(),
            path: Some("/wiki/engineering/mcp".to_string()),
            topic: Some("mcp".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["mcp".to_string()],
            entities: vec!["MCP".to_string()],
            importance: 0.8,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("write cross-parent same-topic Wiki row");
    let response: Value = serde_json::from_str(&response).expect("wiki response JSON");

    assert_ne!(response["id"], ops.id);
    assert_eq!(response["wiki_write_mode"], "created");
    let preserved = server
        .with_named_project_store_read("wiki", |store| {
            store.get(&ops.id).map_err(|error| error.to_string())
        })
        .expect("read original row")
        .expect("original row remains");
    assert_eq!(preserved.path, "/wiki/ops/mcp");
    assert_eq!(preserved.text, ops.text);
}

#[tokio::test]
async fn tachi_wiki_write_supersedes_duplicate_topic_rows() {
    let mut canonical = make_entry("canonical-trendlock-row");
    canonical.path = "/wiki/agent/tachi/trendlock".to_string();
    canonical.topic = "trendlock".to_string();
    canonical.text = "TrendLock canonical base rule.".to_string();
    canonical.summary = "TrendLock".to_string();
    canonical.metadata = json!({"wiki": true});
    canonical.domain = Some("wiki".to_string());

    let mut duplicate = make_entry("duplicate-trendlock-row");
    duplicate.path = "/wiki/agent/tachi/trendlock-copy".to_string();
    duplicate.topic = "trendlock-copy".to_string();
    duplicate.text =
        "TrendLock canonical rule should replace older same-topic wiki entries.".to_string();
    duplicate.summary = "Duplicate TrendLock".to_string();
    duplicate.metadata = json!({"wiki": true});
    duplicate.domain = Some("wiki".to_string());

    let (server, _home) = seed_wiki_project_entries(vec![canonical, duplicate]);

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "TrendLock Canonical".to_string(),
            text: "TrendLock canonical rule should replace older same-topic wiki entries."
                .to_string(),
            path: Some("/wiki/agent/tachi/trendlock".to_string()),
            topic: Some("trendlock".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["trendlock".to_string()],
            entities: vec!["TrendLock".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("wiki write should supersede duplicate");
    let json: Value = serde_json::from_str(&response).expect("write json");
    assert_eq!(json["id"], json!("canonical-trendlock-row"));
    assert_eq!(json["wiki_duplicates_superseded"], json!(1));
    let superseded_by: Option<String> = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = 'duplicate-trendlock-row'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())
        })
        .expect("read superseded_by");
    assert_eq!(superseded_by, Some("canonical-trendlock-row".to_string()));
    assert_eq!(json["wiki_write_mode"], json!("updated"));
}

#[tokio::test]
async fn idless_projection_replay_reconciles_stale_active_duplicates() {
    let (server, _home) = seed_wiki_project_entries(Vec::new());
    let params = SaveMemoryParams {
        text: "Replay-safe Wiki projection canonical body.".to_string(),
        summary: "Replay-safe Wiki projection".to_string(),
        path: "/wiki/agent/tachi/replay-safe-projection".to_string(),
        importance: 0.9,
        category: "experience".to_string(),
        topic: "replay-safe projection".to_string(),
        keywords: vec!["wiki".to_string()],
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: None,
        force: true,
        auto_link: false,
        project: Some("wiki".to_string()),
        project_explicit: true,
        retention_policy: Some("permanent".to_string()),
        domain: Some("wiki".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: Some(json!({"wiki": true})),
        emit_continuity: false,
    };
    let first: Value = serde_json::from_str(
        &crate::memory_search_ops::handle_save_memory_with_wiki_projection(
            &server,
            params.clone(),
            Vec::new(),
            None,
        )
        .await
        .expect("create idless projection winner"),
    )
    .expect("first projection response JSON");
    let winner_id = first["id"].as_str().expect("winner id").to_string();

    let mut stale = make_entry("stale-replay-duplicate");
    stale.path = "/wiki/agent/tachi/replay-safe-projection-copy".to_string();
    stale.topic = "replay-safe projection".to_string();
    stale.text = "Older active Wiki projection duplicate.".to_string();
    stale.metadata = json!({"wiki": true});
    stale.domain = Some("wiki".to_string());
    server
        .with_named_project_store("wiki", |store| {
            store.upsert(&stale).map_err(|error| error.to_string())
        })
        .expect("seed stale active duplicate after canonical creation");

    let replay: Value = serde_json::from_str(
        &crate::memory_search_ops::handle_save_memory_with_wiki_projection(
            &server,
            params,
            Vec::new(),
            None,
        )
        .await
        .expect("replay idless projection"),
    )
    .expect("replay projection response JSON");
    assert_eq!(replay["saved"], json!(false));
    assert_eq!(replay["status"], json!("duplicate"));
    assert_eq!(replay["id"], json!(winner_id));
    assert_eq!(replay["wiki_duplicates_superseded"], json!(1));
    let superseded_by: Option<String> = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id=?1",
                    [&stale.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("read replay-reconciled duplicate");
    assert_eq!(superseded_by, Some(winner_id));
}

#[tokio::test]
async fn tachi_wiki_write_supersedes_multi_token_topic_and_preserves_receipt() {
    let receipt = json!({
        "schema": "model-invocation-v1",
        "effective_model": "legacy-wiki-model"
    });
    let mut predecessor = make_entry("legacy-agent-review-row");
    predecessor.path = "/wiki/general/old-agent-review".to_string();
    predecessor.topic = "agent review".to_string();
    predecessor.text = "Legacy content deliberately shares no body tokens.".to_string();
    predecessor.metadata = json!({
        "wiki": true,
        "provenance": {"model_invocation": receipt.clone()}
    });
    predecessor.domain = Some("wiki".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![predecessor.clone()]);

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Current agent review".to_string(),
            text: "Fresh canonical guidance uses entirely different wording.".to_string(),
            path: Some("/wiki/general/new-agent-review".to_string()),
            topic: Some("agent review".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["review".to_string()],
            entities: vec!["AgentReview".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("write multi-token same-topic Wiki replacement");
    let response: Value = serde_json::from_str(&response).expect("wiki response JSON");
    let replacement_id = response["id"].as_str().expect("replacement id");
    assert_eq!(response["wiki_duplicates_superseded"], json!(1));

    let (superseded_by, replacement) = server
        .with_named_project_store_read("wiki", |store| {
            let superseded_by = store
                .supersession_target(&predecessor.id)
                .map_err(|error| error.to_string())?;
            let replacement = store
                .get(replacement_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "replacement disappeared".to_string())?;
            Ok((superseded_by, replacement))
        })
        .expect("read multi-token replacement state");
    assert_eq!(superseded_by, Some(Some(replacement_id.to_string())));
    assert_eq!(
        replacement.metadata.pointer("/provenance/model_invocation"),
        Some(&receipt),
        "same-topic replacement must preserve the predecessor receipt"
    );
}

#[tokio::test]
async fn ordinary_wiki_write_never_rewrites_or_supersedes_a_guide() {
    let shared_text =
        "Agent review guidance requires exact evidence and an independent final verdict.";
    let mut guide = make_entry("guide-agent-review");
    guide.path = "/guide/global/workflows/agent-review".to_string();
    guide.topic = "agent-review".to_string();
    guide.text = shared_text.to_string();
    guide.summary = "Agent review guide".to_string();
    guide.metadata = json!({"layer": "guide", "artifact_kind": "guide"});
    guide.domain = Some("wiki".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![guide]);

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Agent review Wiki".to_string(),
            text: shared_text.to_string(),
            path: Some("/wiki/general/agent-review".to_string()),
            topic: Some("agent-review".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["review".to_string()],
            entities: vec!["AgentReview".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("write Wiki row beside same-topic Guide");
    let response: Value = serde_json::from_str(&response).expect("write json");
    assert_ne!(response["id"], json!("guide-agent-review"));
    assert_eq!(response["wiki_write_mode"], json!("created"));

    let guide = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get("guide-agent-review")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Guide row disappeared".to_string())
        })
        .expect("read unchanged Guide");
    assert_eq!(guide.path, "/guide/global/workflows/agent-review");
    assert_eq!(guide.text, shared_text);
    let superseded_by: Option<String> = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = 'guide-agent-review'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("read Guide supersession state");
    assert_eq!(superseded_by, None);
}

#[tokio::test]
async fn guide_write_never_rewrites_or_supersedes_an_ordinary_wiki_row() {
    let shared_text =
        "Agent review guidance requires exact evidence and an independent final verdict.";
    let mut wiki = make_entry("wiki-agent-review");
    wiki.path = "/wiki/general/agent-review".to_string();
    wiki.topic = "agent-review".to_string();
    wiki.text = shared_text.to_string();
    wiki.summary = "Agent review Wiki".to_string();
    wiki.metadata = json!({"wiki": true});
    wiki.domain = Some("wiki".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![wiki]);

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Agent review Guide".to_string(),
            text: shared_text.to_string(),
            path: Some("/guide/global/workflows/agent-review".to_string()),
            topic: Some("agent-review".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["review".to_string()],
            entities: vec!["AgentReview".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("write Guide row beside same-topic Wiki");
    let response: Value = serde_json::from_str(&response).expect("write json");
    assert_ne!(response["id"], json!("wiki-agent-review"));
    assert_eq!(response["wiki_write_mode"], json!("created"));

    let wiki = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get("wiki-agent-review")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "ordinary Wiki row disappeared".to_string())
        })
        .expect("read unchanged Wiki row");
    assert_eq!(wiki.path, "/wiki/general/agent-review");
    assert_eq!(wiki.text, shared_text);
    let superseded_by: Option<String> = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = 'wiki-agent-review'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("read Wiki supersession state");
    assert_eq!(superseded_by, None);
}

#[tokio::test]
async fn tachi_wiki_write_scans_the_complete_parent_bucket_for_duplicates() {
    let replacement_text =
        "Canonical queue recovery guidance requires durable claims and replay-safe receipts.";
    let mut canonical = make_entry("complete-scan-canonical");
    canonical.path = "/wiki/agent/tachi/complete-scan".to_string();
    canonical.topic = "complete-scan".to_string();
    canonical.text = "Older canonical queue recovery guidance.".to_string();
    canonical.metadata = json!({"wiki": true});
    canonical.domain = Some("wiki".to_string());

    let mut entries = vec![canonical];
    for index in 0..501 {
        let mut unrelated = make_entry(&format!("complete-scan-noise-{index:03}"));
        unrelated.path = format!("/wiki/agent/tachi/a-noise-{index:03}");
        unrelated.topic = format!("noise-{index:03}");
        unrelated.text = format!(
            "Unrelated article {index} discusses an isolated subject with no queue vocabulary."
        );
        unrelated.metadata = json!({"wiki": true});
        unrelated.domain = Some("wiki".to_string());
        entries.push(unrelated);
    }
    let mut late_duplicate = make_entry("complete-scan-late-duplicate");
    late_duplicate.path = "/wiki/agent/tachi/zzzz-late-duplicate".to_string();
    late_duplicate.topic = "late-copy".to_string();
    late_duplicate.text = replacement_text.to_string();
    late_duplicate.metadata = json!({"wiki": true});
    late_duplicate.domain = Some("wiki".to_string());
    entries.push(late_duplicate);
    let (server, _home) = seed_wiki_project_entries(entries);

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Complete duplicate scan".to_string(),
            text: replacement_text.to_string(),
            path: Some("/wiki/agent/tachi/complete-scan".to_string()),
            topic: Some("complete-scan".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["queue".to_string(), "replay".to_string()],
            entities: vec!["QueueRecovery".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("complete Wiki duplicate scan");
    let response: Value = serde_json::from_str(&response).expect("write json");
    assert_eq!(response["id"], json!("complete-scan-canonical"));

    let superseded_by: Option<String> = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = 'complete-scan-late-duplicate'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("read late duplicate");
    assert_eq!(
        superseded_by.as_deref(),
        Some("complete-scan-canonical"),
        "a duplicate beyond the former fixed scan boundary must be projected"
    );
}

#[tokio::test]
async fn tachi_wiki_write_rolls_back_canonical_and_claim_when_supersedes_edge_fails() {
    let mut canonical = make_entry("atomic-wiki-canonical");
    canonical.path = "/wiki/agent/tachi/atomic-wiki".to_string();
    canonical.topic = "atomic-wiki".to_string();
    canonical.text = "Original canonical Wiki text.".to_string();
    canonical.metadata = json!({"wiki": true});
    canonical.domain = Some("wiki".to_string());

    let mut duplicate = make_entry("atomic-wiki-duplicate");
    duplicate.path = "/wiki/agent/tachi/atomic-wiki-copy".to_string();
    duplicate.topic = "atomic-wiki".to_string();
    duplicate.text = "Older duplicate Wiki text.".to_string();
    duplicate.metadata = json!({"wiki": true});
    duplicate.domain = Some("wiki".to_string());

    let (server, _home) = seed_wiki_project_entries(vec![canonical, duplicate]);
    let wiki_db: String = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT file FROM pragma_database_list WHERE name = 'main'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("resolve Wiki DB path");
    let offline = rusqlite::Connection::open(wiki_db).expect("open edge failpoint connection");
    offline
        .execute_batch(
            r#"
            INSERT INTO memory_edges
                (source_id, target_id, relation, weight, metadata, created_at)
            VALUES
                ('wiki-edge-failure-blocker', 'wiki-edge-failure-blocker', 'test', 1.0, '{}', '');
            CREATE UNIQUE INDEX "injected wiki projection edge failure"
                ON memory_edges ((1));
            "#,
        )
        .expect("install edge failure constraint");
    drop(offline);

    let error = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Atomic Wiki".to_string(),
            text: "Replacement canonical Wiki text that must roll back.".to_string(),
            path: Some("/wiki/agent/tachi/atomic-wiki".to_string()),
            topic: Some("atomic-wiki".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["atomic-wiki".to_string()],
            entities: vec![],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect_err("edge failure must fail the whole Wiki projection");
    assert!(error.contains("UNIQUE constraint failed"), "{error}");

    server
        .with_named_project_store_read("wiki", |store| {
            let canonical = store
                .get("atomic-wiki-canonical")
                .map_err(|error| error.to_string())?
                .expect("canonical survives");
            assert_eq!(canonical.text, "Original canonical Wiki text.");
            let superseded_by: Option<String> = store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = 'atomic-wiki-duplicate'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(superseded_by, None);
            Ok(())
        })
        .expect("verify rolled-back projection");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_wiki_canonical_cannot_supersede_the_new_active_winner() {
    let mut canonical = make_entry("wiki-stale-canonical");
    canonical.path = "/wiki/agent/tachi/stale-canonical".to_string();
    canonical.topic = "stale-canonical".to_string();
    canonical.text =
        "Stable projection concurrency rule keeps one active canonical wiki winner.".to_string();
    canonical.metadata = json!({"wiki": true});
    canonical.domain = Some("wiki".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![canonical]);
    let stale_server = MemoryServer::new(server.global_db_path_buf(), server.project_db_path_buf())
        .expect("open independent stale writer");

    let arrived = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release = std::sync::Arc::new(std::sync::Barrier::new(2));
    let _pause = crate::memory_search_ops::save_memory::install_pre_upsert_pause(
        "wiki-stale-canonical",
        true,
        std::sync::Arc::clone(&arrived),
        std::sync::Arc::clone(&release),
    );
    let stale_write = tokio::spawn(async move {
        stale_server
            .tachi_wiki_write(Parameters(WikiWriteParams {
                title: "Stale canonical".to_string(),
                text: "Stale writer must not become canonical after another writer wins."
                    .to_string(),
                path: Some("/wiki/agent/tachi/stale-canonical".to_string()),
                topic: Some("stale-canonical".to_string()),
                summary: None,
                category: "experience".to_string(),
                keywords: vec![],
                entities: vec![],
                importance: 0.9,
                scope: "global".to_string(),
                retention_policy: "permanent".to_string(),
                domain: None,
                project: None,
                metadata: None,
                force: true,
                references: vec![],
                include_patterns: false,
                pattern_query: None,
                pattern_top_k: None,
            }))
            .await
    });
    tokio::task::spawn_blocking(move || arrived.wait())
        .await
        .expect("stale writer reaches pre-upsert pause");

    let winner_response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Fresh canonical".to_string(),
            text: "Stable projection concurrency rule keeps one active canonical wiki winner."
                .to_string(),
            path: Some("/wiki/agent/tachi/fresh-canonical".to_string()),
            topic: Some("fresh-canonical".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("fresh writer wins");
    let winner_id = serde_json::from_str::<Value>(&winner_response).expect("winner response JSON")
        ["id"]
        .as_str()
        .expect("winner id")
        .to_string();
    assert_ne!(winner_id, "wiki-stale-canonical", "{winner_response}");
    server
        .with_named_project_store_read("wiki", |store| {
            assert_eq!(
                store
                    .supersession_target("wiki-stale-canonical")
                    .map_err(|error| error.to_string())?,
                Some(Some(winner_id.clone())),
                "fresh writer must claim the prior canonical"
            );
            assert_eq!(
                store
                    .supersession_target(&winner_id)
                    .map_err(|error| error.to_string())?,
                Some(None),
                "fresh writer must initially be active"
            );
            Ok(())
        })
        .expect("verify fresh writer before releasing stale writer");

    tokio::task::spawn_blocking(move || release.wait())
        .await
        .expect("release stale writer");
    let error = stale_write
        .await
        .expect("join stale writer")
        .expect_err("stale canonical must be refused");
    assert!(
        error.contains("wiki projection canonical changed"),
        "{error}"
    );

    server
        .with_named_project_store_read("wiki", |store| {
            assert_eq!(
                store
                    .supersession_target("wiki-stale-canonical")
                    .map_err(|error| error.to_string())?,
                Some(Some(winner_id.clone()))
            );
            assert_eq!(
                store
                    .supersession_target(&winner_id)
                    .map_err(|error| error.to_string())?,
                Some(None),
                "the active winner must never point back to the stale canonical"
            );
            Ok(())
        })
        .expect("verify one-way supersession");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_idless_wiki_duplicate_reports_no_false_created_write() {
    let (server, _home) = seed_wiki_project_entries(Vec::new());
    let writer_one = MemoryServer::new(server.global_db_path_buf(), server.project_db_path_buf())
        .expect("open first Wiki writer");
    let writer_two = MemoryServer::new(server.global_db_path_buf(), server.project_db_path_buf())
        .expect("open second Wiki writer");
    let path = "/wiki/general/concurrent-idless-truth";
    let text = "Concurrent Wiki projection must report only one actual created write.";
    let _barrier = crate::memory_search_ops::save_memory::install_pre_upsert_identity_barrier(
        path,
        text,
        std::sync::Arc::new(std::sync::Barrier::new(2)),
    );

    let write = |writer: MemoryServer| {
        tokio::spawn(async move {
            writer
                .tachi_wiki_write(Parameters(WikiWriteParams {
                    title: "Concurrent idless truth".to_string(),
                    text: text.to_string(),
                    path: Some(path.to_string()),
                    topic: Some("concurrent-idless-truth".to_string()),
                    summary: None,
                    category: "experience".to_string(),
                    keywords: vec![],
                    entities: vec![],
                    importance: 0.8,
                    scope: "global".to_string(),
                    retention_policy: "permanent".to_string(),
                    domain: None,
                    project: None,
                    metadata: None,
                    force: true,
                    references: vec![],
                    include_patterns: false,
                    pattern_query: None,
                    pattern_top_k: None,
                }))
                .await
        })
    };
    let first = write(writer_one);
    let second = write(writer_two);
    let mut parsed = Vec::new();
    for task in [first, second] {
        let raw = task
            .await
            .expect("Wiki writer task")
            .expect("Wiki writer response");
        parsed.push(serde_json::from_str::<Value>(&raw).expect("Wiki response JSON"));
    }
    let created = parsed
        .iter()
        .find(|response| response["wiki_write_mode"] == "created")
        .expect("one writer reports the committed creation");
    let duplicate = parsed
        .iter()
        .find(|response| response["wiki_write_mode"] == "duplicate")
        .expect("one writer reports the no-write duplicate");

    assert!(created["status"]
        .as_str()
        .is_some_and(|status| status.starts_with("saved")));
    assert_eq!(duplicate["saved"], json!(false));
    assert!(duplicate.get("continuity_event").is_none());
    assert_eq!(created["id"], duplicate["id"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_distinct_guide_writes_leave_one_active_same_path_winner() {
    let (server, _home) = seed_wiki_project_entries(Vec::new());
    let writer_one = MemoryServer::new(server.global_db_path_buf(), server.project_db_path_buf())
        .expect("open first Guide writer");
    let writer_two = MemoryServer::new(server.global_db_path_buf(), server.project_db_path_buf())
        .expect("open second Guide writer");
    let path = "/guide/global/workflows/concurrent-review";
    let _barrier = crate::memory_search_ops::save_memory::install_pre_upsert_path_barrier(
        path,
        std::sync::Arc::new(std::sync::Barrier::new(2)),
    );

    let write = |writer: MemoryServer, title: &'static str, text: &'static str| {
        tokio::spawn(async move {
            writer
                .tachi_wiki_write(Parameters(WikiWriteParams {
                    title: title.to_string(),
                    text: text.to_string(),
                    path: Some(path.to_string()),
                    topic: Some("concurrent guide review".to_string()),
                    summary: None,
                    category: "experience".to_string(),
                    keywords: vec![],
                    entities: vec![],
                    importance: 0.9,
                    scope: "global".to_string(),
                    retention_policy: "permanent".to_string(),
                    domain: None,
                    project: None,
                    metadata: None,
                    force: true,
                    references: vec![],
                    include_patterns: false,
                    pattern_query: None,
                    pattern_top_k: None,
                }))
                .await
        })
    };
    let first = write(
        writer_one,
        "Concurrent Guide A",
        "First writer proposes a distinct guide body for the shared path.",
    );
    let second = write(
        writer_two,
        "Concurrent Guide B",
        "Second writer proposes different operational guidance for the shared path.",
    );
    let first: Value = serde_json::from_str(
        &first
            .await
            .expect("first Guide writer task")
            .expect("first Guide writer response"),
    )
    .expect("first Guide response JSON");
    let second: Value = serde_json::from_str(
        &second
            .await
            .expect("second Guide writer task")
            .expect("second Guide writer response"),
    )
    .expect("second Guide response JSON");
    assert_ne!(
        first["id"], second["id"],
        "the writes must exercise distinct identities"
    );

    let responses = [&first, &second];
    let replacement = responses
        .iter()
        .find(|response| response["wiki_previous_revision"] == json!(1))
        .expect("serialized replacement reports its transaction predecessor");
    let original = responses
        .iter()
        .find(|response| response.get("wiki_previous_revision").is_none())
        .expect("first creation has no predecessor");
    let replacement_id = replacement["id"].as_str().expect("replacement id");
    let original_id = original["id"].as_str().expect("original id");
    assert_eq!(replacement["wiki_write_mode"], json!("updated"));
    assert_eq!(original["wiki_write_mode"], json!("created"));

    server
        .with_named_project_store_read("wiki", |store| {
            let (active, superseded): (i64, i64) = store
                .connection()
                .query_row(
                    "SELECT
                         sum(CASE WHEN archived=0 AND superseded_by IS NULL THEN 1 ELSE 0 END),
                         sum(CASE WHEN superseded_by IS NOT NULL THEN 1 ELSE 0 END)
                     FROM memories WHERE path=?1",
                    [path],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(
                active, 1,
                "Guide path must have one active projection winner"
            );
            assert_eq!(superseded, 1, "the serialized loser must retain lineage");
            let winner = store
                .get(replacement_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "replacement Guide row disappeared".to_string())?;
            assert_eq!(winner.metadata["wiki_update_of"], json!(original_id));
            assert_eq!(winner.metadata["wiki_previous_revision"], json!(1));
            let superseded_by: Option<String> = store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id=?1",
                    [original_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(superseded_by.as_deref(), Some(replacement_id));
            Ok(())
        })
        .expect("verify Guide projection winner");
}
