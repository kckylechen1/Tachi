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
