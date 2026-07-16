use super::*;

/// tachi#925 defect 2: a save/checkpoint whose requested `scope` is silently
/// downgraded to a different effective `db_scope` (e.g. `scope=project` on a
/// single-DB daemon with no project DB bound, falling back to global) must
/// say so in the response — not just in provenance metadata nobody reads.
#[tokio::test]
async fn tachi_memory_save_scope_downgrade_emits_scope_warning_json() {
    // `make_server()` is single-DB: no project DB is bound, so a requested
    // `scope=project` cannot be honored and falls back to global.
    let server = make_server();

    let mut params = tachi_memory_params("save");
    params.format = Some("json".to_string());
    params.scope = Some("project".to_string());
    params.text = Some("Should have landed in the project DB.".to_string());
    params.path = Some("/scratch/scope-downgrade".to_string());

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("save should succeed even when scope falls back");
    let parsed: Value = serde_json::from_str(&body).expect("save JSON");

    assert_eq!(
        parsed.get("scope").and_then(Value::as_str),
        Some("project"),
        "expected the requested scope to be echoed back: {parsed}"
    );
    let scope_warning = parsed
        .get("scope_warning")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a scope_warning on downgrade: {parsed}"));
    assert!(
        scope_warning.contains("requested scope was not honored"),
        "expected the stable #925 scope-downgrade warning prefix: {scope_warning}"
    );
}

/// Matching scope (request honored) must NOT carry a scope_warning.
#[tokio::test]
async fn tachi_memory_save_matching_scope_has_no_scope_warning() {
    let server = make_server();

    let mut params = tachi_memory_params("save");
    params.format = Some("json".to_string());
    // "global" is always honored, even on a single-DB daemon.
    params.scope = Some("global".to_string());
    params.text = Some("Global scope requested and honored.".to_string());
    params.path = Some("/scratch/scope-match".to_string());

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("save should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("save JSON");

    assert!(
        parsed.get("scope_warning").is_none(),
        "matching scope should not carry a scope_warning: {parsed}"
    );
    assert!(
        parsed.get("scope").is_none(),
        "matching scope should not carry a scope field (only present on downgrade): {parsed}"
    );
}

/// tachi#1176: an UNBOUND session's scope_warning must not send the caller in
/// a circle. `reject_unbound_cross_project_write` hard-rejects an explicit
/// `project=<name>` from an unbound session (-32602) on the very next call —
/// so the downgrade warning must not tell an unbound session to do exactly
/// that. RED on origin/main (the old message always said "pass project=").
#[tokio::test]
async fn tachi_memory_save_scope_downgrade_unbound_session_warning_is_actionable_not_circular() {
    // `make_server()` never calls `set_session_identity`, so
    // `session_project()` is `None` — the same "unbound" signal the C1 guard
    // (`reject_unbound_cross_project_write`) checks.
    let server = make_server();

    let mut params = tachi_memory_params("save");
    params.format = Some("json".to_string());
    params.scope = Some("project".to_string());
    params.text = Some("Unbound session should not be told to pass project=.".to_string());
    params.path = Some("/scratch/scope-downgrade-unbound".to_string());

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("save should succeed even when scope falls back");
    let parsed: Value = serde_json::from_str(&body).expect("save JSON");

    let scope_warning = parsed
        .get("scope_warning")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a scope_warning on downgrade: {parsed}"));
    assert!(
        !scope_warning.contains("pass project="),
        "unbound session's scope_warning must not suggest project=<name> \
         (it is hard-rejected by reject_unbound_cross_project_write on the \
         very next call — #1176): {scope_warning}"
    );
    assert!(
        scope_warning.contains("connect a bound session"),
        "unbound session's scope_warning must point at how to become bound \
         instead: {scope_warning}"
    );
}

/// tachi#1176 counterpart: a BOUND session's scope_warning is unchanged —
/// `pass project=<name>` is genuinely actionable there, since the C1 guard
/// only rejects explicit `project=` from an UNBOUND session.
#[tokio::test]
async fn tachi_memory_save_scope_downgrade_bound_session_still_suggests_project() {
    let server = make_server();
    server.set_session_identity(None, Some("bound-project".to_string()), None);

    let mut params = tachi_memory_params("save");
    params.format = Some("json".to_string());
    params.scope = Some("project".to_string());
    params.text = Some("Bound session keeps the pass project= advice.".to_string());
    params.path = Some("/scratch/scope-downgrade-bound".to_string());

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("save should succeed even when scope falls back");
    let parsed: Value = serde_json::from_str(&body).expect("save JSON");

    let scope_warning = parsed
        .get("scope_warning")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a scope_warning on downgrade: {parsed}"));
    assert!(
        scope_warning.contains("pass project=<name>"),
        "bound session's scope_warning must keep the actionable project= \
         advice unchanged: {scope_warning}"
    );
}

/// Same regression, through the checkpoint action (#925 explicitly calls out
/// "emit on save AND checkpoint" since checkpoint routes through the same
/// save_memory response assembly via `handle_tachi_save`).
#[tokio::test]
async fn tachi_memory_checkpoint_scope_downgrade_emits_scope_warning_json() {
    let server = make_server();

    let mut params = tachi_memory_params("checkpoint");
    params.format = Some("json".to_string());
    params.scope = Some("project".to_string());
    params.text = Some("Checkpoint content that should have landed in project.".to_string());
    params.title = Some("Scope downgrade checkpoint".to_string());

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("checkpoint should succeed even when scope falls back");
    let parsed: Value = serde_json::from_str(&body).expect("checkpoint JSON");

    assert_eq!(
        parsed.get("scope").and_then(Value::as_str),
        Some("project"),
        "expected the requested scope to be echoed back: {parsed}"
    );
    let scope_warning = parsed
        .get("scope_warning")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a scope_warning on checkpoint downgrade: {parsed}"));
    assert!(
        scope_warning.contains("requested scope was not honored"),
        "expected the stable #925 scope-downgrade warning prefix: {scope_warning}"
    );
}
