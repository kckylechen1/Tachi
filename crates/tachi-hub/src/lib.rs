//! Shared Hub capability rules and response helpers.
//!
//! This crate intentionally stays below `tachi-server`: it owns pure Hub
//! policy and MCP shape helpers, while DB access, LLM calls, proxy routing, and
//! CLI handlers remain in `tachi-server`.

mod capability;
mod security_scan;
mod tool_profiles;

pub use capability::{
    build_skill_tool_from_cap, capability_callable, capability_not_callable_reason,
    capability_visibility_for_cap, capability_visibility_from_definition,
    health_status_allows_call, is_supported_hub_capability_type, make_text_tool_result,
    review_status_allows_call, sanitize_skill_tool_name, should_expose_skill_tool,
    skill_document_content, CapabilityVisibility,
};
pub use security_scan::{
    merge_skill_scans, normalize_review_status, resolve_security_scan_backend,
    scan_skill_definition, SecurityScanBackend,
};
pub use tool_profiles::{
    default_tool_profile, facade_action_allowed, facade_action_required_bundle, filter_tool_defs,
    parse_tool_patterns_csv, parse_tool_profile, tool_name_matches_pattern, tool_visible,
    ToolBundle, ToolProfile, DELEGATE_MINIMAL_TOOL_PATTERNS, STANDARD_MINIMAL_TOOL_PATTERNS,
};
