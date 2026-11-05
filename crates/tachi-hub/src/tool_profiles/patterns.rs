/// Default Agent tools and read-only connection discovery. Management routes
/// are callable only through separately admitted owner operations.
pub const STANDARD_MINIMAL_TOOL_PATTERNS: &[&str] = &[
    "tachi_memory",
    "tachi_task",
    "tachi_agent_eval",
    "tachi_staff",
    "tachi_a2a",
    "vault_connections",
];

/// Worker profile allow-list. Discovery matches the Lead product surface;
/// action policy still denies recursive staffing.
pub const DELEGATE_MINIMAL_TOOL_PATTERNS: &[&str] = &[
    "tachi_memory",
    "tachi_task",
    "tachi_staff",
    "tachi_a2a",
    "vault_connections",
];
