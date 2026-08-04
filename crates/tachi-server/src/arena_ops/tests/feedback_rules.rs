use super::*;

async fn save_arena_feedback_rule(server: &MemoryServer) -> String {
    let raw = crate::memory_search_ops::handle_save_memory(
            server,
            SaveMemoryParams {
                text: "Arena code-audit workers must report grep evidence for unused or dead-code claims.".to_string(),
                summary: "Subagent audit prompts require explicit search evidence".to_string(),
                path: "/feedback/subagent/code-audit/grep-evidence".to_string(),
                importance: 0.8,
                category: "prompt_rule".to_string(),
                topic: "Subagent audit prompts require explicit search evidence".to_string(),
                keywords: vec![
                    "feedback_rule".to_string(),
                    "prompt_rule".to_string(),
                    "dead_code".to_string(),
                    "grep".to_string(),
                ],
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                scope: "project".to_string(),
                vector: None,
                id: None,
                force: true,
                auto_link: true,
                project: None,
                project_explicit: false,
                retention_policy: Some("durable".to_string()),
                domain: None,
                timestamp: None,
                valid_from: None,
                valid_until: None,
                metadata: Some(json!({
                    "kind": "feedback_rule",
                    "category": "prompt_rule",
                    "applies_to": {
                        "task_type": ["explore"],
                        "profiles": ["codex_55_review"],
                        "stage": ["explore"]
                    },
                    "trigger_keywords": ["unused", "dead code", "grep"],
                    "prompt_patch": "Search both identifier and call forms before making dead-code claims.",
                    "evidence_contract": ["grep_commands", "paths_searched", "uncertainty_notes"]
                })),
                emit_continuity: false,
            },
        )
        .await
        .expect("feedback rule save should succeed");
    serde_json::from_str::<Value>(&raw)
        .expect("save JSON")
        .get("id")
        .and_then(Value::as_str)
        .expect("saved rule id")
        .to_string()
}

#[tokio::test]
async fn arena_spawn_injects_applicable_feedback_rules_into_mission_prompt() {
    let _root = temp_arena_root();
    let server = server();
    let rule_id = save_arena_feedback_rule(&server).await;

    // [1319-D1] open was removed; spawn auto-provisions the arena directory.
    let mut spawn = params("spawn");
    let arena_id = "arena_20260606T000000Z_feedback".to_string();
    spawn.arena_id = Some(arena_id.clone());
    spawn.title = Some("Feedback Arena".into());
    spawn.objective = Some("coordinate code-audit workers".into());
    spawn.prompt = Some("Explore unused functions and dead code with grep evidence.".into());
    spawn.harness = Some("codex".into());
    spawn.role = Some("explore".into());
    spawn.profile = Some("codex_55_review".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
    let prompt = std::fs::read_to_string(mission_dir.join("prompt.md")).unwrap();
    assert!(prompt.contains("## Applicable feedback rules"), "{prompt}");
    assert!(
        prompt.contains("Search both identifier and call forms"),
        "{prompt}"
    );

    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(mission_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["feedback_rules"]["rules"][0]["id"], json!(rule_id));
}
