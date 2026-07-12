use super::*;
use std::collections::BTreeMap;

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

#[test]
fn native_tool_methods_do_not_accumulate_byte_identical_alias_bodies() {
    let mut bodies: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, source) in native_tool_sources() {
        for (name, body) in tool_method_bodies(path, source) {
            bodies.entry(body).or_default().push(name);
        }
    }

    let duplicates = bodies
        .values()
        .filter(|names| names.len() > 1)
        .map(|names| names.join(", "))
        .collect::<Vec<_>>();
    assert!(
        duplicates.is_empty(),
        "new #[tool] methods must not be byte-identical aliases; route through a facade action or share a handler instead: {duplicates:?}"
    );
}

fn native_tool_sources() -> Vec<(&'static str, &'static str)> {
    vec![
        ("src/tools.rs", include_str!("../tools.rs")),
        (
            "src/tools/continuity_facade.rs",
            include_str!("continuity_facade.rs"),
        ),
        (
            "src/tools/component_facade.rs",
            include_str!("component_facade.rs"),
        ),
        (
            "src/tools/dispatch_facade.rs",
            include_str!("dispatch_facade.rs"),
        ),
        (
            "src/tools/handoff_facade.rs",
            include_str!("handoff_facade.rs"),
        ),
        ("src/tools/hub_facade.rs", include_str!("hub_facade.rs")),
        (
            "src/tools/memory_facade.rs",
            include_str!("memory_facade.rs"),
        ),
        (
            "src/tools/pipeline_facade.rs",
            include_str!("pipeline_facade.rs"),
        ),
        (
            "src/tools/runtime_context_facade.rs",
            include_str!("runtime_context_facade.rs"),
        ),
        (
            "src/tools/sandbox_facade.rs",
            include_str!("sandbox_facade.rs"),
        ),
        ("src/tools/vault_facade.rs", include_str!("vault_facade.rs")),
        ("src/tools/wiki_facade.rs", include_str!("wiki_facade.rs")),
        (
            "src/tools/workflow_facade.rs",
            include_str!("workflow_facade.rs"),
        ),
    ]
}

fn tool_method_bodies(path: &str, source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while let Some(relative) = source[offset..].find("pub(crate) async fn ") {
        let fn_start = offset + relative;
        let preceding = &source[..fn_start];
        let recent = &preceding[preceding.len().saturating_sub(512)..];
        if !recent.contains("#[tool") {
            offset = fn_start + "pub(crate) async fn ".len();
            continue;
        }

        let name_start = fn_start + "pub(crate) async fn ".len();
        let name_end = source[name_start..]
            .find('(')
            .map(|idx| name_start + idx)
            .unwrap_or(source.len());
        let name = &source[name_start..name_end];
        let Some(body_start) = source[name_end..].find('{').map(|idx| name_end + idx) else {
            offset = name_end;
            continue;
        };
        let Some(body_end) = matching_brace_end(source, body_start) else {
            panic!("failed to parse tool method body for {path}::{name}");
        };
        let body = source[body_start..=body_end].trim().to_string();
        out.push((format!("{path}::{name}"), body));
        offset = body_end + 1;
    }
    out
}

fn matching_brace_end(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut escaped = false;
    let mut idx = open;
    while idx < bytes.len() {
        let b = bytes[idx];
        let next = bytes.get(idx + 1).copied();
        if in_line_comment {
            in_line_comment = b != b'\n';
            idx += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') {
                in_block_comment = false;
                idx += 2;
            } else {
                idx += 1;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            idx += 1;
            continue;
        }

        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            idx += 2;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            idx += 2;
            continue;
        }
        if b == b'"' {
            in_string = true;
            idx += 1;
            continue;
        }
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(idx);
            }
        }
        idx += 1;
    }
    None
}
