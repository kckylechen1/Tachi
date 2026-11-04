use super::*;

fn find_worktree(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(path).ok()?;
    for entry in entries.filter_map(Result::ok) {
        if entry.file_name() == ".git" {
            return entry.path().parent().map(std::path::Path::to_path_buf);
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if let Some(worktree) = find_worktree(&entry.path()) {
                return Some(worktree);
            }
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staff_start_launches_fake_worker_through_canonical_receipt_lifecycle() {
    assert_staff_lifecycle(false, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_staff_runs_inner_sandbox_and_retains_unproven_workspace() {
    assert_staff_lifecycle(true, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_fence_failure_overrides_prior_completion_and_recovery() {
    for state in ["receipt", "kanban", "recovery"] {
        assert_staff_lifecycle(true, Some(state)).await;
    }
}

#[allow(clippy::await_holding_lock)] // serializes process-global fake-worker environment through terminal cleanup
async fn assert_staff_lifecycle(native: bool, fence_failure: Option<&str>) {
    let _environment = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp canonical run root");
    let temp_bin = tempfile::tempdir().expect("temp fake worker bin");
    write_fake_worker(temp_bin.path(), 0);
    if native && cfg!(target_os = "macos") {
        let worker = temp_bin.path().join("codex");
        let script = std::fs::read_to_string(&worker).unwrap();
        let script = script.replace("printf 'staff fake worker", "/usr/bin/sandbox-exec -p '(version 1)(allow default)' /bin/cat .gitignore || exit 71\nprintf 'staff fake worker");
        std::fs::write(&worker, script).unwrap();
    }
    let old_path = std::ffi::OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
    let joined_path = std::env::join_paths(
        std::iter::once(temp_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .expect("join fake-worker PATH");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let _path = crate::test_support::EnvRestore::set_os("PATH", &joined_path);
    let _git_environment = isolate_staff_repository_environment();
    let _registry_home = crate::test_support::EnvRestore::set_path("HOME", temp_home.path());
    let _repository = StaffRepositoryFixture::new(temp_home.path());
    let _cwd = CurrentDirGuard::set(&_repository.repo);
    assert_eq!(
        std::fs::canonicalize(
            crate::utils::find_project_git_root().expect("private fallback root")
        )
        .unwrap(),
        std::fs::canonicalize(&_repository.repo).unwrap(),
        "every production repository discovery route stays in the fixture"
    );
    let temp_worktrees = tempfile::tempdir().expect("isolated Staff worktrees");
    let _worktrees =
        crate::test_support::EnvRestore::set_path("TACHI_WORKTREES_ROOT", temp_worktrees.path());
    let _fixture_cleanup = CertifiedWorktreeCleanupGuard::arm(temp_worktrees.path(), find_worktree);
    let server = test_server();
    let recommendation = server
        .with_global_store(|store| {
            memcore::insert_route_recommendation(
                store.connection(),
                &memcore::NewRouteRecommendation {
                    recommendation_id: "rec-staff-e2e".to_string(),
                    task_type: Some("implementation".to_string()),
                    risk: "low".to_string(),
                    candidates: serde_json::json!([{"profile": "codex_55_review"}]),
                    recommended_profile: Some("codex_55_review".to_string()),
                    policy_source_revision: Some("staff-e2e".to_string()),
                    rows_considered: 1,
                    occurred_at: memcore::now_utc_iso(),
                },
            )
            .map_err(|error| error.to_string())
        })
        .expect("seed recommendation fact");
    let mut request = staff_request("tachi");
    if native {
        request.declared_file_scope = None;
    }
    request.recommendation_ref = Some(recommendation.recommendation_id.clone());

    let raw = staff_start(&server, request)
        .await
        .expect("Staff start should be accepted before background execution");
    let _cleanup_guard = StaffCleanupGuard::arm(&raw);
    let release_worker = StaffWorkerReleaseGuard(temp_bin.path().join("codex.release"));
    let response: Value = serde_json::from_str(&raw).expect("canonical response JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
    _repository.assert_dispatch_source(&server, dispatch_id);
    let run_dir = dispatch_runs_root().join(dispatch_id);
    let mut observed_busy = false;
    for _ in 0..100 {
        let roster =
            crate::staffing_ops::presence::roster(&server, Some("tachi"), None, 100).unwrap();
        observed_busy = roster["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["dispatch_id"] == dispatch_id && row["state"] == "busy");
        if observed_busy {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        observed_busy,
        "the real Staff launcher registers its spawned worker"
    );
    let injector = rusqlite::Connection::open(server.global_db_path_buf()).unwrap();
    if let Some(prior) = fence_failure {
        injector.execute_batch("CREATE TRIGGER fail_native_fence BEFORE UPDATE OF state ON exec_env_resources WHEN NEW.state = 'quarantined' BEGIN SELECT RAISE(ABORT, 'injected native fence failure'); END;").unwrap();
        if prior == "kanban" {
            crate::dispatch_ops::update_kanban_state(
                &server,
                dispatch_id,
                "TASK_STATE_COMPLETED",
                None,
                Some(false),
            )
            .await
            .unwrap();
        } else {
            let path = run_dir.join("status.json");
            let mut status: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if prior == "receipt" {
                status["resolved_completion"] = serde_json::json!({
                    "state": "TASK_STATE_COMPLETED", "eval_ledger_id": "fixture-success",
                    "reviewed": false, "recorded_at": chrono::Utc::now().to_rfc3339(),
                });
            } else {
                status["completion_recovery"] = serde_json::json!({"status": "pending_canonical_outcome", "dispatch_outcome": {"recorded": false}});
            }
            std::fs::write(path, serde_json::to_vec(&status).unwrap()).unwrap();
        }
    }
    drop(release_worker);
    let (status, result) = wait_for_staff_terminal(&run_dir).await;
    wait_for_staff_cleanup(dispatch_id).await;
    let roster = crate::staffing_ops::presence::roster(&server, Some("tachi"), None, 100).unwrap();
    assert!(roster["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["dispatch_id"] == dispatch_id && row["state"] == "disconnected"));
    let env_id = status["env_id"].as_str().expect("managed Staff env_id");
    let lease = server
        .with_global_store_read(|store| {
            memcore::get_exec_env(store.connection(), env_id).map_err(|error| error.to_string())
        })
        .expect("read automatic Staff lease")
        .expect("automatic Staff lease remains auditable after cleanup");
    if native {
        assert!(std::path::Path::new(&lease.path).exists());
        assert_eq!(
            status["exec_env_postflight"]["status"],
            "delegated_to_harness"
        );
        assert_eq!(
            status["exec_env_postflight"]["workspace_cleanup"],
            "retained"
        );
        assert_eq!(
            status["exec_env_postflight"]["resource_fence"],
            if fence_failure.is_some() {
                "failed"
            } else {
                "persisted"
            }
        );
        assert!(status["exec_env_postflight"]["descendant_liveness"]
            .as_str()
            .unwrap()
            .contains("indeterminate"));
        assert!(
            crate::exec_env_ops::ExecEnvDispatchLeaseGuard::acquire(&server, env_id).is_err(),
            "retained resources must not be reused"
        );
        if fence_failure.is_some() {
            assert_eq!(terminal_staff_state(&status), "TASK_STATE_FAILED");
            assert!(result.contains("native Staff workspace retention failed"));
            assert_eq!(
                crate::dispatch_ops::get_kanban_state(&server, dispatch_id)
                    .await
                    .as_deref(),
                Some("TASK_STATE_FAILED")
            );
            let error_class = server
                .with_named_project_store("tachi", |store| {
                    store
                        .connection()
                        .query_row(
                            "SELECT error_class FROM dispatch_outcomes WHERE dispatch_id = ?1",
                            [dispatch_id],
                            |row| row.get::<_, String>(0),
                        )
                        .map_err(|e| e.to_string())
                })
                .unwrap();
            assert_eq!(error_class, "native_staff_retention_failed");
            injector
                .execute_batch("DROP TRIGGER fail_native_fence")
                .unwrap();
            // Fixture-only teardown: this fixed shell worker returned without escaped children.
            injector.execute("UPDATE exec_envs SET state = 'active' WHERE env_id = ?1 AND state = 'dispatching'", [env_id]).unwrap();
        }
        let resource_ids = {
            let mut stmt = injector.prepare("SELECT resource_id FROM exec_env_resource_bindings WHERE env_id = ?1 AND released_at IS NULL").unwrap();
            stmt.query_map([env_id], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        server
            .with_global_store(|store| {
                for resource_id in &resource_ids {
                    memcore::release_quarantine(store.connection_mut(), resource_id, |resource| {
                        assert!(std::fs::canonicalize(&resource.path)
                            .unwrap()
                            .starts_with(std::fs::canonicalize(temp_worktrees.path()).unwrap()));
                        assert!(std::path::Path::new(&resource.path)
                            .join(".gitignore")
                            .is_file());
                        Ok(0)
                    })
                    .map_err(|error| error.to_string())?;
                }
                Ok(())
            })
            .unwrap();
        let cleanup = tachi_clean::wt_clean::remove_worktree_for_safe_merge(std::path::Path::new(
            &lease.path,
        ));
        assert!(
            cleanup.removed,
            "fixture worker has finished: warnings={:?}, errors={:?}",
            cleanup.warnings, cleanup.errors
        );
        if cfg!(target_os = "macos") && fence_failure.is_none() {
            assert!(
                result.contains(".tachi/"),
                "inner sandbox must read the real fixture: {result}"
            );
        }
    } else {
        assert_eq!(lease.state, memcore::ExecEnvState::Reclaimed);
        assert!(
            !std::path::Path::new(&lease.path).exists(),
            "clean automatic Staff worktree must be removed"
        );
    }

    if fence_failure.is_some() {
        return;
    }
    assert_eq!(
        terminal_staff_state(&status),
        "TASK_STATE_COMPLETED",
        "fake worker terminal receipt"
    );
    assert_eq!(
        status["project"], "tachi",
        "Staff project linkage survives launch"
    );
    assert_eq!(
        status["result_written"], true,
        "terminal outcome retains result receipt"
    );
    assert_eq!(
        status["run_dir"],
        run_dir.to_string_lossy().as_ref(),
        "recovery retains the canonical run directory"
    );
    assert!(
        result.contains("staff fake worker"),
        "canonical result.md: {result}"
    );

    let receipt_before = std::fs::read(run_dir.join("status.json")).expect("receipt bytes");
    let reply = server
        .tachi_staff(rmcp::handler::server::wrapper::Parameters(
            serde_json::from_value(serde_json::json!({
                "action": "result", "dispatch_id": dispatch_id, "format": "json"
            }))
            .expect("result params"),
        ))
        .await
        .expect("read the real worker result through Staff");
    let reply: Value = serde_json::from_str(&reply).expect("result JSON");
    assert_eq!(reply["result"]["body"], result);
    assert_eq!(reply["result"]["truncated"], false);
    assert_eq!(
        std::fs::read(run_dir.join("status.json")).expect("receipt after read"),
        receipt_before,
        "result reading must not rewrite the canonical receipt"
    );

    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("canonical trajectory");
    let events = trajectory
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("trajectory JSON"))
        .map(|event| event["event"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    let received = events
        .iter()
        .position(|event| event == "dispatch_received")
        .expect("receipt-first event");
    let started = events
        .iter()
        .position(|event| event == "execute_started")
        .expect("real execution event");
    let finished = events
        .iter()
        .position(|event| event == "subprocess_finished")
        .expect("subprocess terminal event");
    assert!(
        received < started && started < finished,
        "canonical trajectory ordering: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| *event == "dispatch_received")
            .count(),
        1,
        "no second lifecycle"
    );
    let decision = server
        .with_global_store_read(|store| {
            memcore::get_route_decision_by_dispatch_id(store.connection(), dispatch_id)
                .map_err(|error| error.to_string())
        })
        .expect("read route decision")
        .expect("exactly one route decision for accepted Staff start");
    assert_eq!(
        route_decisions_count(&server),
        1,
        "one acceptance creates one decision"
    );
    assert_eq!(decision.assignment_mode, "advised");
    assert_eq!(
        decision.recommendation_id.as_deref(),
        Some(recommendation.recommendation_id.as_str())
    );
    assert_eq!(
        status["route_decision_id"].as_str(),
        Some(decision.route_decision_id.as_str()),
        "the evidence stamp must retain the fast child's terminal receipt"
    );
}
