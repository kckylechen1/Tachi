use super::*;

#[tokio::test]
async fn evidence_repair_save_uses_supplied_source_timestamp_for_relative_dates() {
    let server = make_server();
    let params: crate::tool_params::SaveMemoryParams=serde_json::from_value(json!({
        "text":"Alice moved to Rome yesterday.","path":"/notes/source-timestamp","scope":"global","force":true,
        "timestamp":"2023-05-10T12:00:00Z","auto_link":false
    })).unwrap();
    let raw = crate::memory_search_ops::handle_save_memory(&server, params)
        .await
        .unwrap();
    let saved: Value = serde_json::from_str(&raw).unwrap();
    let result = memory_call(
        &server,
        json!({"action":"get","id":saved["id"],"as_of":"2025-01-01T00:00:00Z"}),
    )
    .await;
    assert!(result.get("error").is_none(), "{result}");
    assert!(result["valid_from"]
        .as_str()
        .unwrap()
        .starts_with("2023-05-09"));
}

#[tokio::test]
async fn evidence_repair_search_and_ask_budget_spans_memory_and_wiki() {
    let mut wiki = make_entry("budget-wiki");
    wiki.path = "/wiki/general/budget".into();
    wiki.text = format!("budgetcombined {}", "wiki source facts ".repeat(2500));
    wiki.summary = "budgetcombined wiki source".into();
    let (server, _home) = seed_wiki_project_entries(vec![wiki]);
    let mut memory = make_entry("budget-memory");
    memory.path = "/notes/budget".into();
    memory.text = format!("budgetcombined {}", "memory source facts ".repeat(2500));
    memory.summary = "budgetcombined memory source".into();
    server
        .with_global_store(|store| store.upsert(&memory).map_err(|e| e.to_string()))
        .unwrap();
    for action in ["search", "ask"] {
        let result = memory_call(&server,json!({"action":action,"query":"budgetcombined","scope":"all","top_k":3,"auto_conversation_scope":false})).await;
        let rows: Vec<&Value> = if action == "search" {
            result["sections"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|s| s["rows"].as_array().unwrap())
                .collect()
        } else {
            result["evidence"].as_array().unwrap().iter().collect()
        };
        assert!(rows.iter().any(|r| r["id"] == "budget-wiki"), "{result}");
        assert!(rows.iter().any(|r| r["id"] == "budget-memory"), "{result}");
        let count: usize = rows
            .iter()
            .filter_map(|r| r["text"].as_str())
            .map(|t| t.chars().count())
            .sum();
        assert_eq!(count, 48000, "combined evidence budget");
        assert!(rows.iter().any(|r| r["text_budget_truncated"] == true));
    }
}

async fn memory_call(server: &crate::MemoryServer, args: Value) -> Value {
    let params = serde_json::from_value(args).expect("facade params");
    let raw = crate::facade_memory_ops::handle_tachi_memory(server, params)
        .await
        .expect("facade call");
    serde_json::from_str(&raw).expect("facade JSON")
}

#[tokio::test]
async fn evidence_repair_string_prefix_and_public_conversation_controls() {
    let server = make_server();
    for (id, path) in [
        ("prefix-a", "/notes/build-east/c000"),
        ("prefix-b", "/notes/build-west/c000"),
        ("prefix-neighbor", "/notes/build-eastern/c000"),
    ] {
        let mut entry = make_entry(id);
        entry.path = path.into();
        entry.text = format!("prefixunique evidence for {id}");
        entry.summary = entry.text.clone();
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|e| e.to_string()))
            .unwrap();
    }
    let result = memory_call(&server, json!({"action":"search","query":"prefixunique","scope":"memory","path_prefix":"/notes/build-","top_k":10,"auto_conversation_scope":false})).await;
    assert_eq!(
        result["sections"][0]["rows"].as_array().unwrap().len(),
        3,
        "{result}"
    );
    for pack in [true, false] {
        let result = memory_call(&server, json!({"action":"search","query":"prefixunique","scope":"memory","conversation_scope":"/notes/build-east","top_k":10,"auto_conversation_scope":false,"multi_evidence_pack":pack})).await;
        let rows = result["sections"][0]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{result}");
        assert_eq!(rows[0]["id"], "prefix-a");
        assert_eq!(rows[0].get("pack_rank").is_some(), pack);
    }
}

#[tokio::test]
async fn evidence_repair_ask_bare_number_cannot_be_grounded_by_related_history() {
    let server = make_server();
    let mut entry = make_entry("older-install");
    entry.text = "Temporal installation acceptance passed for release 1.2 in May.".into();
    entry.summary = entry.text.clone();
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| e.to_string()))
        .unwrap();
    for query in [
        "有什么证据证明 #2128 temporal 已经在本机安装并通过验收？",
        "#2128已安装了吗？",
    ] {
        let result = memory_call(
            &server,
            json!({"action":"ask","query":query,"synthesize":false}),
        )
        .await;
        assert_eq!(result["grounding_status"], "missing_anchor", "{result}");
        assert_eq!(result["thinking"]["confidence"], "low");
        assert_eq!(result["required_anchors"][0]["source_ref"], "#2128");
        assert!(result["synthesis"].is_null());
    }
}

#[tokio::test]
async fn evidence_repair_ephemeral_chunks_remain_active_and_idempotent() {
    let server = make_server();
    let common = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ".repeat(15);
    let text = format!("user: {common} bluewater\nassistant: {common} redmountain");
    let args = json!({"action":"save","kind":"note","scope":"global","path":"/notes/chunk-survival","text":text,"summary":"alpha bravo bluewater redmountain","keywords":["alpha","bravo"],"retention_policy":"ephemeral","force":true});
    let saved = memory_call(&server, args.clone()).await;
    assert_eq!(saved["complete"], true, "{saved}");
    let ids = saved["chunk_ids"].as_array().expect("chunk IDs");
    assert_eq!(ids.len(), 2, "{saved}");
    for id in ids {
        let (superseded, until): (Option<String>, Option<String>) = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT superseded_by, valid_until FROM memories WHERE id=?1",
                        [id.as_str().unwrap()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert!(
            superseded.is_none() && until.is_none(),
            "chunk {id} lost: {superseded:?} {until:?}"
        );
    }
    let found = memory_call(&server, json!({"action":"search","query":"alpha bravo bluewater","scope":"memory","path_prefix":"/notes/chunk-survival","top_k":10,"auto_conversation_scope":false})).await;
    assert_eq!(
        found["sections"][0]["rows"].as_array().unwrap().len(),
        2,
        "{found}"
    );
    let retry = memory_call(&server, args).await;
    assert_eq!(retry["complete"], true, "{retry}");
    assert_eq!(retry["chunk_ids"], saved["chunk_ids"]);
}

#[tokio::test]
async fn evidence_repair_historical_source_and_unrelated_dates_survive_as_of() {
    let server = make_server();
    for (path, text, expected) in [
        (
            "recorded",
            "Conversation recorded 2023-05-10: Alice moved to Rome yesterday.",
            "2023-05-09",
        ),
        (
            "independent",
            "Alice moved to Rome on 2023-01-01. Bob joined on 2024-01-01.",
            "2023-01-01",
        ),
    ] {
        let saved = memory_call(&server, json!({"action":"save","kind":"note","scope":"global","path":format!("/notes/{path}"),"text":text,"force":true})).await;
        let id = saved["id"].as_str().expect("saved id");
        let result = memory_call(
            &server,
            json!({"action":"get","id":id,"as_of":"2025-01-01T00:00:00Z"}),
        )
        .await;
        assert!(result.get("error").is_none(), "{result}");
        assert!(
            result["valid_from"].as_str().unwrap().starts_with(expected),
            "{result}"
        );
        assert!(
            result.get("valid_until").is_none_or(Value::is_null),
            "{result}"
        );
    }
}

#[test]
fn evidence_repair_combined_sections_share_one_budget_even_at_tiny_remainder() {
    let mut sections = vec![
        (
            "Memory".into(),
            json!([{"text":"a".repeat(31999)}, {"text":"b".repeat(32000)}]),
        ),
        (
            "Wiki".into(),
            json!([{"text":"c".repeat(16000)}, {"text":"d".repeat(32000)}]),
        ),
    ];
    crate::shared_defs::apply_section_evidence_budget(&mut sections);
    let count: usize = sections
        .iter()
        .flat_map(|(_, s)| s.as_array().unwrap())
        .filter_map(|r| r["text"].as_str())
        .map(|t| t.chars().count())
        .sum();
    assert_eq!(count, 48000);
    assert_eq!(sections[1].1[0]["text"].as_str().unwrap().len(), 16000);
    assert_eq!(sections[0].1[1]["text"], ".");
    assert_eq!(sections[1].1[1]["text_omitted_budget"], true);
}
