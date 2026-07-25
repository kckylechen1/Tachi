use super::*;

#[tokio::test]
async fn tachi_wiki_write_stores_and_rejects_invalid_references() {
    let server = make_server();

    let bad = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Bad refs".to_string(),
            text: "Should not persist.".to_string(),
            path: None,
            topic: Some("bad-refs".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.5,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec!["relative/path.md".to_string()],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect_err("relative path should fail validation");
    assert!(bad.contains("references[0]"), "err: {bad}");

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Good refs".to_string(),
            text: "Lesson with links.".to_string(),
            path: None,
            topic: Some("good-refs".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: Some(json!({
                "source_refs": ["https://example.com/forged-legacy-ref"]
            })),
            force: true,
            references: vec![
                "https://github.com/kckylechen1/tachi/issues/149".to_string(),
                "kckylechen1/tachi#149".to_string(),
            ],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("valid references should write");

    let json: Value = serde_json::from_str(&response).expect("json");
    let id = json["id"].as_str().expect("id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry json");
    assert!(
        entry["metadata"].get("source_refs").is_none(),
        "new wiki writes must discard caller-supplied legacy source_refs: {entry:#}"
    );
    let typed_refs = entry["metadata"]["evidence_refs_v1"]
        .as_array()
        .expect("evidence_refs_v1 array");
    assert_eq!(typed_refs.len(), 2);
    assert_eq!(
        typed_refs[0]["ref"],
        json!("https://github.com/kckylechen1/tachi/issues/149")
    );
    assert_eq!(typed_refs[1]["ref"], json!("kckylechen1/tachi#149"));
    assert_eq!(typed_refs[1]["target_kind"], json!("issue"));
    assert_eq!(json["continuity_event"]["event_type"], json!("wiki.saved"));
    let events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    event_type: Some("wiki.saved".to_string()),
                    limit: 10,
                    ..memcore::TachiEventQuery::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("read wiki events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["wiki_id"], json!(id));
    assert_eq!(
        events[0]
            .projection_hints
            .iter()
            .map(|projection| projection.as_str())
            .collect::<Vec<_>>(),
        vec!["timeline", "project_cycle"]
    );
}

#[tokio::test]
async fn tachi_wiki_write_can_attach_projected_pattern_refs() {
    // Pattern discovery consults named-project state under TACHI_HOME. Keep
    // this fixture isolated so a real older checkout DB cannot become an
    // accidental dependency of a global wiki-write regression test.
    let (server, _temp_home) = crate::tests::make_server_with_temp_home();
    server
        .with_global_store(|store| {
            let mut pattern = make_entry("wiki-pattern-ref-row");
            pattern.path = "/user/patterns/agent_os/continuity-first".to_string();
            pattern.summary = "Continuity-first project management".to_string();
            pattern.text =
                "Agent OS treats memory, lorebook, and affect as continuity.".to_string();
            pattern.metadata = json!({
                "projection_kind": "pattern",
                "projection_key": "continuity-first",
                "source_event_id": "pattern-event-1",
                "counters": {"seen": 3, "hit": 1}
            });
            store.upsert(&pattern).map_err(|e| e.to_string())
        })
        .expect("seed projected pattern");

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Continuity architecture".to_string(),
            text: "Continuity architecture should preserve references to active patterns."
                .to_string(),
            path: None,
            topic: Some("continuity-architecture".to_string()),
            summary: Some("Continuity architecture".to_string()),
            category: "experience".to_string(),
            keywords: vec!["continuity".to_string()],
            entities: vec!["Agent OS".to_string()],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: true,
            pattern_query: Some("continuity-first".to_string()),
            pattern_top_k: Some(3),
        }))
        .await
        .expect("wiki write with pattern refs");
    let json: Value = serde_json::from_str(&response).expect("wiki response json");
    let id = json["id"].as_str().expect("wiki id").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki memory");
    let entry: Value = serde_json::from_str(&fetched).expect("wiki entry json");
    assert_eq!(
        entry["metadata"]["pattern_refs"][0]["id"],
        json!("wiki-pattern-ref-row")
    );
    assert_eq!(
        entry["metadata"]["pattern_refs"][0]["projection_key"],
        json!("continuity-first")
    );
}
