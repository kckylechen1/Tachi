use super::*;

#[test]
fn reasoning_lane_declares_zhipu_key_aliases() {
    const KEY: &str = "TACHI_TEST_ONLY_ZAI_ALIAS_KEY";
    std::env::set_var(KEY, "zai-value");

    let client = LlmClient::new().expect("client should initialize");

    assert_eq!(
        client.provider_secret_for_tests(&[KEY]),
        Some("zai-value".to_string())
    );
    assert!(client.reasoning.api_key_envs.contains(&"ZAI_API_KEY"));
    assert!(client.reasoning.api_key_envs.contains(&"BIGMODEL_API_KEY"));
    assert!(client.distill.api_key_envs.contains(&"ZAI_API_KEY"));
    assert!(client.distill.api_key_envs.contains(&"BIGMODEL_API_KEY"));
    std::env::remove_var(KEY);
}

#[test]
fn extract_json_payload_ignores_prefix_and_suffix() {
    let raw = "<think>ignore</think>\n{\"ok\": true}\nextra text";
    assert_eq!(
        LlmClient::extract_json_payload(raw).expect("json payload"),
        "{\"ok\": true}"
    );
}
