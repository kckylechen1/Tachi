use crate::server_state::MemoryServer;

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_agent_id(server: &MemoryServer) -> Option<String> {
    let guard = server.agent_runtime_read();
    guard
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.trim().to_string())
        .filter(|agent_id| !agent_id.is_empty())
}

pub(super) fn fallback_agent_id(registered_agent: Option<String>) -> String {
    registered_agent
        .or_else(|| non_empty_env("TACHI_PROFILE"))
        .unwrap_or_else(|| "unknown-agent".to_string())
}

pub(super) fn resolve_from_agent(server: &MemoryServer) -> String {
    fallback_agent_id(current_agent_id(server))
}
