use super::*;
use std::collections::BTreeMap;

#[test]
fn facade_response_defaults_to_json_and_preserves_markdown_opt_in() {
    let raw = r#"{"flow_id":"flow_1","stage":"build","state":"instruction_ready","tasks":[{"dispatch_id":"d1","state":"running","agent":"codex","task":"Fix search"}]}"#;
    let json =
        format_facade_response("Tachi task status", "status", raw, None, false).unwrap();
    let value = serde_json::from_str::<Value>(&json).unwrap();
    assert_eq!(value["action"], "status");
    assert_eq!(value["status"], "completed");
    assert_eq!(value["flow_id"], "flow_1");

    let markdown = format_facade_response(
        "Tachi task status",
        "status",
        raw,
        Some("markdown"),
        false,
    )
    .unwrap();
    assert!(markdown.starts_with("## Tachi task status"));
    assert!(markdown.contains("flow_id: `flow_1`"));
    assert!(markdown.contains("- `d1` running agent=codex - Fix search"));
}

#[test]
fn facade_response_markdown_parse_failure_is_visible() {
    let raw = "not json";
    let json =
        format_facade_response("Tachi task status", "status", raw, None, false).unwrap();
    assert_eq!(json, raw);

    let err = format_facade_response(
        "Tachi task status",
        "status",
        raw,
        Some("markdown"),
        false,
    )
    .expect_err("markdown formatting should fail on invalid JSON");
    assert!(err.contains("format Tachi task status markdown response"));
    assert!(err.contains("expected JSON"));
}

#[tokio::test]
async fn facade_board_markdown_surfaces_capped_empty_response() {
    let (server, _temp_home) = crate::tests::make_server_with_temp_home();
    let runs_dir = server.tachi_home_dir().join("runs");
    std::fs::create_dir_all(&runs_dir).expect("create runs dir");
    for index in 0..=50 {
        std::fs::write(runs_dir.join(format!("ignored-{index:04}")), "fixture")
            .expect("write capped fallback fixture");
    }
    let raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        crate::tool_params::TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(1),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("bounded board response");

    let markdown = format_facade_response(
        "Tachi task board",
        "board",
        &raw,
        Some("markdown"),
        false,
    )
    .expect("format incomplete board");

    assert!(markdown.contains("incomplete: `true`"), "{markdown}");
    assert!(
        markdown.contains("run_fallback_scan_truncated"),
        "{markdown}"
    );
    assert!(
        markdown.contains("directory order is not a recency index"),
        "{markdown}"
    );
    assert!(
        !markdown.contains("_No board data._"),
        "an incomplete empty board must not render as clean: {markdown}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn facade_board_markdown_surfaces_invalid_empty_fallback() {
    let (server, _temp_home) = crate::tests::make_server_with_temp_home();
    let runs_dir = server.tachi_home_dir().join("runs");
    std::fs::create_dir_all(&runs_dir).expect("create runs dir");
    let outside = server.tachi_home_dir().join("outside-run");
    std::fs::create_dir_all(&outside).expect("create outside run");
    std::os::unix::fs::symlink(&outside, runs_dir.join("20260725T000000Z-invalid-link"))
        .expect("symlink invalid fallback run");
    let raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        crate::tool_params::TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(1),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("invalid fallback board response");

    let markdown = format_facade_response(
        "Tachi task board",
        "board",
        &raw,
        Some("markdown"),
        false,
    )
    .expect("format invalid fallback board");

    assert!(markdown.contains("incomplete: `true`"), "{markdown}");
    assert!(
        markdown.contains("run_fallback_invalid_entries"),
        "{markdown}"
    );
    assert!(
        markdown.contains("run_scan_invalid_entries: `1`"),
        "{markdown}"
    );
}

#[test]
fn facade_response_renders_profiles_as_markdown_tables() {
    // tachi#1201 item 1: profiles now default to markdown when format is
    // omitted entirely — flip scoped to profile discovery actions,
    // not the shared `wants_json` global default.
    // #1182 checkpoint 4 (codex review round 2): tachi#1173 item 2's default
    // `dispatch_profiles_json_for_server` shape is the slim row
    // (name/backend/model/role, `verbose: false` echoed, no `mbit_card`) —
    // exercise that real default shape here instead of only a hand-built
    // full-card fixture, so this test would actually catch a markdown
    // renderer that still assumes the pre-#1173 always-full-card shape.
    let profiles_slim_raw = r#"{
        "verbose": false,
        "dispatch_profiles": [
            {"name": "codex_55_review", "backend": "codex", "model": "gpt-5.5", "role": "reviewer"}
        ]
    }"#;
    let profiles_slim = format_facade_response(
        "Tachi task profiles",
        "profiles",
        profiles_slim_raw,
        Some("markdown"),
        false,
    )
    .unwrap();
    assert!(
        profiles_slim.starts_with("## Tachi task profiles"),
        "{profiles_slim}"
    );
    assert!(
        profiles_slim.contains("| name | backend | model | role |"),
        "{profiles_slim}"
    );
    assert!(profiles_slim.contains("codex_55_review"), "{profiles_slim}");
    assert!(profiles_slim.contains("gpt-5.5"), "{profiles_slim}");
    assert!(!profiles_slim.contains("```json"), "{profiles_slim}");
    // The slim default must not silently fall back to the full-card column
    // set (which would render every stats column as "-").
    assert!(!profiles_slim.contains("precision"), "{profiles_slim}");

    // tachi#1201 item 1: action='profiles' also defaults to markdown when
    // format is omitted.
    let profiles_default = format_facade_response(
        "Tachi task profiles",
        "profiles",
        profiles_slim_raw,
        None,
        false,
    )
    .unwrap();
    assert!(
        profiles_default.starts_with("## Tachi task profiles"),
        "{profiles_default}"
    );
    assert!(
        serde_json::from_str::<Value>(&profiles_default).is_err(),
        "action='profiles' with format omitted must default to markdown, not JSON: {profiles_default}"
    );

    // verbose=true preserves the pre-#1173 full-card table shape.
    let profiles_verbose_raw = r#"{
        "verbose": true,
        "dispatch_profiles": [
            {"name": "codex_55_review", "role": "reviewer", "stage": "review",
             "backend": "codex",
             "mbit_card": {"stats": {"cost": 72, "precision": 95, "speed": 55},
                           "strong_against": ["regressions", "security"]}}
        ]
    }"#;
    let profiles_verbose = format_facade_response(
        "Tachi task profiles",
        "profiles",
        profiles_verbose_raw,
        Some("markdown"),
        false,
    )
    .unwrap();
    assert!(
        profiles_verbose.starts_with("## Tachi task profiles"),
        "{profiles_verbose}"
    );
    assert!(
        profiles_verbose.contains(
            "| name | role | stage | backend | cost | precision | speed | strong_against |"
        ),
        "{profiles_verbose}"
    );
    assert!(
        profiles_verbose.contains("codex_55_review"),
        "{profiles_verbose}"
    );
    assert!(
        profiles_verbose.contains("regressions, security"),
        "{profiles_verbose}"
    );
    assert!(!profiles_verbose.contains("```json"), "{profiles_verbose}");
}

#[test]
fn local_skill_discovery_scans_host_skill_dirs() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original_home = std::env::var_os("HOME");
    let temp_home =
        crate::utils::test_fixture_path(format!("tachi-local-skill-test-{}", uuid::Uuid::new_v4()));
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
    let temp_home = crate::utils::test_fixture_path(format!(
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
