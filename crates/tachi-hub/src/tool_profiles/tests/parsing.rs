use super::*;

#[test]
fn profile_parsing_maps_host_aliases() {
    // IDE + CLI → standard
    assert_eq!(parse_tool_profile("lead"), Some(ToolProfile::standard()));
    assert_eq!(parse_tool_profile("codex"), Some(ToolProfile::standard()));
    assert_eq!(parse_tool_profile("cursor"), Some(ToolProfile::standard()));
    assert_eq!(
        parse_tool_profile("windsurf"),
        Some(ToolProfile::standard())
    );
    assert_eq!(
        parse_tool_profile("antigravity"),
        Some(ToolProfile::standard())
    );
    assert_eq!(
        parse_tool_profile("claude-code"),
        Some(ToolProfile::standard())
    );
    assert_eq!(parse_tool_profile("ide"), Some(ToolProfile::standard()));
    // Worker agents → delegate
    assert_eq!(
        parse_tool_profile("delegate"),
        Some(ToolProfile::delegate())
    );
    assert_eq!(parse_tool_profile("worker"), Some(ToolProfile::delegate()));
    assert_eq!(
        parse_tool_profile("subagent"),
        Some(ToolProfile::delegate())
    );
    assert_eq!(
        parse_tool_profile("companion"),
        Some(ToolProfile::standard())
    );
    assert_eq!(
        parse_tool_profile("workflow"),
        Some(ToolProfile::standard())
    );
    // Existing `agent` configurations retain their narrower write policy.
    assert_eq!(parse_tool_profile("agent"), Some(ToolProfile::remember()));
    for retired in [
        "operate",
        "runtime",
        "openclaw",
        "hermes",
        "adapter",
        "ops",
        "admin",
        "full",
        "emergency",
    ] {
        assert_eq!(parse_tool_profile(retired), None, "{retired}");
        assert_eq!(parse_tool_profile(&format!("delegate+{retired}")), None);
    }
}

#[test]
fn profile_parsing_supports_additive_surface_tokens() {
    assert_eq!(
        parse_tool_profile("observe,coordinate"),
        Some(ToolProfile::observe().merge(ToolProfile::coordinate()))
    );
    assert_eq!(
        parse_tool_profile("remember+coordinate"),
        Some(ToolProfile::remember().merge(ToolProfile::coordinate()))
    );
}

#[test]
fn additive_minimal_profiles_have_one_consistent_precedence_policy() {
    let worker = parse_tool_profile("delegate").expect("Worker restriction");
    assert!(parse_tool_profile("delegate+operate").is_none());
    assert_eq!(worker.as_str(), "delegate");
    assert!(!facade_action_allowed(
        "tachi_staff",
        Some("start"),
        Some(worker)
    ));

    let lead_worker = parse_tool_profile("standard+delegate").expect("additive Lead profile");
    assert_eq!(lead_worker.as_str(), "standard");
    for tool in ["tachi_memory", "tachi_task", "tachi_staff", "tachi_a2a"] {
        assert!(tool_visible(tool, Some(lead_worker), None), "{tool}");
    }
    assert!(!tool_visible("runtime_info", Some(lead_worker), None));
    assert!(facade_action_allowed(
        "tachi_staff",
        Some("start"),
        Some(lead_worker)
    ));
    assert!(!tool_visible("tachi_gh", Some(lead_worker), None));
}

#[test]
fn unknown_and_spoofed_profiles_cannot_select_a_broad_surface() {
    for raw in [
        "unknown-principal",
        "worker+admin",
        "lead,full",
        "ops+emergency",
    ] {
        assert_eq!(
            parse_tool_profile(raw),
            None,
            "'{raw}' must not resolve to a broad profile"
        );
    }
}

#[test]
fn pattern_matching_supports_wildcards() {
    assert!(tool_name_matches_pattern("hub_call", "hub_*"));
    assert!(!tool_name_matches_pattern("save_memory", "hub_*"));
}

#[test]
fn standard_and_delegate_labels() {
    assert_eq!(ToolProfile::standard().as_str(), "standard");
    assert_eq!(ToolProfile::delegate().as_str(), "delegate");
    // admin still wins over minimal flags if explicitly merged.
    assert_eq!(
        ToolProfile::standard().merge(ToolProfile::admin()).as_str(),
        "admin"
    );
    // standard wins over delegate if both set.
    assert_eq!(
        ToolProfile::delegate()
            .merge(ToolProfile::standard())
            .as_str(),
        "standard"
    );
}
