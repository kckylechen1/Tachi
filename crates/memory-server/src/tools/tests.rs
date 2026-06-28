use super::*;

#[test]
fn facade_response_defaults_to_json_and_preserves_markdown_opt_in() {
    let raw = r#"{"flow_id":"flow_1","stage":"plan","state":"instruction_ready","tasks":[{"dispatch_id":"d1","state":"running","agent":"codex","task":"Fix search"}]}"#;
    let json = format_facade_response("Tachi shell plan", "plan", raw, None).unwrap();
    let value = serde_json::from_str::<Value>(&json).unwrap();
    assert_eq!(value["action"], "plan");
    assert_eq!(value["status"], "completed");
    assert_eq!(value["flow_id"], "flow_1");

    let markdown =
        format_facade_response("Tachi shell plan", "plan", raw, Some("markdown")).unwrap();
    assert!(markdown.starts_with("## Tachi shell plan"));
    assert!(markdown.contains("flow_id: `flow_1`"));
    assert!(markdown.contains("- `d1` running agent=codex - Fix search"));
}

#[test]
fn facade_response_markdown_parse_failure_is_visible() {
    let raw = "not json";
    let json = format_facade_response("Tachi shell plan", "plan", raw, None).unwrap();
    assert_eq!(json, raw);

    let err = format_facade_response("Tachi shell plan", "plan", raw, Some("markdown"))
        .expect_err("markdown formatting should fail on invalid JSON");
    assert!(err.contains("format Tachi shell plan markdown response"));
    assert!(err.contains("expected JSON"));
}

#[test]
fn facade_response_renders_recommend_and_profiles_as_markdown_tables() {
    let recommend_raw = r#"{
        "task": "harden the search path",
        "recommended_profile": "codex_55_review",
        "recommended_transport": "native_cli",
        "fallback_chain": ["codex_55_review", "kimi_arch"],
        "candidates": [
            {"profile": "codex_55_review", "role": "reviewer", "score": 91.5,
             "useful_rate": 0.84, "reasons": ["live_useful_rate=0.84", "secondary"]},
            {"profile": "kimi_arch", "role": "architect", "score": 77.0,
             "useful_rate": null, "reasons": ["mbit_fit"]}
        ]
    }"#;
    let recommend = format_facade_response(
        "Tachi task recommend",
        "recommend",
        recommend_raw,
        Some("markdown"),
    )
    .unwrap();
    assert!(
        recommend.starts_with("## Tachi task recommend"),
        "{recommend}"
    );
    assert!(
        recommend.contains("| profile | role | score | useful_rate | top reason |"),
        "{recommend}"
    );
    assert!(recommend.contains("codex_55_review"), "{recommend}");
    assert!(
        recommend.contains("recommended_profile: `codex_55_review`"),
        "{recommend}"
    );
    assert!(
        recommend.contains("fallback_chain: codex_55_review -> kimi_arch"),
        "{recommend}"
    );
    assert!(!recommend.contains("```json"), "{recommend}");

    // JSON remains the default when markdown is not requested.
    let recommend_json =
        format_facade_response("Tachi task recommend", "recommend", recommend_raw, None).unwrap();
    let recommend_value = serde_json::from_str::<Value>(&recommend_json).unwrap();
    assert_eq!(recommend_value["action"], "recommend");
    assert_eq!(recommend_value["status"], "completed");
    assert_eq!(recommend_value["recommended_profile"], "codex_55_review");

    let profiles_raw = r#"{
        "dispatch_profiles": [
            {"name": "codex_55_review", "role": "reviewer", "stage": "review",
             "backend": "codex",
             "mbit_card": {"stats": {"cost": 72, "precision": 95, "speed": 55},
                           "strong_against": ["regressions", "security"]}}
        ]
    }"#;
    let profiles = format_facade_response(
        "Tachi task profiles",
        "profiles",
        profiles_raw,
        Some("markdown"),
    )
    .unwrap();
    assert!(profiles.starts_with("## Tachi task profiles"), "{profiles}");
    assert!(
        profiles.contains(
            "| name | role | stage | backend | cost | precision | speed | strong_against |"
        ),
        "{profiles}"
    );
    assert!(profiles.contains("codex_55_review"), "{profiles}");
    assert!(profiles.contains("regressions, security"), "{profiles}");
    assert!(!profiles.contains("```json"), "{profiles}");
}

#[test]
fn task_wait_poll_delay_backs_off_to_cap() {
    let mut delay = TASK_WAIT_INITIAL_POLL_DELAY;
    assert_eq!(delay, StdDuration::from_millis(250));

    delay = next_task_wait_poll_delay(delay);
    assert_eq!(delay, StdDuration::from_millis(500));

    delay = next_task_wait_poll_delay(delay);
    assert_eq!(delay, StdDuration::from_secs(1));

    delay = next_task_wait_poll_delay(delay);
    assert_eq!(delay, StdDuration::from_secs(2));

    delay = next_task_wait_poll_delay(delay);
    assert_eq!(delay, TASK_WAIT_MAX_POLL_DELAY);
}

#[test]
fn local_skill_discovery_scans_host_skill_dirs() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original_home = std::env::var_os("HOME");
    let temp_home =
        std::env::temp_dir().join(format!("tachi-local-skill-test-{}", uuid::Uuid::new_v4()));
    let skill_dir = temp_home.join(".agents/skills/agent-only-probe");
    let duplicate_skill_dir = temp_home.join(".codex/skills/agent-only-probe");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::create_dir_all(&duplicate_skill_dir).expect("create duplicate skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: agent-only-probe\ndescription: Use for zhsearchprobe workflows\n---\n# Agent Only Probe\n",
    )
    .expect("write skill");
    std::fs::write(
        duplicate_skill_dir.join("SKILL.md"),
        "---\nname: agent-only-probe\ndescription: Use for zhsearchprobe workflows\n---\n# Agent Only Probe Duplicate\n",
    )
    .expect("write duplicate skill");
    std::env::set_var("HOME", &temp_home);

    let found = discover_local_host_skills("zhsearchprobe", 5);

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = std::fs::remove_dir_all(&temp_home);

    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].get("name").and_then(Value::as_str),
        Some("agent-only-probe")
    );
    assert_eq!(
        found[0].get("source").and_then(Value::as_str),
        Some("host_skill_dir")
    );
}

#[test]
fn local_skill_discovery_expands_common_chinese_queries() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original_home = std::env::var_os("HOME");
    let temp_home = std::env::temp_dir().join(format!(
        "tachi-local-skill-zh-test-{}",
        uuid::Uuid::new_v4()
    ));
    let skill_dir = temp_home.join(".codex/skills/gh-fix-ci");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: gh-fix-ci\ndescription: Inspect GitHub PR checks and fix failing CI workflows\n---\n# GH Fix CI\n",
    )
    .expect("write skill");
    std::env::set_var("HOME", &temp_home);

    let found = discover_local_host_skills("中文 代码审查 修复 CI", 5);

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = std::fs::remove_dir_all(&temp_home);

    assert!(
        found
            .iter()
            .any(|cap| cap.get("name").and_then(Value::as_str) == Some("gh-fix-ci")),
        "expected Chinese query aliases to find gh-fix-ci: {found:?}"
    );
}
