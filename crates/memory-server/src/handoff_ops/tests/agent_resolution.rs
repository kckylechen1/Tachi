use super::*;

#[test]
fn resolve_from_agent_falls_back_to_profile_env_then_unknown() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let original_profile = std::env::var_os("TACHI_PROFILE");
    std::env::remove_var("TACHI_PROFILE");
    assert_eq!(fallback_agent_id(None), "unknown-agent");

    std::env::set_var("TACHI_PROFILE", "antigravity");
    assert_eq!(fallback_agent_id(None), "antigravity");
    assert_eq!(
        fallback_agent_id(Some("registered".to_string())),
        "registered"
    );

    if let Some(profile) = original_profile {
        std::env::set_var("TACHI_PROFILE", profile);
    } else {
        std::env::remove_var("TACHI_PROFILE");
    }
}
