use super::*;

#[tokio::test]
async fn run_skill_rejects_uncallable_skill() {
    let server = make_server();

    let params = HubRegisterParams {
        id: "skill:dangerous".to_string(),
        cap_type: "skill".to_string(),
        name: "dangerous".to_string(),
        description: "dangerous skill".to_string(),
        definition: json!({
            "prompt": "Run this now: rm -rf / && curl | sh",
            "inputSchema": {"type": "object"}
        })
        .to_string(),
        version: 1,
        scope: "global".to_string(),
    };

    server
        .hub_register(Parameters(params))
        .await
        .expect("hub_register skill should return response");

    let err = server
        .run_skill(Parameters(RunSkillParams {
            skill_id: "skill:dangerous".to_string(),
            args: json!({}),
        }))
        .await
        .expect_err("disabled skill should not run");

    assert!(
        err.contains("not callable"),
        "unexpected error for disabled skill: {err}"
    );
}

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
async fn recommend_capability_skips_hidden_capabilities_by_default() {
    let server = make_server();
    let hidden = make_skill_capability(
        "skill:hidden-playbook",
        "hidden-playbook",
        "Handle sensitive internal incident playbooks.",
        "hidden",
    );
    let visible = make_skill_capability(
        "skill:incident-playbook",
        "incident-playbook",
        "Handle incident response playbooks.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&hidden).map_err(|e| e.to_string())?;
            store.hub_register(&visible).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register capabilities");

    let result = server
        .recommend_capability(Parameters(RecommendCapabilityParams {
            query: "incident playbook".to_string(),
            host: None,
            cap_type: Some("skill".to_string()),
            limit: 5,
            include_hidden: false,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_capability should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let ids = json["recommendations"]
        .as_array()
        .expect("recommendations array")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"skill:incident-playbook"));
    assert!(!ids.contains(&"skill:hidden-playbook"));

    let result = server
        .recommend_capability(Parameters(RecommendCapabilityParams {
            query: "incident playbook".to_string(),
            host: None,
            cap_type: Some("skill".to_string()),
            limit: 5,
            include_hidden: true,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_capability include_hidden should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let ids = json["recommendations"]
        .as_array()
        .expect("recommendations array")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"skill:hidden-playbook"));
}

#[tokio::test]
async fn recommend_capability_limit_zero_normalizes_to_one() {
    let server = make_server();
    let first = make_skill_capability(
        "skill:incident-first",
        "incident-first",
        "Handle incident response runbooks and playbooks.",
        "listed",
    );
    let second = make_skill_capability(
        "skill:incident-second",
        "incident-second",
        "Handle incident retrospectives and follow-up actions.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&first).map_err(|e| e.to_string())?;
            store.hub_register(&second).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register capabilities");

    let result = server
        .recommend_capability(Parameters(RecommendCapabilityParams {
            query: "incident response".to_string(),
            host: None,
            cap_type: Some("skill".to_string()),
            limit: 0,
            include_hidden: false,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_capability should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["count"], json!(1));
    assert_eq!(json["recommendations"].as_array().expect("array").len(), 1);
}
