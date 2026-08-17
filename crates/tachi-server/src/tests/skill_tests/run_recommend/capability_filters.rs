use super::*;

// #1690 C3 slice A: the `recommend_capability`/`recommend_skill` MCP routes are
// retired with the "second model brain". The scoring core they exposed
// (`recommend_capabilities_inner`) SURVIVES because the kept
// `handle_prepare_capability_bundle` (tachi_skill bundle/loadout) still calls
// it — so the frozen filter semantics these tests guarded (hidden-capability
// exclusion, limit normalization) are re-anchored onto that live entry point.

#[tokio::test]
async fn recommend_capabilities_inner_skips_hidden_capabilities_by_default() {
    let server = make_server();
    let hidden = make_skill_capability(
        "skill:hidden-playbook",
        "hidden-playbook",
        "Handle sensitive internal incident playbooks.",
        "hidden",
    );
    let visible = make_skill_capability(
        "skill:incident-playbook",
        "incident-playbook",
        "Handle incident response playbooks.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&hidden).map_err(|e| e.to_string())?;
            store.hub_register(&visible).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register capabilities");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "incident playbook",
        None,
        Some("skill"),
        5,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    let ids = results.iter().map(|rec| rec.id.as_str()).collect::<Vec<_>>();
    assert!(ids.contains(&"skill:incident-playbook"));
    assert!(!ids.contains(&"skill:hidden-playbook"));

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "incident playbook",
        None,
        Some("skill"),
        5,
        true,
        false,
    )
    .expect("recommend capabilities include_hidden should succeed");
    let ids = results.iter().map(|rec| rec.id.as_str()).collect::<Vec<_>>();
    assert!(ids.contains(&"skill:hidden-playbook"));
}

#[tokio::test]
async fn recommend_capabilities_inner_limit_zero_normalizes_to_one() {
    let server = make_server();
    let first = make_skill_capability(
        "skill:incident-first",
        "incident-first",
        "Handle incident response runbooks and playbooks.",
        "listed",
    );
    let second = make_skill_capability(
        "skill:incident-second",
        "incident-second",
        "Handle incident retrospectives and follow-up actions.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&first).map_err(|e| e.to_string())?;
            store.hub_register(&second).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register capabilities");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "incident response",
        None,
        Some("skill"),
        0,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    assert_eq!(results.len(), 1);
}
