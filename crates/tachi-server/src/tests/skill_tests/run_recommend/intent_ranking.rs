use super::*;

#[tokio::test]
async fn recommend_skill_prefers_matching_skill() {
    let server = make_server();
    let excel = make_skill_capability(
        "skill:excel-automation",
        "excel-automation",
        "Build spreadsheet workflows and Excel reports from CSV data.",
        "listed",
    );
    let web = make_skill_capability(
        "skill:web-research",
        "web-research",
        "Browse websites and summarize online sources.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&excel).map_err(|e| e.to_string())?;
            store.hub_register(&web).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register skills");

    let result = server
        .recommend_skill(Parameters(RecommendSkillParams {
            query: "make an excel spreadsheet report from csv exports".to_string(),
            host: Some("codex".to_string()),
            limit: 3,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_skill should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let skills = json["skills"].as_array().expect("skills array");
    assert!(
        !skills.is_empty(),
        "expected at least one skill recommendation"
    );
    assert_eq!(skills[0]["id"], "skill:excel-automation");
    assert_eq!(
        skills[0]["suggested_tool_name"],
        json!("tachi_skill_excel_automation")
    );
}

#[tokio::test]
async fn recommend_skill_prefers_review_for_code_review_queries() {
    let server = make_server();
    let review = make_skill_capability(
        "skill:review",
        "review",
        "Inspect diffs and catch correctness, security, and maintainability risks before merge.",
        "listed",
    );
    let baoyu_markdown = make_skill_capability(
        "skill:baoyu-markdown-to-html",
        "baoyu-markdown-to-html",
        "Convert markdown docs to HTML, preserve code blocks, review formatting, and publish documentation.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&review).map_err(|e| e.to_string())?;
            store
                .hub_register(&baoyu_markdown)
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register skills");

    let result = server
        .recommend_skill(Parameters(RecommendSkillParams {
            query: "code review".to_string(),
            host: Some("codex".to_string()),
            limit: 3,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_skill should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let top_id = json["skills"][0]["id"].as_str().expect("top skill id");
    assert!(
        matches!(
            top_id,
            "skill:review" | "skill:waza-check" | "skill:superpowers-requesting-code-review"
        ),
        "expected a review workflow skill to win, got {json}"
    );
    assert_ne!(top_id, "skill:baoyu-markdown-to-html");
}

#[tokio::test]
async fn recommend_skill_prefers_investigate_for_debug_500_error_queries() {
    let server = make_server();
    let investigate = make_skill_capability(
        "skill:investigate",
        "investigate",
        "Debug 500 errors by tracing requests, logs, and failing handlers.",
        "listed",
    );
    let feishu_docs = make_skill_capability(
        "skill:feishu-doc-reader",
        "feishu-doc-reader",
        "Read Feishu docs, error guides, and debugging notes for API integrations.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store
                .hub_register(&investigate)
                .map_err(|e| e.to_string())?;
            store
                .hub_register(&feishu_docs)
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register skills");

    let result = server
        .recommend_skill(Parameters(RecommendSkillParams {
            query: "debug 500 error".to_string(),
            host: Some("codex".to_string()),
            limit: 3,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_skill should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["skills"][0]["id"], "skill:investigate");
}

#[tokio::test]
async fn recommend_skill_prefers_ship_for_create_pr_queries() {
    let server = make_server();
    let ship = make_skill_capability(
        "skill:ship",
        "ship",
        "Ship code, prepare pull requests, and land changes safely.",
        "listed",
    );
    let review = make_skill_capability(
        "skill:review",
        "review",
        "Review code changes and summarize risks before merge.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&ship).map_err(|e| e.to_string())?;
            store.hub_register(&review).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register skills");

    let result = server
        .recommend_skill(Parameters(RecommendSkillParams {
            query: "ship this code, create a PR".to_string(),
            host: Some("codex".to_string()),
            limit: 3,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_skill should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["skills"][0]["id"], "skill:ship");
}

#[tokio::test]
async fn recommend_skill_uses_active_patterns_as_ranking_context() {
    let server = make_server();
    let closure = make_skill_capability(
        "skill:marmalade-closure",
        "marmalade-closure",
        "Write marmalade closure notes and durable project completion records.",
        "listed",
    );
    let spreadsheet = make_skill_capability(
        "skill:spreadsheet",
        "spreadsheet",
        "Build spreadsheet reports from CSV exports.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&closure).map_err(|e| e.to_string())?;
            store
                .hub_register(&spreadsheet)
                .map_err(|e| e.to_string())?;
            let mut pattern = make_entry("pattern-alignment-bridge-closure");
            pattern.path = "/user/patterns/agent_os/alignment-bridge".to_string();
            pattern.summary = "Zephyr alignment bridge closes through marmalade closure".to_string();
            pattern.text =
                "When the user asks about the zephyr alignment bridge, use marmalade closure to write durable completion records."
                    .to_string();
            pattern.metadata = json!({
                "projection_kind": "pattern",
                "projection_key": "alignment-bridge-closure",
                "source_event_id": "pattern-event-recommend-skill",
                "counters": {"seen": 4, "hit": 2}
            });
            store.upsert(&pattern).map_err(|e| e.to_string())
        })
        .expect("seed skills and pattern");

    let result = server
        .recommend_skill(Parameters(RecommendSkillParams {
            query: "zephyr".to_string(),
            host: Some("codex".to_string()),
            limit: 3,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_skill should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let top = &json["skills"][0];
    assert_eq!(
        top["id"],
        json!("skill:marmalade-closure"),
        "expected pattern-bridged marmalade skill to rank first, got {json}"
    );
    assert_eq!(
        top["pattern_refs"][0]["projection_key"],
        json!("alignment-bridge-closure")
    );
    assert!(
        top["reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or_default()
                .contains("active pattern 'alignment-bridge-closure'")),
        "expected active pattern reason in {json}"
    );
}

#[tokio::test]
async fn recommend_skill_host_bonus_ignores_definition_paths() {
    let server = make_server();
    let mut alpha = make_skill_capability(
        "skill:alpha",
        "host affinity fixture",
        "Fixture isolates host affinity from filesystem paths.",
        "listed",
    );
    let mut zeta = make_skill_capability(
        "skill:zeta",
        "host affinity fixture",
        "Fixture isolates host affinity from filesystem paths.",
        "listed",
    );
    alpha.definition = json!({
        "content": "host affinity fixture",
        "resolved_path": "/work/sigil/skills/fixture/SKILL.md"
    })
    .to_string();
    zeta.definition = json!({
        "content": "host affinity fixture",
        "resolved_path": "/work/codex-issue/skills/fixture/SKILL.md"
    })
    .to_string();

    server
        .with_global_store(|store| {
            store.hub_register(&alpha).map_err(|e| e.to_string())?;
            store.hub_register(&zeta).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register path-variant skills");

    let result = server
        .recommend_skill(Parameters(RecommendSkillParams {
            query: "host affinity fixture".to_string(),
            host: Some("codex".to_string()),
            limit: 10,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_skill should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let fixtures = json["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .filter(|skill| matches!(skill["id"].as_str(), Some("skill:alpha" | "skill:zeta")))
        .collect::<Vec<_>>();

    assert_eq!(fixtures.len(), 2, "both path-variant skills must rank");
    assert_eq!(
        fixtures[0]["id"],
        json!("skill:alpha"),
        "definition paths must not give skill:zeta a codex host bonus"
    );
    assert_eq!(
        fixtures[0]["score"], fixtures[1]["score"],
        "the absolute checkout path is not host affinity"
    );
    assert!(
        fixtures
            .iter()
            .flat_map(|skill| skill["reasons"].as_array().into_iter().flatten())
            .all(|reason| reason.as_str() != Some("mentions host 'codex'")),
        "a host bonus must come from stable capability metadata, never an implementation path"
    );
}
