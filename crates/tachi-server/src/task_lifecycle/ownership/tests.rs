use super::*;
use crate::managed_run_control::test_hooks::{install_status_io_hook, StatusIoHookStage};
use rmcp::handler::server::wrapper::Parameters;

fn local(server: &MemoryServer, project: &str, identity: &str) {
    server.set_session_identity(None, Some(project.into()), None);
    crate::claims_ops::admit_agent_connection(server, Some(identity.into()), true).unwrap();
}

fn params(action: &str, flow: Option<&str>) -> TachiTaskParams {
    serde_json::from_value(json!({"action":action,"flow_id":flow,"format":"full"})).unwrap()
}

fn claim(server: &MemoryServer, flow: &str, suffix: &str) -> String {
    let worktree = flow_runs_root()
        .parent()
        .unwrap()
        .join(format!("wt-{suffix}"));
    std::fs::create_dir_all(&worktree).unwrap();
    let p = serde_json::from_value(json!({"action":"claim","flow_id":flow,
        "issue_ref":"org/repo#123","claim_mode":"writable","claim_role":"executor",
        "claim_scope":[format!("{suffix}/**")],"worktree_path":worktree,
        "expected_head":"1234567","lease_expires_at":"2035-01-01T00:00:00Z"}))
    .unwrap();
    crate::claims_ops::handle_task_claim(server, &p).unwrap()["claim_id"]
        .as_str()
        .unwrap()
        .into()
}

fn issue() -> IssueSnapshot {
    IssueSnapshot {
        repo: "org/repo".into(),
        number: 123,
        title: "Private owned task".into(),
        body: Some("private body".into()),
        labels: vec![],
        state: Some("OPEN".into()),
        url: "https://github.com/org/repo/issues/123".into(),
        doc_paths: vec![],
        spec_paths: vec![],
    }
}

fn intake(server: &MemoryServer, flow: &str) -> Result<bool, String> {
    claimed_intake(
        server,
        LocalProject::capture(server),
        flow,
        "private task",
        &issue(),
        &json!({"dispatch_allowed":false}),
    )
}

fn snapshot(server: &MemoryServer) -> Value {
    super::super::scoped_work_read_section(server, &params("board", None)).unwrap()
}

fn db_path(server: &MemoryServer) -> PathBuf {
    server
        .with_global_store_read(|store| Ok(PathBuf::from(store.connection().path().unwrap())))
        .unwrap()
}

fn stable(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.remove("read_at");
    }
    if let Some(items) = value.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            item.as_object_mut().unwrap().remove("read_at");
        }
    }
    value
}

#[tokio::test]
async fn scoped_views_share_admitted_facts_and_ignore_foreign_rows_and_files() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_owned_alpha", "alpha");
    intake(&server, "flow_owned_alpha").unwrap();
    let before = stable(snapshot(&server));
    assert_eq!(before["available"], true);
    assert_eq!(before["items"].as_array().unwrap().len(), 1);
    let mut views = Vec::new();
    for action in ["board", "status", "brief"] {
        let raw = server
            .tachi_task(Parameters(params(action, Some("flow_owned_alpha"))))
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        views.push(stable(
            value
                .get("work_read_model")
                .or_else(|| value.pointer("/cycle/work_read_model"))
                .unwrap()
                .clone(),
        ));
    }
    assert_eq!(views[0], views[1]);
    assert_eq!(views[1], views[2]);
    local(&server, "beta", "agent.beta");
    claim(&server, "flow_owned_beta", "beta");
    intake(&server, "flow_owned_beta").unwrap();
    let foreign = run_dir_for_flow_id("flow_owned_beta").unwrap();
    // A foreign status may be corrupt; project alpha must not open it at all.
    std::fs::write(foreign.join("status.json"), "foreign corrupt sentinel").unwrap();
    local(&server, "alpha", "agent.alpha");
    assert_eq!(stable(snapshot(&server)), before);
    let foreign_selection =
        super::super::scoped_work_read_section(&server, &params("status", Some("flow_owned_beta")))
            .unwrap();
    assert!(foreign_selection["items"].as_array().unwrap().is_empty());
    assert!(foreign_selection["flows"].as_array().unwrap().is_empty());
    assert!(!foreign_selection.to_string().contains("beta"));
}

#[test]
fn ownership_never_adopts_legacy_or_ambiguous_flows() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_legacy", "one");
    let path = run_dir_for_flow_id("flow_legacy").unwrap();
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("status.json"), "{\"task\":\"legacy\"}").unwrap();
    assert_eq!(intake(&server, "flow_legacy").unwrap_err(), UNAVAILABLE);
    assert_eq!(
        std::fs::read_to_string(path.join("status.json")).unwrap(),
        "{\"task\":\"legacy\"}"
    );
    claim(&server, "flow_ambiguous", "two");
    claim(&server, "flow_ambiguous", "three");
    assert_eq!(intake(&server, "flow_ambiguous").unwrap_err(), UNAVAILABLE);
    assert!(!run_dir_for_flow_id("flow_ambiguous").unwrap().exists());
    let raw = rusqlite::Connection::open(db_path(&server)).unwrap();
    session_claims::insert_claim(
        &raw,
        &memcore::NewSessionClaim {
            claim_id: "legacy-presence".into(),
            flow_id: Some("flow_legacy_presence".into()),
            ..Default::default()
        },
    )
    .unwrap();
    claim(&server, "flow_legacy_presence", "four");
    assert_eq!(
        intake(&server, "flow_legacy_presence").unwrap_err(),
        UNAVAILABLE
    );
    assert!(!run_dir_for_flow_id("flow_legacy_presence")
        .unwrap()
        .exists());
    let expired = claim(&server, "flow_expired_claim", "expired");
    raw.execute(
        "UPDATE session_claims SET lease_expires_at='2000-01-01T00:00:00.000Z' WHERE claim_id=?1",
        [expired],
    )
    .unwrap();
    assert_eq!(
        intake(&server, "flow_expired_claim").unwrap_err(),
        UNAVAILABLE
    );
    assert!(!run_dir_for_flow_id("flow_expired_claim").unwrap().exists());
    assert_eq!(intake(&server, "flow_no_claim").unwrap_err(), UNAVAILABLE);
    local(&server, "beta", "agent.beta");
    assert_eq!(intake(&server, "flow_ambiguous").unwrap_err(), UNAVAILABLE);
}

#[test]
fn revoked_admission_refuses_before_files_and_sealing_holds_the_sqlite_writer_lock() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_revoke", "one");
    let connection = server.work_claim_connection().unwrap().1;
    let raw = rusqlite::Connection::open(db_path(&server)).unwrap();
    raw.execute(
        "UPDATE identity_admissions SET state='rejected' WHERE connection_id=?1",
        [&connection],
    )
    .unwrap();
    assert!(intake(&server, "flow_revoke").is_err());
    assert!(!run_dir_for_flow_id("flow_revoke").unwrap().exists());
    raw.execute(
        "UPDATE identity_admissions SET state='self_asserted' WHERE connection_id=?1",
        [&connection],
    )
    .unwrap();
    let path = db_path(&server);
    let connection_copy = connection.clone();
    install_status_io_hook(
        StatusIoHookStage::AfterDirectoryValidation,
        run_dir_for_flow_id("flow_revoke").unwrap(),
        move |_| {
            let competing = rusqlite::Connection::open(path).unwrap();
            competing.busy_timeout(std::time::Duration::ZERO).unwrap();
            let error = competing
                .execute(
                    "UPDATE identity_admissions SET state='rejected' WHERE connection_id=?1",
                    [connection_copy],
                )
                .unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
        },
    );
    intake(&server, "flow_revoke").unwrap();
    raw.execute(
        "UPDATE identity_admissions SET state='rejected' WHERE connection_id=?1",
        [connection],
    )
    .unwrap();
    assert_eq!(snapshot(&server)["available"], false);
}

#[test]
fn interrupted_seal_and_directory_replacement_never_expose_private_payload() {
    for replacement in [false, true] {
        let (server, _home) = crate::tests::make_server_with_temp_home();
        local(&server, "alpha", "agent.alpha");
        let flow = "flow_interrupted";
        claim(&server, flow, "one");
        let path = run_dir_for_flow_id(flow).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let destination = outside.path().to_owned();
        std::fs::write(destination.join("status.json"), "{\"task\":\"FOREIGN\"}").unwrap();
        install_status_io_hook(
            StatusIoHookStage::BeforeAtomicRename,
            path.clone(),
            move |dir| {
                if replacement {
                    std::fs::rename(dir, dir.with_extension("displaced")).unwrap();
                    std::os::unix::fs::symlink(&destination, dir).unwrap();
                } else {
                    std::fs::create_dir(dir.join("status.json")).unwrap();
                }
            },
        );
        assert_eq!(intake(&server, flow).unwrap_err(), UNAVAILABLE);
        assert_eq!(
            std::fs::read_to_string(outside.path().join("status.json")).unwrap(),
            "{\"task\":\"FOREIGN\"}"
        );
        assert!(!outside.path().join("pr_handoff.md").exists());
        assert!(!legacy_read_allowed(&path));
        assert_eq!(snapshot(&server)["flows"][0]["state"], "unavailable");
    }
}

#[test]
fn seal_is_not_removed_by_merge_null_or_legacy_writers() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_seal", "one");
    intake(&server, "flow_seal").unwrap();
    let path = run_dir_for_flow_id("flow_seal").unwrap();
    let original = std::fs::read(path.join("status.json")).unwrap();
    let mut protected: Value = serde_json::from_slice(&original).unwrap();
    let original_seal = protected["ownership"].clone();
    super::super::utils::deep_merge(&mut protected, json!({"ownership":null,"state":"ignored"}));
    assert_eq!(protected["ownership"], original_seal);
    assert!(super::super::utils::merge_flow_status(&path, json!({"ownership":null})).is_err());
    assert!(crate::task_lifecycle::merge_github_status(&path, json!({"state":"changed"})).is_err());
    assert!(crate::task_lifecycle::mark_task_dispatch_completion(
        "flow_seal",
        "fake",
        json!({"state":"completed"})
    )
    .is_err());
    assert_eq!(std::fs::read(path.join("status.json")).unwrap(), original);
    assert!(!path.join("events.jsonl").exists());
}

#[test]
fn independent_server_connections_can_seal_a_flow_only_once() {
    let (first, _home) = crate::tests::make_server_with_temp_home();
    local(&first, "alpha", "agent.alpha");
    claim(&first, "flow_seal_race", "one");
    let second = MemoryServer::new(db_path(&first), None).unwrap();
    local(&second, "alpha", "agent.alpha");
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            barrier.wait();
            intake(&first, "flow_seal_race")
        });
        let two = scope.spawn(|| {
            barrier.wait();
            intake(&second, "flow_seal_race")
        });
        [one.join().unwrap(), two.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| matches!(r, Ok(true))).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r,Err(e) if e==UNAVAILABLE))
            .count(),
        1
    );
    assert_eq!(snapshot(&first)["flows"][0]["state"], "available");
    let raw = rusqlite::Connection::open(db_path(&first)).unwrap();
    assert_eq!(
        session_claims::list_project_work_claims(&raw, "alpha")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn interruption_after_the_seal_is_incomplete_and_never_legacy() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_partial", "one");
    let path = run_dir_for_flow_id("flow_partial").unwrap();
    install_status_io_hook(StatusIoHookStage::BeforeAtomicRename, path.clone(), |dir| {
        // The first atomic rename installs the seal. The blocked handoff name
        // makes the next file publication fail while retaining that seal.
        std::fs::create_dir(dir.join("pr_handoff.md")).unwrap();
    });
    assert_eq!(intake(&server, "flow_partial").unwrap_err(), UNAVAILABLE);
    let receipt: Value =
        serde_json::from_slice(&std::fs::read(path.join("status.json")).unwrap()).unwrap();
    assert_eq!(receipt["state"], "incomplete");
    assert!(receipt["ownership"].is_object());
    assert!(!legacy_read_allowed(&path));
    assert_eq!(snapshot(&server)["flows"][0]["state"], "unavailable");
    assert!(!path.join("instruction.md").exists());
}

#[tokio::test]
async fn real_task_intake_uses_the_claimed_producer_and_keeps_presence_unbound() {
    use std::os::unix::fs::PermissionsExt;
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_real_intake", "one");
    let bin = tempfile::tempdir().unwrap();
    let gh = bin.path().join("gh");
    std::fs::write(&gh,"#!/bin/sh\nprintf called >> \"${0%/*}/gh-calls\"\nprintf '%s' '{\"title\":\"owned intake\",\"body\":\"Acceptance criteria: retain the seal\",\"labels\":[],\"state\":\"OPEN\",\"url\":\"https://github.com/org/repo/issues/123\"}'\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let prior = std::env::var_os("PATH");
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(p) = &self.0 {
                std::env::set_var("PATH", p);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }
    let _restore = Restore(prior.clone());
    let mut paths = vec![bin.path().to_owned()];
    if let Some(prior) = prior {
        paths.extend(std::env::split_paths(&prior));
    }
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    for flow in [None, Some("flow_missing_claim")] {
        let mut denied = params("intake", flow);
        denied.issue_ref = Some("org/repo#123".into());
        assert!(server.tachi_task(Parameters(denied)).await.is_err());
        assert!(!bin.path().join("gh-calls").exists());
    }
    local(&server, "alpha", "agent.without_claim");
    let mut denied = params("intake", Some("flow_real_intake"));
    denied.issue_ref = Some("org/repo#123".into());
    assert!(server.tachi_task(Parameters(denied)).await.is_err());
    assert!(!bin.path().join("gh-calls").exists());
    local(&server, "alpha", "agent.alpha");
    let mut p = params("intake", Some("flow_real_intake"));
    p.issue_ref = Some("org/repo#123".into());
    let result = server.tachi_task(Parameters(p)).await.unwrap();
    assert!(result.contains("flow_real_intake"));
    assert_eq!(
        std::fs::read_to_string(bin.path().join("gh-calls")).unwrap(),
        "called"
    );
    let receipt: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(receipt["automation_plan"]["dispatch_allowed"], false);
    assert_eq!(
        receipt["automation_plan"]["status"],
        "execution_integration_unavailable"
    );
    let raw = rusqlite::Connection::open(db_path(&server)).unwrap();
    assert!(session_claims::list_unbound_claims(&raw)
        .unwrap()
        .is_empty());
    assert_eq!(snapshot(&server)["flows"][0]["state"], "available");
    let path = run_dir_for_flow_id("flow_real_intake").unwrap();
    assert!(path.join("pr_handoff.md").is_file());
    assert!(path.join("instruction.md").is_file());
}

#[tokio::test]
async fn unsupported_producers_refuse_before_receipts_claims_or_artifacts() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_no_execute", "one");
    intake(&server, "flow_no_execute").unwrap();
    let path = run_dir_for_flow_id("flow_no_execute").unwrap();
    let before = std::fs::read(path.join("status.json")).unwrap();
    let raw = rusqlite::Connection::open(db_path(&server)).unwrap();
    let durable_counts = || {
        [
            "session_claims",
            "exec_envs",
            "dispatch_outcomes",
            "dispatch_adjudications",
            "route_decisions",
        ]
        .into_iter()
        .map(|table| {
            raw.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        })
        .collect::<Vec<_>>()
    };
    let counts_before = durable_counts();
    let runs_before = std::fs::read_dir(flow_runs_root())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let mut complete = params("complete", Some("flow_no_execute"));
    complete.task = Some("not launched".into());
    complete.agent = Some("custom".into());
    complete.outcome = Some("success".into());
    assert!(server
        .tachi_task(Parameters(complete))
        .await
        .unwrap_err()
        .contains("producer is unavailable"));
    let staff=serde_json::from_value(json!({"action":"start","task":"do not launch","staffing_reason":"explicit_user_request","flow_id":"flow_no_execute","worker":"custom"})).unwrap();
    assert!(server
        .tachi_staff(Parameters(staff))
        .await
        .unwrap_err()
        .contains("producer is unavailable"));
    let verify=serde_json::from_value(json!({"action":"record","flow_id":"flow_no_execute","check_id":"no-write","status":"passed"})).unwrap();
    assert!(server
        .tachi_verify(Parameters(verify))
        .await
        .unwrap_err()
        .contains("producer is unavailable"));
    assert_eq!(std::fs::read(path.join("status.json")).unwrap(), before);
    assert!(!path.join("verification.json").exists());
    assert!(!path.join("events.jsonl").exists());
    assert_eq!(
        durable_counts(),
        counts_before,
        "refusal must not persist claims, execution environments or outcome/evidence rows"
    );
    assert_eq!(
        std::fs::read_dir(flow_runs_root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>(),
        runs_before,
        "refusal must not create a worker run"
    );
    assert_eq!(snapshot(&server)["items"].as_array().unwrap().len(), 1);
    local(&server, "beta", "agent.beta");
    assert_eq!(
        producer_preflight(&server, Some("flow_no_execute")).unwrap_err(),
        UNAVAILABLE
    );
}

#[tokio::test]
async fn gc_changes_semantic_revision_without_a_version_bump_and_never_reopens_legacy_access() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    let id = claim(&server, "flow_gc", "one");
    intake(&server, "flow_gc").unwrap();
    claim(&server, "flow_read_grant", "read-grant");
    let raw = rusqlite::Connection::open(db_path(&server)).unwrap();
    raw.execute(
        "UPDATE session_claims SET heartbeat_at='2000-01-01T00:00:00.000Z' WHERE claim_id=?1",
        [&id],
    )
    .unwrap();
    let before = snapshot(&server);
    session_claims::gc_session_claims(&raw, Utc::now(), 7, 30).unwrap();
    let after = snapshot(&server);
    assert_eq!(
        after["available"], true,
        "another live held claim retains project read authority"
    );
    assert_ne!(before["revision"], after["revision"]);
    assert_eq!(
        session_claims::get_claim(&raw, &id)
            .unwrap()
            .unwrap()
            .transition_version,
        0
    );
    local(&server, "alpha", "agent.successor");
    let mut handoff:Value=serde_json::to_value(json!({"action":"handoff","claim_id":id,"transition_version":0,"claim_mode":"writable","claim_role":"executor","claim_scope":["one/**"],"worktree_path":flow_runs_root().parent().unwrap().join("wt-one"),"expected_head":"1234567","lease_expires_at":"2035-01-01T00:00:00Z"})).unwrap();
    crate::claims_ops::handle_task_handoff(
        &server,
        &serde_json::from_value(handoff.take()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        session_claims::work_claim_project(&raw, &id)
            .unwrap()
            .as_deref(),
        Some("alpha")
    );
    session_claims::release_work_claim(&raw, &id, "agent.successor", 1, "done").unwrap();
    session_claims::gc_session_claims(&raw, Utc::now() + chrono::Duration::days(60), 7, 30)
        .unwrap();
    assert!(snapshot(&server)["items"].as_array().unwrap().is_empty());
    let path = run_dir_for_flow_id("flow_gc").unwrap();
    assert!(!legacy_read_allowed(&path));
    assert!(crate::task_lifecycle::read_json_file(&path.join("status.json")).is_err());
    server.set_session_identity(None, None, None);
    let legacy = crate::dispatch_ops::handle_tachi_board(
        &server,
        serde_json::from_value(json!({"limit":10})).unwrap(),
    )
    .await
    .unwrap();
    assert!(!legacy.contains("flow_gc"));
    assert!(!legacy.contains("private task"));
}

#[test]
fn local_identity_and_routing_without_a_live_project_claim_never_authorize_reading() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.owner");
    claim(&server, "flow_private_alpha", "owner");
    intake(&server, "flow_private_alpha").unwrap();
    local(&server, "alpha", "agent.unclaimed");
    let absent = snapshot(&server);
    assert_eq!(absent["available"], false);
    assert!(absent["items"].as_array().unwrap().is_empty());
    assert_eq!(
        producer_preflight(&server, Some("flow_private_alpha")).unwrap_err(),
        UNAVAILABLE
    );
    local(&server, "beta", "agent.unclaimed");
    let held = claim(&server, "flow_private_beta", "beta");
    intake(&server, "flow_private_beta").unwrap();
    assert_eq!(snapshot(&server)["available"], true);
    server.set_session_identity(None, Some("alpha".into()), None);
    assert_eq!(
        snapshot(&server)["available"],
        false,
        "changing only routing cannot transfer beta's grant to alpha"
    );
    server.set_session_identity(None, Some("beta".into()), None);
    let raw = rusqlite::Connection::open(db_path(&server)).unwrap();
    raw.execute(
        "UPDATE session_claims SET lease_expires_at='2000-01-01T00:00:00.000Z' WHERE claim_id=?1",
        [held],
    )
    .unwrap();
    assert_eq!(
        snapshot(&server)["available"],
        false,
        "expired ownership is not a read grant"
    );
}

#[test]
fn metadata_classifier_rejects_null_duplicate_malformed_and_missing_markers_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("flow_metadata");
    std::fs::create_dir(&path).unwrap();
    for input in [
        "{\"ownership\":null}",
        "{\"ownership\":{},\"ownership\":{}}",
        "{\"ownership\":3}",
        "{\"ownership\":",
        "[]",
    ] {
        std::fs::write(path.join("status.json"), input).unwrap();
        assert!(!legacy_read_allowed(&path));
        assert!(anchor(&path)
            .unwrap()
            .read_unsealed_component("status.json", 1024)
            .is_err());
        assert_eq!(
            std::fs::read_to_string(path.join("status.json")).unwrap(),
            input
        );
        assert!(!path.join(".status-mutation.lock").exists());
    }
    std::fs::remove_file(path.join("status.json")).unwrap();
    assert!(!legacy_read_allowed(&path));
    std::fs::write(path.join("status.json"), "{\"task\":\"legacy\"}").unwrap();
    assert!(legacy_read_allowed(&path));
}

#[test]
fn verification_only_legacy_exception_is_fixed_and_deny_only() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("flow_verification_only");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(
        path.join("verification.json"),
        r#"{"overall":"failed","source":"caller_asserted"}"#,
    )
    .unwrap();
    std::fs::write(path.join("private.json"), r#"{"private":"sentinel"}"#).unwrap();
    assert!(legacy_verification_preflight(&path).is_ok());
    assert_eq!(
        read_legacy_verification(&path).unwrap().unwrap()["overall"],
        "failed"
    );
    assert!(!legacy_read_allowed(&path));
    assert!(read_legacy_json(&path.join("private.json"))
        .unwrap()
        .is_err());
    for status in [
        "",
        "null",
        "{",
        r#"{"ownership":null}"#,
        r#"{"ownership":{},"ownership":{}}"#,
        r#"{"ownership":{"unknown":true}}"#,
        r#"{"ownership":{"version":1,"claim_id":"c","canonical_project":"alpha","flow_id":"flow_verification_only"}}"#,
    ] {
        std::fs::write(path.join("status.json"), status).unwrap();
        assert!(legacy_verification_preflight(&path).is_err(), "{status}");
        assert!(read_legacy_verification(&path).is_err(), "{status}");
    }
    std::fs::remove_file(path.join("status.json")).unwrap();
    let foreign = tmp.path().join("foreign.json");
    std::fs::write(&foreign, "{}").unwrap();
    std::os::unix::fs::symlink(&foreign, path.join("status.json")).unwrap();
    assert!(read_legacy_verification(&path).is_err());
}

#[test]
fn scoped_zero_limit_and_unavailable_run_filter_are_explicit() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_zero", "alpha");
    intake(&server, "flow_zero").unwrap();
    let mut p = params("board", None);
    p.limit = Some(0);
    let value = super::super::scoped_work_read_section(&server, &p).unwrap();
    assert_eq!(value["available"], true);
    assert_eq!(value["health"]["visible_work_count"], 0);
    assert_eq!(value["items"], json!([]));
    assert_eq!(value["flows"], json!([]));
    assert_eq!(value["sources"]["work_claims"]["claims"], json!([]));
    p.state_filter = Some("active".into());
    let value = super::super::scoped_work_read_section(&server, &p).unwrap();
    assert_eq!(value["available"], false);
    assert_eq!(
        value["scope"]["reason"],
        "run_state_filter_source_unavailable"
    );
}

#[test]
fn new_seal_refuses_a_legacy_writer_that_won_the_directory_fence() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_contaminated", "alpha");
    let path = run_dir_for_flow_id("flow_contaminated").unwrap();
    install_status_io_hook(
        StatusIoHookStage::AfterDirectoryValidation,
        path.clone(),
        |dir| {
            std::fs::write(dir.join("verification.json"), "legacy race").unwrap();
        },
    );
    assert!(intake(&server, "flow_contaminated").is_err());
    assert!(!path.join("status.json").exists());
    assert!(!path.join("instruction.md").exists());
    assert!(!path.join("pr_handoff.md").exists());
}

#[test]
fn bounded_project_snapshot_refuses_before_opening_any_receipt() {
    use super::super::work_read_projection::{MAX_SCOPED_CLAIMS, MAX_SCOPED_RECEIPTS};
    use crate::managed_run_control::test_hooks::run_status_io_hook;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_bounded", "first");
    intake(&server, "flow_bounded").unwrap();
    let path = run_dir_for_flow_id("flow_bounded").unwrap();
    for count in 1..=MAX_SCOPED_CLAIMS {
        claim(
            &server,
            &format!("flow_cap_{count}"),
            &format!("cap-{count}"),
        );
        if count == MAX_SCOPED_RECEIPTS || count == MAX_SCOPED_CLAIMS {
            let opened = Arc::new(AtomicBool::new(false));
            let witness = opened.clone();
            install_status_io_hook(
                StatusIoHookStage::AfterDirectoryValidation,
                path.clone(),
                move |_| {
                    witness.store(true, Ordering::SeqCst);
                },
            );
            let mut p = params("board", None);
            p.limit = Some(1);
            let value = super::super::scoped_work_read_section(&server, &p).unwrap();
            assert_eq!(value["available"], false);
            assert_eq!(value["health"]["bounded"], true);
            assert_eq!(value["health"]["incomplete"], true);
            assert!(
                !opened.load(Ordering::SeqCst),
                "the bound must precede every receipt open"
            );
            assert_eq!(
                value["sources"]["work_claims"]["reason"],
                if count == MAX_SCOPED_CLAIMS {
                    "project_claim_limit"
                } else {
                    "project_receipt_limit"
                }
            );
            run_status_io_hook(StatusIoHookStage::AfterDirectoryValidation, &path);
            assert!(opened.load(Ordering::SeqCst));
        }
    }
}

#[tokio::test]
async fn exact_and_full_views_keep_the_same_work_and_flow_revisions() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    local(&server, "alpha", "agent.alpha");
    claim(&server, "flow_revision_a", "1");
    intake(&server, "flow_revision_a").unwrap();
    claim(&server, "flow_revision_b", "2");
    intake(&server, "flow_revision_b").unwrap();
    let all = stable(snapshot(&server));
    for action in ["status", "brief"] {
        let raw = server
            .tachi_task(Parameters(params(action, Some("flow_revision_a"))))
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let exact = stable(
            value
                .get("work_read_model")
                .or_else(|| value.pointer("/cycle/work_read_model"))
                .unwrap()
                .clone(),
        );
        let item = &exact["items"][0];
        let same = all["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["work_token"] == item["work_token"])
            .unwrap();
        assert_eq!(item, same);
        assert_eq!(exact["revision"], all["revision"]);
        let flow = &exact["flows"][0];
        assert_eq!(
            flow,
            all["flows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["flow_id"] == flow["flow_id"])
                .unwrap()
        );
    }
}
