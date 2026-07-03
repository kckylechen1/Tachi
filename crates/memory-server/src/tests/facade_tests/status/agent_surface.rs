use super::*;
use std::collections::BTreeSet;

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

#[tokio::test]
async fn tachi_status_slims_healthy_provider_health_and_omits_empty_continuity() {
    let (server, _temp_home) = make_server_with_temp_home();
    let body = crate::status_ops::handle_tachi_status_agent(&server)
        .await
        .expect("agent status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("agent status JSON");

    let provider_health = parsed["api_keys"]["provider_health"]
        .as_object()
        .expect("provider health object");
    let keys = provider_health.keys().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        keys,
        BTreeSet::from([
            "last_success_age_secs".to_string(),
            "source_of_truth".to_string(),
            "status".to_string()
        ])
    );
    assert_eq!(provider_health.get("status"), Some(&json!("ok")));
    assert!(
        !parsed
            .as_object()
            .expect("status object")
            .contains_key("continuity"),
        "empty continuity block should be omitted from agent status: {body}"
    );
}
