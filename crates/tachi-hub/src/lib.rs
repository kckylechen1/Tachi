//! Shared Hub capability rules and response helpers.
//!
//! This crate intentionally stays below `memory-server`: it owns pure Hub
//! policy and MCP shape helpers, while DB access, LLM calls, proxy routing, and
//! CLI handlers remain in `memory-server`.

mod capability;
mod security_scan;
mod skill_execution;

pub use capability::{
    build_skill_tool_from_cap, capability_callable, capability_visibility_for_cap,
    capability_visibility_from_definition, health_status_allows_call, make_text_tool_result,
    review_status_allows_call, sanitize_skill_tool_name, should_expose_mcp_tools,
    should_expose_skill_tool, CapabilityVisibility,
};
pub use security_scan::{
    merge_skill_scans, normalize_review_status, resolve_security_scan_backend,
    scan_skill_definition, SecurityScanBackend,
};
pub use skill_execution::{
    build_skill_execution_envelope, SkillExecution, SkillExecutionMode,
    SIMULATED_SKILL_OUTPUT_MARKER, SIMULATED_SKILL_OUTPUT_WARNING,
};
