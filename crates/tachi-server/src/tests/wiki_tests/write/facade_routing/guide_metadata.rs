use super::*;

#[tokio::test]
async fn tachi_wiki_write_preserves_guide_path_and_applies_to_metadata() {
    let server = make_server();

    let response = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "write".to_string(),
            format: None,
            query: None,
            category: Some("guide".to_string()),
            lifecycle: None,
            top_k: None,
            limit: None,
            title: Some("AgentReview guide".to_string()),
            text: Some(
                "AgentReview outputs should be routed by destination layer and promotion intent."
                    .to_string(),
            ),
            path: Some("/guide/global/workflows/agent-review".to_string()),
            topic: Some("agent-review-guide".to_string()),
            summary: Some("AgentReview routing guide".to_string()),
            keywords: vec!["agent-review".to_string()],
            entities: Vec::new(),
            references: Vec::new(),
            importance: None,
            scope: Some("global".to_string()),
            project: None,
            domain: Some("docs".to_string()),
            metadata: Some(json!({
                "layer": "caller-should-not-override",
                "applies_to": {
                    "task_type": ["agent_review"],
                    "profiles": ["codex_55_review"],
                    "stage": ["review"]
                }
            })),
            force: true,
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("guide wiki write should succeed");
    let json: Value = serde_json::from_str(&response).expect("write JSON");
    assert_eq!(
        json["wiki_path"],
        json!("/guide/global/workflows/agent-review")
    );
    let id = json["id"].as_str().expect("wiki id").to_string();

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get guide memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry JSON");
    assert_eq!(entry["path"], json!("/guide/global/workflows/agent-review"));
    assert_eq!(entry["metadata"]["layer"], json!("guide"));
    assert_eq!(entry["metadata"]["scope"], json!("global"));
    assert_eq!(entry["metadata"]["authority"], json!("playbook"));
    assert_eq!(
        entry["metadata"]["applies_to"]["task_type"],
        json!(["agent_review"])
    );
    assert_eq!(
        entry["metadata"]["applies_to"]["profiles"],
        json!(["codex_55_review"])
    );
    assert_eq!(entry["metadata"]["applies_to"]["stage"], json!(["review"]));
}

#[tokio::test]
async fn v22_named_project_write_migrates_before_guide_pattern_and_reference_reads() {
    let (server, _temp_home) = crate::tests::make_server_with_temp_home();
    let project = format!("v22-guide-{}", uuid::Uuid::new_v4().simple());
    let db_path = crate::path_utils::plan_c_global_db_path(&project);
    std::fs::create_dir_all(db_path.parent().expect("named project parent"))
        .expect("create named project directory");

    {
        let mut store = memcore::MemoryStore::open_with_label(
            db_path.to_str().expect("named project path"),
            &project,
        )
        .expect("create named project fixture");
        let mut pattern = make_entry("v22-guide-pattern");
        pattern.path = "/user/patterns/guides/migration-first".to_string();
        pattern.summary = "Migrate before projected reads".to_string();
        pattern.text =
            "Named project writes migrate before reading projected references.".to_string();
        pattern.metadata = json!({
            "projection_kind": "pattern",
            "projection_key": "migration-first",
            "source_event_id": "v22-guide-pattern-event",
            "counters": {"seen": 2, "hit": 1}
        });
        store.upsert(&pattern).expect("seed projected pattern");
    }
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open v22 fixture");
        conn.execute(
            "DELETE FROM hard_state WHERE namespace = 'migrations' AND key = ?1",
            ["v23_reserved_reference_guards"],
        )
        .unwrap();
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
             DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;
             PRAGMA user_version = 22;",
        )
        .unwrap();
    }

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Migration-first guide".to_string(),
            text: "Guide metadata, projected pattern refs, and typed source refs survive the v22 upgrade."
                .to_string(),
            path: Some("/guide/project/migration-first".to_string()),
            topic: Some("migration-first-guide".to_string()),
            summary: Some("Migration-first named project guide".to_string()),
            category: "guide".to_string(),
            keywords: vec!["migration".to_string()],
            entities: Vec::new(),
            importance: 0.9,
            scope: "project".to_string(),
            retention_policy: "permanent".to_string(),
            domain: Some("wiki".to_string()),
            project: Some(project.clone()),
            metadata: Some(json!({"guide_marker": "preserved"})),
            force: true,
            references: vec!["kckylechen1/tachi#1430".to_string()],
            include_patterns: true,
            pattern_query: Some("migration-first".to_string()),
            pattern_top_k: Some(3),
        }))
        .await
        .expect("v22 named-project guide write should migrate before reads");
    let saved: Value = serde_json::from_str(&response).expect("wiki response");
    let id = saved["id"].as_str().expect("saved id");

    let entry = server
        .with_named_project_store_read(&project, |store| {
            store.get(id).map_err(|error| error.to_string())
        })
        .expect("read migrated named project")
        .expect("saved guide exists");
    assert_eq!(entry.metadata["guide_marker"], json!("preserved"));
    assert_eq!(
        entry.metadata["pattern_refs"][0]["projection_key"],
        json!("migration-first")
    );
    assert_eq!(
        entry.metadata["evidence_refs_v1"][0]["ref"],
        json!("kckylechen1/tachi#1430")
    );
    assert_eq!(
        entry.metadata.get("source_refs").unwrap_or(&Value::Null),
        &Value::Null
    );

    let verify = rusqlite::Connection::open(&db_path).expect("verify migrated named project");
    let version: i64 = verify
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 23);
}
