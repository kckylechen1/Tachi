use super::*;

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
    let entries = cases
        .iter()
        .map(|(id, _)| make_entry(id))
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
    assert_eq!(entry.metadata["rem"], Value::Null);
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
            text: "TrendLock should protect a live trend until the trend invalidation rule actually breaks.".to_string(),
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
