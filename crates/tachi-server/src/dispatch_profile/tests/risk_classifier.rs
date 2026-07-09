use super::*;

#[test]
fn risk_classifier_uses_touched_area_and_missing_verification_signals() {
    let risk = classify_dispatch_risk(
        "review changes in crates/tachi-server/src/agent_eval.rs and dispatch_profile.rs; tests not run",
        None,
        &[],
    );

    assert_eq!(risk.risk, "high");
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "touched_area:eval_ledger_changes"));
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "touched_area:dispatch_refactor"));
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "missing_verification_signal"));
    assert!(risk
        .required_profiles
        .iter()
        .any(|profile| profile == "codex_55_review"));
    assert!(risk
        .blocked_profiles
        .iter()
        .any(|profile| profile == "codex_53_fast"));
}

#[test]
fn risk_classifier_preserves_low_risk_research_route() {
    let risk = classify_dispatch_risk("research low-risk documentation wording", None, &[]);

    assert_eq!(risk.risk, "low");
    assert!(risk.blocked_profiles.is_empty());
}

#[test]
fn risk_classifier_marks_regression_hints_high() {
    let risk = classify_dispatch_risk(
        "fix a regression where the worker got stuck in a retry loop",
        None,
        &[],
    );

    assert_eq!(risk.risk, "high");
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "prior_failure_or_regression_hint"));
}

#[test]
fn risk_classifier_does_not_treat_plain_override_or_profile_as_failure() {
    let override_risk = classify_dispatch_risk(
        "document the config override behavior for normal settings",
        None,
        &[],
    );
    assert_ne!(override_risk.risk, "high");
    assert!(!override_risk
        .reasons
        .iter()
        .any(|reason| reason == "prior_failure_or_regression_hint"));

    let profile_risk = classify_dispatch_risk("review user profile page wording", None, &[]);
    assert_ne!(profile_risk.risk, "high");
}

#[test]
fn risk_classifier_escalates_on_sensitive_file_paths() {
    let paths = vec![
        "docs/notes.md".to_string(),
        "crates/tachi-server/src/vault_crypto.rs".to_string(),
    ];
    let risk = classify_dispatch_risk("plan a small docs update", None, &paths);

    assert_eq!(risk.risk, "high");
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "touches vault/secrets boundary"));
}

#[test]
fn risk_classifier_dedupes_text_and_path_signals() {
    let paths = vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()];
    let risk = classify_dispatch_risk("review dispatch profile changes", None, &paths);
    let count = risk
        .reasons
        .iter()
        .filter(|reason| *reason == "touches dispatch routing")
        .count();

    assert_eq!(risk.risk, "high");
    assert_eq!(count, 1);
}
