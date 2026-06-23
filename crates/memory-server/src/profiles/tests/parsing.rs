use super::*;

#[test]
fn profile_parsing_maps_host_aliases() {
    // IDE + CLI → standard
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
    // Framework agents → operate
    assert_eq!(parse_tool_profile("openclaw"), Some(ToolProfile::operate()));
    assert_eq!(parse_tool_profile("hermes"), Some(ToolProfile::operate()));
    assert_eq!(
        parse_tool_profile("companion"),
        Some(
            ToolProfile::remember()
                .merge(ToolProfile::coordinate())
                .merge(ToolProfile::operate())
        )
    );
    assert_eq!(
        parse_tool_profile("workflow"),
        Some(ToolProfile::coordinate().merge(ToolProfile::operate()))
    );
    assert_eq!(parse_tool_profile("admin"), Some(ToolProfile::admin()));
}

#[test]
fn profile_parsing_supports_additive_surface_tokens() {
    assert_eq!(
        parse_tool_profile("observe,coordinate"),
        Some(ToolProfile::observe().merge(ToolProfile::coordinate()))
    );
    assert_eq!(
        parse_tool_profile("remember+operate"),
        Some(ToolProfile::remember().merge(ToolProfile::operate()))
    );
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
