use super::*;

#[tokio::test]
async fn save_memory_includes_provenance_for_registered_agent() {
    let server = make_server();

    server
        .agent_register(Parameters(AgentRegisterParams {
            agent_id: "claude-code".to_string(),
            display_name: Some("Claude Code".to_string()),
            capabilities: vec!["code-gen".to_string()],
            tool_filter: None,
            rate_limit_rpm: None,
            rate_limit_burst: None,
        }))
        .await
        .expect("agent_register should succeed");

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Investigated the failing OAuth callback edge case.".to_string(),
            summary: "OAuth callback investigation".to_string(),
            path: "/project/auth".to_string(),
            importance: 0.8,
            category: "fact".to_string(),
            topic: "auth".to_string(),
            keywords: vec!["oauth".to_string(), "callback".to_string()],
            persons: vec![],
            entities: vec!["oauth".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");
    let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"]
        .as_str()
        .expect("save_memory should return id")
        .to_string();

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("get JSON");
    let provenance = &fetched_json["metadata"]["provenance"];

    assert_eq!(provenance["tool_name"], json!("save_memory"));
    assert_eq!(provenance["source_kind"], json!("memory_write"));
    assert_eq!(provenance["requested_scope"], json!("project"));
    assert_eq!(provenance["db_scope"], json!("global"));
    assert_eq!(provenance["agent"]["agent_id"], json!("claude-code"));
}

#[tokio::test]
async fn save_memory_folds_legacy_persons_and_location_out_of_public_fields() {
    let server = make_server();
    let id = "legacy-person-location-boundary";

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text:
                "Kyle validated that legacy OpenClaw person and location fields are boundary-only."
                    .to_string(),
            summary: "Legacy field boundary".to_string(),
            path: "/project/legacy-boundary".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "legacy-fields".to_string(),
            keywords: vec!["legacy".to_string()],
            persons: vec!["Kyle".to_string()],
            entities: vec!["Tachi".to_string()],
            location: "St. Louis".to_string(),
            scope: "project".to_string(),
            vector: None,
            id: Some(id.to_string()),
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");
    let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    assert!(saved_json.get("persons").is_none());
    assert!(saved_json.get("location").is_none());

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("get JSON");
    assert!(fetched_json.get("persons").is_none());
    assert!(fetched_json.get("location").is_none());
    assert_eq!(fetched_json["entities"], json!(["Tachi", "Kyle", "user"]));
    assert_eq!(
        fetched_json["metadata"]["legacy_location"],
        json!("St. Louis")
    );
}

#[tokio::test]
async fn save_memory_updates_legacy_location_on_overwrite() {
    let server = make_server();
    let id = "legacy-location-update-boundary";

    for location in ["St. Louis", "Shanghai"] {
        let saved = server
            .save_memory(Parameters(SaveMemoryParams {
                text: "Location overwrite test.".to_string(),
                summary: "Location overwrite".to_string(),
                path: "/project/location-overwrite".to_string(),
                importance: 0.7,
                category: "fact".to_string(),
                topic: "legacy-fields".to_string(),
                keywords: vec!["legacy".to_string()],
                persons: vec![],
                entities: vec!["Tachi".to_string()],
                location: location.to_string(),
                scope: "project".to_string(),
                vector: None,
                id: Some(id.to_string()),
                force: true,
                auto_link: false,
                project: None,
                retention_policy: None,
                domain: None,
                timestamp: None,
                valid_from: None,
                valid_until: None,
                metadata: None,
            }))
            .await
            .expect("save_memory should succeed");
        let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
        assert!(saved_json.get("location").is_none());
    }

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("get JSON");
    assert!(fetched_json.get("location").is_none());
    assert_eq!(
        fetched_json["metadata"]["legacy_location"],
        json!("Shanghai")
    );
}

#[tokio::test]
async fn save_memory_allows_curated_tier_metadata() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Curated trading lessons should enter the lifecycle as consolidated knowledge."
                .to_string(),
            summary: "Curated lifecycle tier".to_string(),
            path: "/trading/equity/lessons/tier-test".to_string(),
            importance: 0.85,
            category: "experience".to_string(),
            topic: "memory-lifecycle".to_string(),
            keywords: vec!["tier".to_string()],
            persons: vec![],
            entities: vec!["Tachi".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: Some("permanent".to_string()),
            domain: Some("equity_trading".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({"tier": "consolidated"})),
        }))
        .await
        .expect("save_memory should succeed");
    let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"].as_str().expect("id").to_string();

    let tier = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT tier FROM memories WHERE id = ?1", [&id], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| e.to_string())
        })
        .expect("read tier");
    assert_eq!(tier, "consolidated");
}

#[tokio::test]
async fn save_memory_clamps_importance_into_valid_range() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "importance clamp regression".to_string(),
            summary: "importance clamp".to_string(),
            path: "/project/tests".to_string(),
            importance: 9.9,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec!["importance".to_string()],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");

    let saved_json: serde_json::Value =
        serde_json::from_str(&saved).expect("save should be valid JSON");
    let id = saved_json["id"].as_str().expect("save should return id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value =
        serde_json::from_str(&fetched).expect("get should be valid JSON");
    assert_eq!(fetched_json["importance"], json!(1.0));
}

#[tokio::test]
async fn save_memory_redacts_obvious_secrets_before_persisting() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "The bearer is Authorization: Bearer test-bearer-token-abcdefghijklmnopqrstuvwxyz and token=secret_token_value_abcdefghijklmnopqrstuvwxyz".to_string(),
            summary: "redaction".to_string(),
            path: "/scratch/redaction".to_string(),
            importance: 0.9,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");

    let saved_json: Value = serde_json::from_str(&saved).expect("save JSON");
    assert!(saved_json["secret_redactions"].as_u64().unwrap_or(0) >= 2);
    let id = saved_json["id"].as_str().expect("saved id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: Value = serde_json::from_str(&fetched).expect("get JSON");
    let text = fetched_json["text"].as_str().unwrap_or_default();
    assert!(text.contains("[REDACTED]"));
    assert!(!text.contains("test-bearer-token-abcdefghijklmnopqrstuvwxyz"));
    assert!(!text.contains("secret_token_value_abcdefghijklmnopqrstuvwxyz"));
}

#[tokio::test]
async fn save_memory_scrubs_think_tags_from_text_and_summary() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Keep this.\n<think>private reasoning\nwith details</think>\nAnd this."
                .to_string(),
            summary: "Summary <think>hidden</think> visible".to_string(),
            path: "/scratch/tests/think-scrub".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec!["think-scrub".to_string()],
            persons: vec![],
            entities: vec!["memory-server".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");

    let saved_json: Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"].as_str().expect("saved id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: Value = serde_json::from_str(&fetched).expect("get JSON");
    let text = fetched_json["text"].as_str().unwrap_or_default();
    let summary = fetched_json["summary"].as_str().unwrap_or_default();
    assert!(text.contains("Keep this."));
    assert!(text.contains("And this."));
    assert!(!text.contains("<think>"));
    assert!(!text.contains("private reasoning"));
    assert_eq!(summary, "Summary  visible");
}

#[tokio::test]
async fn save_memory_noise_rejection_returns_structured_json() {
    let server = make_server();

    let response = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "hello".to_string(),
            summary: String::new(),
            path: "/scratch/tests/noise".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: false,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("noise rejection should be a normal JSON response");

    let json: Value = serde_json::from_str(&response).expect("noise JSON");
    assert_eq!(json["saved"], json!(false));
    assert_eq!(json["noise"], json!(true));
}

#[test]
fn strip_code_fence_uses_last_closing_fence() {
    let raw = "```json\n{\"outer\":\"ok\",\"inner\":\"```json\\n{}\\n```\"}\n```";
    let stripped = crate::llm::LlmClient::strip_code_fence(raw);
    assert_eq!(
        stripped,
        "{\"outer\":\"ok\",\"inner\":\"```json\\n{}\\n```\"}"
    );
}

#[test]
fn fact_to_entry_merges_legacy_persons_into_entities() {
    let fact = json!({
        "text": "Kyle migrated Sigil search completely and successfully.",
        "topic": "migration",
        "keywords": ["sigil", "search"],
        "persons": ["Kyle", ""],
        "entities": ["Sigil", "memory-server"],
        "scope": "project",
        "importance": 0.9
    });

    let entry = crate::tool_params::fact_to_entry(&fact, "extraction", json!({}))
        .expect("fact_to_entry should build an entry");
    assert!(entry.persons.is_empty());
    assert_eq!(
        entry.entities,
        vec![
            "Sigil".to_string(),
            "memory-server".to_string(),
            "Kyle".to_string(),
            "user".to_string()
        ]
    );
}

/// Regression for "auto-link bumps access stats" bug.
///
/// `save_memory` with `auto_link: true` spawns a background search keyed by
/// each entity to discover related memories. That search is a write-side side
/// effect, NOT a user read — it must use `SearchOptions { record_access: false,
/// .. }` so the matched-but-not-actually-read entries do not get their
/// `access_count` / `last_access` bumped (which would inflate ACT-R frequency,
/// suppress the `access_count = 0` GC prune path, and bias promotion ranking).

#[tokio::test]
async fn save_memory_auto_link_does_not_bump_target_access_count() {
    let server = make_server();

    // Seed an entry tagged with the entity we will later search via auto-link.
    let seeded_id = format!("auto-link-target-{}", uuid::Uuid::new_v4());
    let mut seeded = make_entry(&seeded_id);
    seeded.entities = vec!["sigil".to_string()];
    seeded.text = "Original notes about sigil internals".to_string();
    server
        .with_global_store(|store| store.upsert(&seeded).map_err(|e| format!("seed: {e}")))
        .expect("seed entry");

    // Sanity: freshly-written entry has access_count = 0.
    let pre = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&seeded_id, false)
                .map_err(|e| format!("get: {e}"))
        })
        .expect("pre-read")
        .expect("seeded entry exists");
    assert_eq!(
        pre.access_count, 0,
        "fresh seed should have access_count=0, got {}",
        pre.access_count
    );
    assert!(
        pre.last_access.is_none(),
        "fresh seed should have no last_access"
    );

    // Trigger save_memory with auto_link=true and an entity that matches the
    // seeded entry. Auto-link will search global store for "sigil", which will
    // return the seeded entry as a candidate.
    let _ = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "New observation about sigil rotation".to_string(),
            summary: String::new(),
            path: "/".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec!["sigil".to_string()],
            location: String::new(),
            scope: "general".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");

    // Auto-link runs in tokio::spawn; give it time to execute the search.
    // 500ms is generous for a sync sqlite read against an in-memory test DB.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Re-read the seeded entry. Its access_count MUST still be 0 — auto-link
    // is a write-side side effect, not a user read.
    let post = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&seeded_id, false)
                .map_err(|e| format!("get: {e}"))
        })
        .expect("post-read")
        .expect("seeded entry still exists");

    assert_eq!(
        post.access_count, 0,
        "auto-link search must NOT bump access_count of matched entries; got {}",
        post.access_count
    );
    assert!(
        post.last_access.is_none(),
        "auto-link search must NOT set last_access on matched entries; got {:?}",
        post.last_access
    );
}
