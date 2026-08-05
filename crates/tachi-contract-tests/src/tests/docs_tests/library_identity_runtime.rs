//! Frozen assertions for #746 library identity runtime contract.

#[test]
fn library_identity_runtime_doc_names_contract_surfaces() {
    let body =
        include_str!("../../../../../docs/engineering/architecture/library-identity-runtime.md");
    for needle in [
        "#746",
        "#732",
        "#737",
        "x-tachi-project",
        "tachiProject",
        "X-Tachi-Project",
        "cross-library",
        "single-writer",
        "session_identity",
        "HTTP direct-connect",
        "stdio",
        "Plan C",
        "worktree",
        "vector",
        "explicit_project_can_cross_binding",
    ] {
        assert!(
            body.contains(needle),
            "library-identity-runtime.md must mention {needle}"
        );
    }
}

#[test]
fn multi_project_daemon_points_at_runtime_contract() {
    let body = include_str!("../../../../../docs/engineering/architecture/multi-project-daemon.md");
    assert!(
        body.contains("library-identity-runtime.md"),
        "multi-project-daemon.md must link the current-state runtime contract"
    );
    assert!(
        body.contains("#746") || body.contains("library-identity"),
        "must reference the library-identity track"
    );
}
