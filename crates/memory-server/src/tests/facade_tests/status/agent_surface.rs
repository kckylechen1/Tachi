use super::*;

#[tokio::test]
async fn tachi_status_agent_surface_is_compact() {
    let (server, _temp_home) = make_server_with_temp_home();
    let body = crate::status_ops::handle_tachi_status_agent(&server)
        .await
        .expect("agent status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("agent status JSON");
    // Compact surface: runtime is slim (the heavy provider arrays are omitted;
    // they live in handle_tachi_status_full + runtime_info).
    assert!(parsed["runtime"]["provider_pools"].is_null());
    assert!(parsed["runtime"]["provider_health"].is_null());
    assert!(
        parsed["runtime"]["provider_secret_count"]
            .as_u64()
            .is_some(),
        "slim runtime still keeps the secret count"
    );
    // api_keys.provider_pools collapses to a {total, rate_limited} summary.
    assert!(
        parsed["api_keys"]["provider_pools"]["total"]
            .as_u64()
            .is_some(),
        "compact api_keys.provider_pools should be a summary, not the full array: {body}"
    );
    assert!(parsed["api_keys"]["provider_pools"].as_array().is_none());
}
