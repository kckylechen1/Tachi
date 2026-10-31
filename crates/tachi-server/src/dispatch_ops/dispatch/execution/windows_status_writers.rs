use super::*;

#[test]
fn acp_acknowledgement_uses_the_private_status_fence() {
    let temp = tempfile::tempdir().unwrap();
    let run = temp.path();
    let receipt = DispatchIdentityReceipt::planned(
        tachi_dispatch::DispatchIdentityRequest {
            profile: None,
            model: Some("gpt-5.5".into()),
            agent: Some("codex".into()),
            harness: Some("acp".into()),
        },
        acp_model_only_identity("gpt-5.5"),
        "Windows native status fence".into(),
        false,
    );
    std::fs::write(
        run.join("status.json"),
        serde_json::to_vec(&json!({"identity_receipt": receipt, "project": "preserved"})).unwrap(),
    )
    .unwrap();
    assert!(persist_acp_model_acknowledgement(run, Some("gpt-5.5")).unwrap());
    let before = std::fs::read(run.join("status.json")).unwrap();
    let status: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(status["project"], "preserved");
    assert_eq!(status["status_revision"], 1);
    std::fs::hard_link(
        run.join(".status-mutation.lock"),
        run.join("invalid-fence-link"),
    )
    .unwrap();
    assert!(persist_acp_model_acknowledgement(run, Some("gpt-5.5")).is_err());
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
}
