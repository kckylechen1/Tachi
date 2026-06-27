use super::*;

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
