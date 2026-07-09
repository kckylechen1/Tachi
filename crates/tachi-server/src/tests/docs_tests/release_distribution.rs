//! Frozen assertions for release/distribution operating docs (#728/#758/#874).

#[test]
fn release_distribution_doc_names_trust_split_and_running_binary_gate() {
    let body = include_str!("../../../../../docs/engineering/architecture/release-distribution.md");
    assert!(
        body.contains("homebrew-tachi"),
        "must name the public distribution tap"
    );
    assert!(
        body.contains("Private") || body.contains("private"),
        "must describe private source surface"
    );
    assert!(
        body.contains("runtime.build.git_sha") || body.contains("git_sha"),
        "must document the running-binary deploy gate"
    );
    assert!(
        body.contains("#728") && body.contains("#874") && body.contains("#758"),
        "must map the three related issues"
    );
    assert!(
        !body.contains("public tap into a support forum")
            || body.contains("not a capability/source authority")
            || body.contains("not") && body.contains("design truth"),
        "must keep tap non-authoritative for design"
    );
}

#[test]
fn node_floor_decision_defers_commander_15() {
    let body =
        include_str!("../../../../../docs/engineering/decisions/2026-07-09-node-runtime-floor.md");
    assert!(body.contains("#853"));
    assert!(
        body.contains("Do not raise the Node floor") || body.contains("not raise the Node floor"),
        "decision must defer Node 22.12 floor"
    );
    assert!(body.contains("commander"));
}

#[test]
fn audit_586_residual_postpones_typed_errors_only() {
    let body =
        include_str!("../../../../../docs/engineering/decisions/2026-07-09-audit-586-residual.md");
    assert!(body.contains("#586"));
    assert!(body.contains("#547"));
    assert!(
        body.contains("CRITICAL") && body.contains("closed"),
        "must record CRITICAL closure"
    );
}
