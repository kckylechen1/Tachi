use super::*;

/// The retained canonical kernel installs the in-memory cancellation owner
/// before its custom child reaches the hanging root. These fixed kernel
/// fixtures exercise the real status/cancel handlers and prove confirmation
/// follows process-group reaping; they do not claim native Staff admission.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // serializes process-global fake-worker environment through terminal cleanup
async fn issue_1825_managed_custom_cancel_race_matrix() {
    let _environment = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp canonical run root");
    let temp_bin = tempfile::tempdir().expect("temp fake worker bin");
    let root_pid = temp_bin.path().join("root.pid");
    let descendant_pid = temp_bin.path().join("descendant.pid");
    let cancel_trigger = temp_bin.path().join("cancel-trigger");
    let cancel_ack = temp_bin.path().join("cancel-ack");
    let cancel_latch = temp_bin.path().join("cancel-latch");
    let cancel_nonce = "staff-cancel-live-nonce";
    write_hanging_managed_custom_worker(
        temp_bin.path(),
        &root_pid,
        &descendant_pid,
        &cancel_trigger,
        &cancel_ack,
        &cancel_latch,
        cancel_nonce,
    );
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
    let repository = StaffRepositoryFixture::new(temp_home.path());
    let _cwd = CurrentDirGuard::set(&repository.repo);
    let temp_worktrees = tempfile::tempdir().expect("isolated Staff worktrees");
    let _worktrees =
        crate::test_support::EnvRestore::set_path("TACHI_WORKTREES_ROOT", temp_worktrees.path());
    let _fixture_cleanup = CertifiedWorktreeCleanupGuard::arm(temp_worktrees.path(), find_worktree);
    let _transport = crate::test_support::EnvRestore::set("TACHI_OPENCODE_TRANSPORT", "cli");
    let _v2 = crate::test_support::EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let _review = crate::test_support::EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let _embedding =
        crate::test_support::EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
    std::fs::create_dir_all(repository.repo.join(".tachi/credentials"))
        .expect("create isolated credential profile directory");
    std::fs::write(
        repository
            .repo
            .join(".tachi/credentials/opencode-shared.json"),
        serde_json::json!({
            "credential_profiles": {
                "opencode_shared": {
                    "entries": {"auth_json": "OPENCODE_SHARED_AUTH_JSON"},
                    "allowed_consumers": {
                        "agents": ["opencode"],
                        "profiles": ["kernel_opencode_credentials"]
                    },
                    "materializers": [{
                        "type": "env",
                        "source": "auth_json",
                        "target": "OPENCODE_SHARED_AUTH_JSON"
                    }, {
                        "type": "config_overlay",
                        "source": "auth_json",
                        "target": "{credentials_dir}/opencode.json",
                        "template": {
                            "provider": {
                                "fixture": {
                                    "apiKey": "{env:OPENCODE_SHARED_AUTH_JSON}"
                                }
                            }
                        }
                    }]
                }
            }
        })
        .to_string(),
    )
    .expect("write isolated opencode credential profile");
    let server = test_server();
    crate::vault_test_api::seed_vault(&server, "issue-1825-opencode-fixture")
        .expect("initialize isolated credential vault");
    crate::vault_test_api::seed_vault_secret(
        &server,
        crate::vault_test_api::VaultSecretFixture {
            name: "OPENCODE_SHARED_AUTH_JSON".to_string(),
            value: r#"{"token":"issue-1825-fixture"}"#.to_string(),
            secret_type: "api_key".to_string(),
            description: "issue-1825 managed panic credential fixture".to_string(),
            allowed_agents: None,
        },
    )
    .expect("seed isolated opencode credential secret");
    let (project, _) = server
        .resolve_server_named_project_binding("tachi")
        .expect("resolve the registered fixture project before initializing its store");
    let cleanup_project = project.as_str();
    server
        .with_named_project_store(cleanup_project, |_| Ok(()))
        .expect("initialize named project store");

    // Classification persistence fails only after the real OpenCode
    // credential overlay exists. The keyed event gives this test the exact
    // run directory without a timing probe, then injects one write failure
    // for that run before the production registration branch continues.
    let materialization_root = dispatch_runs_root();
    let (_materialization_barrier, materialized, materialization_release) =
        crate::dispatch_ops::install_managed_credential_materialization_barrier(
            &materialization_root,
        );
    let mut classification_request = kernel_request(cleanup_project);
    classification_request.profile = Some("kernel_opencode_credentials".to_string());
    classification_request.worker = Some("custom".to_string());
    classification_request.flow_id = Some("flow_1825_classification_failure".to_string());
    let classification_server = server.clone();
    let classification_start = tokio::task::spawn(async move {
        kernel_start(&classification_server, classification_request).await
    });
    let classification_dir = tokio::task::spawn_blocking(move || {
        materialized
            .recv()
            .expect("credential materialization event")
    })
    .await
    .expect("credential materialization event join");
    let classification_id = classification_dir
        .file_name()
        .and_then(|name| name.to_str())
        .expect("classified run dispatch id")
        .to_string();
    repository.assert_dispatch_source(&server, &classification_id);
    assert!(
        classification_dir
            .join("credentials/opencode.json")
            .exists(),
        "the keyed event proves the real OpenCode credential overlay exists before failure"
    );
    let _classification_failure =
        crate::managed_run_control::fail_next_managed_custom_start_status_write(
            &classification_dir,
        );
    let _classification_result_failure =
        crate::dispatch_ops::install_managed_result_persist_failure(
            temp_home.path(),
            temp_runs.path(),
        );
    materialization_release
        .send(())
        .expect("release classification after failure injection");
    let classification_error = classification_start
        .await
        .expect("classification start join")
        .expect_err("injected classification persistence must reject Staff launch");
    assert!(
        classification_error.contains("injected managed custom classification persistence failure")
    );
    let classification_status: Value = serde_json::from_slice(
        &std::fs::read(classification_dir.join("status.json"))
            .expect("classification failure status"),
    )
    .expect("classification failure status JSON");
    assert_eq!(
        terminal_staff_state(&classification_status),
        "TASK_STATE_FAILED"
    );
    assert_eq!(
        classification_status["result_written"], false,
        "the early classification path must not claim a result after its atomic write fails"
    );
    assert!(!classification_dir.join("result.md").exists());
    assert!(
        !classification_dir
            .join("credentials/opencode.json")
            .exists(),
        "classification failure must clean the materialized OpenCode overlay before registry drop"
    );
    assert!(!server.managed_run_controls.contains(&classification_id));
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, &classification_id).await,
        Some("TASK_STATE_FAILED".to_string())
    );
    let classification_outcomes: i64 = server
        .with_named_project_store_read(cleanup_project, |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [&classification_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count classification failure outcomes");
    assert_eq!(classification_outcomes, 1);
    let classification_flow_lock_dir =
        crate::task_lifecycle::run_dir_for_flow_id("flow_1825_classification_failure")
            .expect("valid classification failure flow run directory")
            .join(".dispatch-dedupe");
    assert!(
        !classification_flow_lock_dir.exists()
            || std::fs::read_dir(&classification_flow_lock_dir)
                .expect("classification flow lock directory")
                .next()
                .is_none(),
        "classification failure must release the flow dispatch slot"
    );

    // Pre-spawn: the retained canonical launch has registered its managed owner
    // and accepted cancellation, but the production runner has not yet
    // called Command::spawn. Releasing the keyed barrier lets that runner
    // consume the queued command and prove spawn suppression.
    let (_pre_guard, pre_entered, pre_release) =
        crate::dispatch_ops::install_managed_pre_spawn_barrier(temp_home.path(), temp_runs.path());
    let mut pre_request = kernel_request(cleanup_project);
    pre_request.profile = Some("kernel_opencode_write".to_string());
    pre_request.worker = Some("custom".to_string());
    pre_request.flow_id = Some("flow_1825_pre_spawn".to_string());
    let pre_raw = kernel_start(&server, pre_request)
        .await
        .expect("pre-spawn Staff start");
    let _pre_cleanup = StaffCleanupGuard::arm(&pre_raw);
    let pre: Value = serde_json::from_str(&pre_raw).expect("pre-spawn response");
    let pre_dispatch_id = pre["dispatch_id"].as_str().expect("pre-spawn dispatch id");
    tokio::task::spawn_blocking(move || pre_entered.recv().expect("pre-spawn barrier"))
        .await
        .expect("pre-spawn barrier join");
    let pre_status: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: pre_dispatch_id.to_string(),
            },
        )
        .await
        .expect("pre-spawn status"),
    )
    .expect("pre-spawn status JSON");
    let pre_revision = pre_status["status_revision"]
        .as_u64()
        .expect("pre-spawn revision");
    let pre_server = server.clone();
    let pre_id = pre_dispatch_id.to_string();
    let pre_cancel = tokio::task::spawn(async move {
        pre_server
            .tachi_staff(rmcp::handler::server::wrapper::Parameters(
                serde_json::from_value::<tachi_params::TachiStaffParams>(serde_json::json!({
                    "action": "cancel",
                    "format": "markdown",
                    "dispatch_id": pre_id,
                    "expected_status_revision": pre_revision,
                }))
                .expect("markdown cancellation parameters"),
            ))
            .await
    });
    for _ in 0..100 {
        let status: Value = serde_json::from_str(
            &staff_status(
                &server,
                StaffStatusRequest {
                    dispatch_id: pre_dispatch_id.to_string(),
                },
            )
            .await
            .expect("pre requested status"),
        )
        .expect("pre requested JSON");
        if status["cancellation"]["receipt"] == "cancellation_requested" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    pre_release.send(()).expect("release pre-spawn runner");
    let pre_cancel_raw = pre_cancel
        .await
        .expect("pre cancel join")
        .expect("pre cancel response");
    assert!(
        !pre_cancel_raw.starts_with("## "),
        "cancel must ignore markdown formatting and return canonical JSON"
    );
    let pre_cancel: Value = serde_json::from_str(&pre_cancel_raw).expect("pre cancel JSON");
    assert_eq!(pre_cancel["receipt"], "cancellation_confirmed");
    assert_eq!(pre_cancel["termination_proof"], "spawn_suppressed");
    let pre_final: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: pre_dispatch_id.to_string(),
            },
        )
        .await
        .expect("pre final status"),
    )
    .expect("pre final JSON");
    assert_eq!(pre_cancel, pre_final["cancellation"]);
    assert_eq!(
        pre_cancel_raw,
        serde_json::to_string(&pre_final["cancellation"])
            .expect("serialize canonical pre-spawn cancellation receipt")
    );
    assert_eq!(terminal_staff_state(&pre_final), "TASK_STATE_CANCELED");
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, pre_dispatch_id).await,
        Some("TASK_STATE_CANCELED".to_string())
    );
    assert!(!server.managed_run_controls.contains(pre_dispatch_id));
    assert!(!dispatch_runs_root()
        .join(pre_dispatch_id)
        .join("credentials")
        .exists());

    // Timeout: this is the real Staff background watchdog, narrowed only
    // by the isolated run-root override, never by a direct runner call.
    let _timeout_override = crate::dispatch_ops::install_managed_timeout_override(
        temp_home.path(),
        temp_runs.path(),
        std::time::Duration::from_millis(200),
    );
    let mut timeout_request = kernel_request(cleanup_project);
    timeout_request.profile = Some("kernel_opencode_credentials".to_string());
    timeout_request.worker = Some("custom".to_string());
    timeout_request.flow_id = Some("flow_1825_timeout".to_string());
    let timeout_raw = kernel_start(&server, timeout_request)
        .await
        .expect("timeout Staff start");
    let mut timeout_cleanup = StaffCleanupGuard::arm(&timeout_raw);
    let timeout_response: Value = serde_json::from_str(&timeout_raw).expect("timeout response");
    let timeout_id = timeout_response["dispatch_id"]
        .as_str()
        .expect("timeout dispatch id");
    let timeout_dir = dispatch_runs_root().join(timeout_id);
    assert!(
        timeout_dir.join("credentials/opencode.json").exists(),
        "the managed OpenCode timeout must begin with a real credential materialization"
    );
    let (timeout_status, _) = wait_for_staff_terminal(&timeout_dir).await;
    wait_for_staff_cleanup(timeout_id).await;
    assert_eq!(terminal_staff_state(&timeout_status), "TASK_STATE_FAILED");
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, timeout_id).await,
        Some("TASK_STATE_FAILED".to_string())
    );
    let timeout_outcomes: i64 = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [timeout_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count timeout outcomes");
    assert_eq!(
        timeout_outcomes, 0,
        "a watchdog timeout must not fabricate a completion outcome"
    );
    assert!(!server.managed_run_controls.contains(timeout_id));
    assert!(
        !timeout_dir.join("credentials/opencode.json").exists(),
        "managed timeout cleanup must remove the real OpenCode materialization"
    );
    let timeout_project_outcomes: i64 = server
        .with_named_project_store(cleanup_project, |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [timeout_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count timeout project outcomes");
    assert_eq!(
        timeout_project_outcomes, 1,
        "managed timeout must record one truthful project failure outcome"
    );
    let timeout_flow_lock_dir = crate::task_lifecycle::run_dir_for_flow_id("flow_1825_timeout")
        .expect("valid timeout flow run directory")
        .join(".dispatch-dedupe");
    assert!(
        !timeout_flow_lock_dir.exists()
            || std::fs::read_dir(&timeout_flow_lock_dir)
                .expect("timeout flow lock directory")
                .next()
                .is_none(),
        "timeout completion must release the flow dispatch slot"
    );
    timeout_cleanup.disarm();
    drop(_timeout_override);

    // Hold the real background runner immediately before its select loop,
    // cross the real watchdog deadline, then enqueue a public cancellation.
    // The runner must not receive or signal that late command.
    let _late_timeout_override = crate::dispatch_ops::install_managed_timeout_override(
        temp_home.path(),
        temp_runs.path(),
        std::time::Duration::from_millis(200),
    );
    let (_late_select_guard, late_select_entered, late_select_release) =
        crate::dispatch_ops::install_managed_before_select_barrier(
            temp_home.path(),
            temp_runs.path(),
        );
    let mut late_timeout_request = kernel_request(cleanup_project);
    late_timeout_request.profile = Some("kernel_opencode_write".to_string());
    late_timeout_request.worker = Some("custom".to_string());
    late_timeout_request.flow_id = Some("flow_1825_timeout_late_cancel".to_string());
    let late_timeout_raw = kernel_start(&server, late_timeout_request)
        .await
        .expect("late-timeout Staff start");
    let mut late_timeout_cleanup = StaffCleanupGuard::arm(&late_timeout_raw);
    let late_timeout_start: Value =
        serde_json::from_str(&late_timeout_raw).expect("late-timeout response");
    let late_timeout_id = late_timeout_start["dispatch_id"]
        .as_str()
        .expect("late-timeout dispatch id")
        .to_string();
    let late_timeout_dir = dispatch_runs_root().join(&late_timeout_id);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            late_select_entered
                .recv()
                .expect("runner reached managed select")
        }),
    )
    .await
    .expect("managed runner reaches select barrier")
    .expect("select barrier join");
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let late_status: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: late_timeout_id.clone(),
            },
        )
        .await
        .expect("late-timeout accepted status"),
    )
    .expect("late-timeout accepted status JSON");
    let late_revision = late_status["status_revision"]
        .as_u64()
        .expect("late-timeout accepted revision");
    let late_cancel_server = server.clone();
    let late_cancel_id = late_timeout_id.clone();
    let late_cancel = tokio::task::spawn(async move {
        late_cancel_server
            .tachi_staff(rmcp::handler::server::wrapper::Parameters(
                serde_json::from_value::<tachi_params::TachiStaffParams>(serde_json::json!({
                    "action": "cancel",
                    "dispatch_id": late_cancel_id,
                    "expected_status_revision": late_revision,
                }))
                .expect("late cancellation parameters"),
            ))
            .await
            .expect("late cancellation response")
    });
    for _ in 0..100 {
        let status: Value = serde_json::from_str(
            &staff_status(
                &server,
                StaffStatusRequest {
                    dispatch_id: late_timeout_id.clone(),
                },
            )
            .await
            .expect("late cancellation requested status"),
        )
        .expect("late cancellation requested status JSON");
        if status["cancellation"]["receipt"] == "cancellation_requested" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    late_select_release
        .send(())
        .expect("release late-timeout runner");
    let (late_terminal, _) = wait_for_staff_terminal(&late_timeout_dir).await;
    let late_cancel_raw = late_cancel.await.expect("late cancellation join");
    let late_cancel: Value =
        serde_json::from_str(&late_cancel_raw).expect("late cancellation JSON");
    assert_eq!(terminal_staff_state(&late_terminal), "TASK_STATE_FAILED");
    assert_ne!(late_terminal["state"], "TASK_STATE_CANCELED");
    assert_eq!(late_cancel["receipt"], "cancellation_unavailable");
    assert_eq!(late_cancel["reason"], "completion_or_timeout_winner");
    assert_eq!(late_cancel, late_terminal["cancellation"]);
    assert_eq!(
        late_cancel_raw,
        serde_json::to_string(&late_terminal["cancellation"])
            .expect("serialize canonical late-timeout receipt")
    );
    assert_eq!(
        late_terminal["cancellation"]["observed_status_revision"],
        late_terminal["status_revision"]
    );
    wait_for_staff_cleanup(&late_timeout_id).await;
    assert!(!server.managed_run_controls.contains(&late_timeout_id));
    assert!(!late_timeout_dir.join("credentials").exists());
    late_timeout_cleanup.disarm();
    drop(_late_timeout_override);

    // A panic inside the managed runner is joined by the outer production
    // background task, which must still write FAILED and release all
    // durable/volatile ownership before status is observed.
    let _panic_injection = crate::dispatch_ops::install_managed_panic_after_spawn_for_run_root(
        temp_home.path(),
        temp_runs.path(),
    );
    let mut panic_request = kernel_request(cleanup_project);
    panic_request.profile = Some("kernel_opencode_credentials".to_string());
    panic_request.worker = Some("custom".to_string());
    panic_request.flow_id = Some("flow_1825_managed_panic".to_string());
    let panic_raw = kernel_start(&server, panic_request)
        .await
        .expect("panic-injected Staff start");
    let mut panic_cleanup = StaffCleanupGuard::arm(&panic_raw);
    let panic_response: Value = serde_json::from_str(&panic_raw).expect("panic start JSON");
    let panic_id = panic_response["dispatch_id"]
        .as_str()
        .expect("panic dispatch id");
    let panic_dir = dispatch_runs_root().join(panic_id);
    assert!(
        panic_dir.join("credentials/opencode.json").exists(),
        "the managed OpenCode profile must materialize ephemeral credentials before panic cleanup"
    );
    let (panic_terminal, panic_result) = wait_for_staff_terminal(&panic_dir).await;
    wait_for_staff_cleanup(panic_id).await;
    assert_eq!(terminal_staff_state(&panic_terminal), "TASK_STATE_FAILED");
    assert!(panic_result.contains("managed subprocess panicked"));
    assert!(!server.managed_run_controls.contains(panic_id));
    assert!(
        !panic_dir.join("credentials/opencode.json").exists(),
        "panic terminal cleanup must remove the actual OpenCode credential materialization"
    );
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, panic_id).await,
        Some("TASK_STATE_FAILED".to_string())
    );
    let panic_flow_lock_dir = crate::task_lifecycle::run_dir_for_flow_id("flow_1825_managed_panic")
        .expect("valid panic flow run directory")
        .join(".dispatch-dedupe");
    assert!(
        !panic_flow_lock_dir.exists()
            || std::fs::read_dir(&panic_flow_lock_dir)
                .expect("panic flow lock directory")
                .next()
                .is_none(),
        "panic finalization must release the Staff flow slot"
    );
    panic_cleanup.disarm();
    drop(_panic_injection);

    // A one-shot failure of the first managed terminal replacement must
    // still terminalize this real Staff/background run before volatile
    // registry and flow ownership are released.
    let mut persist_failure_request = kernel_request(cleanup_project);
    persist_failure_request.profile = Some("kernel_opencode_write".to_string());
    persist_failure_request.worker = Some("custom".to_string());
    persist_failure_request.flow_id = Some("flow_1825_terminal_write_failure".to_string());
    let persist_failure_raw = kernel_start(&server, persist_failure_request)
        .await
        .expect("terminal-write-failure Staff start");
    let mut persist_failure_cleanup = StaffCleanupGuard::arm(&persist_failure_raw);
    let persist_failure_start: Value =
        serde_json::from_str(&persist_failure_raw).expect("terminal-write-failure start JSON");
    let persist_failure_id = persist_failure_start["dispatch_id"]
        .as_str()
        .expect("terminal-write-failure dispatch id");
    let persist_failure_dir = dispatch_runs_root().join(persist_failure_id);
    wait_for_managed_custom_processes(&persist_failure_dir, &root_pid, &descendant_pid).await;
    let _persist_failure =
        crate::dispatch_ops::fail_next_managed_terminal_status_write(&persist_failure_dir);
    let persist_failure_status: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: persist_failure_id.to_string(),
            },
        )
        .await
        .expect("terminal-write-failure accepted status"),
    )
    .expect("terminal-write-failure accepted status JSON");
    let persist_failure_revision = persist_failure_status["status_revision"]
        .as_u64()
        .expect("terminal-write-failure accepted revision");
    let persist_failure_cancel_raw = server
        .tachi_staff(rmcp::handler::server::wrapper::Parameters(
            serde_json::from_value::<tachi_params::TachiStaffParams>(serde_json::json!({
                "action": "cancel",
                "dispatch_id": persist_failure_id,
                "expected_status_revision": persist_failure_revision,
            }))
            .expect("terminal-write-failure cancellation parameters"),
        ))
        .await
        .expect("terminal-write-failure cancellation response");
    let persist_failure_cancel: Value = serde_json::from_str(&persist_failure_cancel_raw)
        .expect("terminal-write-failure cancellation JSON");
    let (persist_failure_terminal, persist_failure_result) =
        wait_for_staff_terminal(&persist_failure_dir).await;
    wait_for_staff_cleanup(persist_failure_id).await;
    assert_eq!(
        persist_failure_cancel["receipt"],
        "cancellation_unavailable"
    );
    assert_eq!(persist_failure_cancel["reason"], "persist_failed");
    assert_eq!(
        terminal_staff_state(&persist_failure_terminal),
        "TASK_STATE_FAILED"
    );
    assert!(persist_failure_result.contains("managed_cancelled"));
    assert_eq!(
        persist_failure_terminal["cancellation"],
        persist_failure_cancel
    );
    assert_eq!(
        persist_failure_terminal["cancellation"]["observed_status_revision"],
        persist_failure_terminal["status_revision"]
    );
    assert!(!server.managed_run_controls.contains(persist_failure_id));
    assert!(!persist_failure_dir.join("credentials").exists());
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, persist_failure_id).await,
        Some("TASK_STATE_FAILED".to_string())
    );
    let persist_failure_flow_lock_dir =
        crate::task_lifecycle::run_dir_for_flow_id("flow_1825_terminal_write_failure")
            .expect("valid terminal-write-failure flow run directory")
            .join(".dispatch-dedupe");
    assert!(
        !persist_failure_flow_lock_dir.exists()
            || std::fs::read_dir(&persist_failure_flow_lock_dir)
                .expect("terminal-write-failure flow lock directory")
                .next()
                .is_none(),
        "terminal-write fallback must release the Staff flow slot"
    );
    persist_failure_cleanup.disarm();
    drop(_persist_failure);

    // Credential cleanup participates in the same real Staff ->
    // background terminalization as process proof. Inject only this
    // isolated run root's cleanup call so the final writer must choose a
    // FAILED/unavailable receipt before the cancellation response exits.
    let _cleanup_failure = crate::dispatch_ops::install_managed_credential_cleanup_failure(
        temp_home.path(),
        temp_runs.path(),
    );
    let mut cleanup_request = kernel_request(cleanup_project);
    cleanup_request.profile = Some("kernel_opencode_write".to_string());
    cleanup_request.worker = Some("custom".to_string());
    cleanup_request.flow_id = Some("flow_1825_cleanup_failure".to_string());
    let cleanup_raw = kernel_start(&server, cleanup_request)
        .await
        .expect("cleanup-failure Staff start");
    let mut cleanup_guard = StaffCleanupGuard::arm(&cleanup_raw);
    cleanup_guard.track_process_group(&root_pid);
    let cleanup_start: Value = serde_json::from_str(&cleanup_raw).expect("cleanup start JSON");
    let cleanup_id = cleanup_start["dispatch_id"]
        .as_str()
        .expect("cleanup dispatch id");
    let cleanup_dir = dispatch_runs_root().join(cleanup_id);
    wait_for_managed_custom_processes(&cleanup_dir, &root_pid, &descendant_pid).await;
    let cleanup_status: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: cleanup_id.to_string(),
            },
        )
        .await
        .expect("cleanup accepted status"),
    )
    .expect("cleanup accepted status JSON");
    let cleanup_revision = cleanup_status["status_revision"]
        .as_u64()
        .expect("cleanup accepted revision");
    let cleanup_cancel_raw = server
        .tachi_staff(rmcp::handler::server::wrapper::Parameters(
            serde_json::from_value::<tachi_params::TachiStaffParams>(serde_json::json!({
                "action": "cancel",
                "dispatch_id": cleanup_id,
                "expected_status_revision": cleanup_revision,
            }))
            .expect("public cancellation parameters"),
        ))
        .await
        .expect("public cleanup failure cancellation response");
    let cleanup_cancel: Value =
        serde_json::from_str(&cleanup_cancel_raw).expect("cleanup failure cancellation JSON");
    assert_eq!(cleanup_cancel["receipt"], "cancellation_unavailable");
    assert_eq!(cleanup_cancel["reason"], "credential_cleanup_failed");
    let cleanup_final: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: cleanup_id.to_string(),
            },
        )
        .await
        .expect("cleanup terminal status"),
    )
    .expect("cleanup terminal status JSON");
    assert_eq!(terminal_staff_state(&cleanup_final), "TASK_STATE_FAILED");
    assert_eq!(
        cleanup_final["cancellation"]["receipt"],
        "cancellation_unavailable"
    );
    assert_eq!(
        cleanup_final["cancellation"]["reason"],
        "credential_cleanup_failed"
    );
    assert_eq!(
        cleanup_final["cancellation"]["observed_status_revision"], cleanup_final["status_revision"],
        "the durable cleanup-failure receipt must carry the committed root revision"
    );
    assert_eq!(
        cleanup_cancel, cleanup_final["cancellation"],
        "cleanup-failure response must return the already committed canonical receipt"
    );
    assert_eq!(
        cleanup_cancel_raw,
        serde_json::to_string(&cleanup_final["cancellation"])
            .expect("serialize canonical cancellation receipt"),
        "public cancel must return canonical JSON bytes without facade action/status injection"
    );
    assert_ne!(
        cleanup_final["cancellation"]["receipt"],
        "cancellation_confirmed"
    );
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, cleanup_id).await,
        Some("TASK_STATE_FAILED".to_string())
    );
    let global_cleanup_outcomes: i64 = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [cleanup_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count global cleanup failure outcomes");
    assert_eq!(global_cleanup_outcomes, 0);
    let cleanup_outcomes: i64 = server
        .with_named_project_store_read(cleanup_project, |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [cleanup_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count named-project cleanup failure outcomes");
    assert_eq!(cleanup_outcomes, 1);
    assert!(!server.managed_run_controls.contains(cleanup_id));
    let cleanup_flow_lock_dir =
        crate::task_lifecycle::run_dir_for_flow_id("flow_1825_cleanup_failure")
            .expect("valid cleanup-failure flow run directory")
            .join(".dispatch-dedupe");
    assert!(
        !cleanup_flow_lock_dir.exists()
            || std::fs::read_dir(&cleanup_flow_lock_dir)
                .expect("cleanup flow lock directory")
                .next()
                .is_none(),
        "cleanup failure must release the flow dispatch slot before responding"
    );
    wait_for_staff_cleanup(cleanup_id).await;
    cleanup_guard.disarm();
    drop(_cleanup_failure);

    // The actual result artifact is part of the managed terminal receipt:
    // a keyed write failure must turn the public cancel response into the
    // committed unavailable receipt only after cleanup and release.
    let _result_persist_failure = crate::dispatch_ops::install_managed_result_persist_failure(
        temp_home.path(),
        temp_runs.path(),
    );
    let mut result_failure_request = kernel_request(cleanup_project);
    result_failure_request.profile = Some("kernel_opencode_credentials".to_string());
    result_failure_request.worker = Some("custom".to_string());
    result_failure_request.flow_id = Some("flow_1825_result_persist_failure".to_string());
    let result_failure_raw = kernel_start(&server, result_failure_request)
        .await
        .expect("result-persist-failure Staff start");
    let mut result_failure_guard = StaffCleanupGuard::arm(&result_failure_raw);
    result_failure_guard.track_process_group(&root_pid);
    let result_failure_start: Value =
        serde_json::from_str(&result_failure_raw).expect("result-persist-failure start JSON");
    let result_failure_id = result_failure_start["dispatch_id"]
        .as_str()
        .expect("result-persist-failure dispatch id");
    let result_failure_dir = dispatch_runs_root().join(result_failure_id);
    assert!(
        result_failure_dir
            .join("credentials/opencode.json")
            .exists(),
        "the result-write discriminator must begin with the real OpenCode overlay"
    );
    wait_for_managed_custom_processes(&result_failure_dir, &root_pid, &descendant_pid).await;
    let result_failure_status: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: result_failure_id.to_string(),
            },
        )
        .await
        .expect("result-persist-failure accepted status"),
    )
    .expect("result-persist-failure accepted status JSON");
    let result_failure_revision = result_failure_status["status_revision"]
        .as_u64()
        .expect("result-persist-failure accepted revision");
    let result_failure_cancel_raw = server
        .tachi_staff(rmcp::handler::server::wrapper::Parameters(
            serde_json::from_value::<tachi_params::TachiStaffParams>(serde_json::json!({
                "action": "cancel",
                "dispatch_id": result_failure_id,
                "expected_status_revision": result_failure_revision,
            }))
            .expect("result-persist-failure cancellation parameters"),
        ))
        .await
        .expect("result-persist-failure cancellation response");
    let result_failure_cancel: Value = serde_json::from_str(&result_failure_cancel_raw)
        .expect("result-persist-failure cancellation JSON");
    let result_failure_final: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: result_failure_id.to_string(),
            },
        )
        .await
        .expect("result-persist-failure terminal status"),
    )
    .expect("result-persist-failure terminal status JSON");
    assert_eq!(
        terminal_staff_state(&result_failure_final),
        "TASK_STATE_FAILED"
    );
    assert_eq!(
        result_failure_final["result_written"], false,
        "a failed atomic artifact write must never claim a canonical result"
    );
    assert_eq!(
        result_failure_final["cancellation"]["reason"],
        "result_persist_failed"
    );
    assert_eq!(result_failure_cancel, result_failure_final["cancellation"]);
    assert_eq!(
        result_failure_cancel_raw,
        serde_json::to_string(&result_failure_final["cancellation"])
            .expect("serialize canonical result-persist receipt")
    );
    assert_ne!(
        result_failure_final["cancellation"]["receipt"],
        "cancellation_confirmed"
    );
    assert!(!result_failure_dir.join("result.md").exists());
    assert!(
        !result_failure_dir
            .join("credentials/opencode.json")
            .exists(),
        "result persistence failure must still clean the real OpenCode overlay"
    );
    assert!(!server.managed_run_controls.contains(result_failure_id));
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, result_failure_id).await,
        Some("TASK_STATE_FAILED".to_string())
    );
    let result_failure_outcomes: i64 = server
        .with_named_project_store_read(cleanup_project, |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [result_failure_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count result-persist failure outcomes");
    assert_eq!(result_failure_outcomes, 1);
    let result_failure_flow_lock_dir =
        crate::task_lifecycle::run_dir_for_flow_id("flow_1825_result_persist_failure")
            .expect("valid result-persist flow run directory")
            .join(".dispatch-dedupe");
    assert!(
        !result_failure_flow_lock_dir.exists()
            || std::fs::read_dir(&result_failure_flow_lock_dir)
                .expect("result-persist flow lock directory")
                .next()
                .is_none(),
        "result persistence failure must release the flow slot before responding"
    );
    wait_for_staff_cleanup(result_failure_id).await;
    result_failure_guard.disarm();
    drop(_result_persist_failure);
    let _ = std::fs::remove_file(&root_pid);
    let _ = std::fs::remove_file(&descendant_pid);

    let mut request = kernel_request(cleanup_project);
    request.profile = Some("kernel_opencode_write".to_string());
    request.worker = Some("custom".to_string());
    request.issue_ref = Some("kckylechen1/tachi#1825".to_string());
    request.flow_id = Some("flow_1825_managed_cancel_e2e".to_string());

    let raw = kernel_start(&server, request)
        .await
        .expect("managed custom Staff start is accepted");
    let mut cleanup_guard = StaffCleanupGuard::arm(&raw);
    cleanup_guard.track_process_group(&root_pid);
    let accepted: Value = serde_json::from_str(&raw).expect("accepted response JSON");
    let dispatch_id = accepted["dispatch_id"].as_str().expect("dispatch id");
    let run_dir = dispatch_runs_root().join(dispatch_id);
    wait_for_managed_custom_processes(&run_dir, &root_pid, &descendant_pid).await;

    let accepted_status: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: dispatch_id.to_string(),
            },
        )
        .await
        .expect("actual Staff status after accepted start"),
    )
    .expect("accepted canonical status JSON");
    let accepted_revision = accepted_status["status_revision"]
        .as_u64()
        .expect("accepted managed custom status revision");
    assert_eq!(accepted_status["state"], "TASK_STATE_WORKING");
    assert_eq!(
        accepted_status["execution_classification"],
        "managed_custom"
    );
    let root_before_cancel: libc::pid_t = std::fs::read_to_string(&root_pid)
        .expect("root pid before cancellation")
        .trim()
        .parse()
        .expect("numeric root pid before cancellation");
    let descendant_before_cancel: libc::pid_t = std::fs::read_to_string(&descendant_pid)
        .expect("descendant pid before cancellation")
        .trim()
        .parse()
        .expect("numeric descendant pid before cancellation");
    // SAFETY: both PIDs come from this test's freshly-created fixture and
    // `getpgid` only queries kernel process metadata.
    assert_eq!(
        unsafe { libc::getpgid(root_before_cancel) },
        root_before_cancel
    );
    // SAFETY: the descendant is emitted by the same freshly-created fixture.
    assert_eq!(
        unsafe { libc::getpgid(descendant_before_cancel) },
        root_before_cancel
    );
    // SAFETY: signal 0 is a non-mutating existence probe for the fixture.
    assert_eq!(unsafe { libc::kill(root_before_cancel, 0) }, 0);
    // SAFETY: signal 0 is a non-mutating existence probe for the fixture.
    assert_eq!(unsafe { libc::kill(descendant_before_cancel, 0) }, 0);
    let (_dequeue_guard, dequeued, continue_cancel, observation) =
        crate::dispatch_ops::install_managed_cancel_dequeue_barrier(&run_dir);
    let cancel_server = server.clone();
    let cancel_dispatch_id = dispatch_id.to_string();
    let cancel_task = tokio::task::spawn(async move {
        cancel_server
            .tachi_staff(rmcp::handler::server::wrapper::Parameters(
                serde_json::from_value::<tachi_params::TachiStaffParams>(serde_json::json!({
                    "action": "cancel",
                    "dispatch_id": cancel_dispatch_id,
                    "expected_status_revision": accepted_revision,
                }))
                .expect("public cancellation parameters"),
            ))
            .await
            .expect("public Staff cancel response")
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || dequeued.recv().expect("dequeued cancellation")),
    )
    .await
    .expect("Staff cancellation must reach the live production runner")
    .expect("dequeue observation join");
    let duplicate: Value = serde_json::from_str(
        &staff_cancel(
            &server,
            StaffCancelRequest {
                dispatch_id: dispatch_id.to_string(),
                expected_status_revision: accepted_revision + 1,
            },
        )
        .await
        .expect("duplicate Staff cancellation response"),
    )
    .expect("duplicate cancellation JSON");
    assert_eq!(duplicate["receipt"], "cancellation_unavailable");
    assert_eq!(duplicate["reason"], "duplicate_cancellation");
    std::fs::write(&cancel_trigger, b"trigger").expect("trigger live-root acknowledgement");
    for _ in 0..360 {
        if cancel_ack.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&cancel_ack).expect("live-root acknowledgement"),
        format!("{root_before_cancel}:{cancel_nonce}\n"),
        "only a live root can acknowledge cancellation dequeue"
    );
    continue_cancel.send(()).expect("release production runner");
    let observation = tokio::task::spawn_blocking(move || {
        observation
            .recv()
            .expect("dequeued runner try_wait observation")
    })
    .await
    .expect("try_wait observation join");
    assert_eq!(observation.run_dir, run_dir);
    assert_eq!(observation.child_pid, Some(root_before_cancel as u32));
    assert_eq!(observation.try_wait_result, "running");
    assert_eq!(observation.sigterm_result, "delivered");
    assert!(matches!(
        observation.reap_result,
        "root_reaped" | "root_killed"
    ));
    assert!(observation.group_absent);
    assert_eq!(
        observation.runner_error.as_deref(),
        Some("managed_cancelled")
    );
    assert_eq!(
        observation.termination_proof,
        Some("unix_process_group_absent")
    );
    assert!(observation.finalization_directive);
    assert_eq!(
        observation.finalization_result,
        Some("cancellation_confirmed")
    );
    assert_eq!(
        observation.canonical_receipt.as_deref(),
        Some("cancellation_confirmed")
    );
    assert_eq!(observation.canonical_reason, None);
    assert_eq!(
        observation.canonical_state.as_deref(),
        Some("TASK_STATE_CANCELED")
    );
    let cancel_raw = cancel_task.await.expect("Staff cancel task");
    let cancel: Value = serde_json::from_str(&cancel_raw).expect("cancel response JSON");
    assert_no_cancellation_control_keys(&cancel);
    assert_eq!(
        cancel["receipt"], "cancellation_confirmed",
        "managed Staff cancellation must confirm its live runner: {cancel}"
    );
    assert_eq!(cancel["expected_status_revision"], accepted_revision);
    assert_eq!(cancel["termination_proof"], "unix_process_group_absent");

    // The facade releases this response only after the production terminal
    // owner persists cancellation, cleans run-scoped credentials, releases
    // the flow slot, and removes its managed registry entry.
    let instant_status: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: dispatch_id.to_string(),
            },
        )
        .await
        .expect("Staff status at cancellation response boundary"),
    )
    .expect("instant canonical status JSON");
    assert_eq!(terminal_staff_state(&instant_status), "TASK_STATE_CANCELED");
    assert_eq!(
        instant_status["cancellation"]["receipt"],
        "cancellation_confirmed"
    );
    assert_eq!(
        instant_status["cancellation"]["observed_status_revision"],
        instant_status["status_revision"],
        "final canonical receipt must carry its committed root revision"
    );
    assert_eq!(
        instant_status["cancellation"]["observed_status_revision"],
        accepted_revision + 2,
        "requested then confirmed cancellation advances exactly twice"
    );
    assert_eq!(cancel, instant_status["cancellation"]);
    assert_eq!(
        cancel_raw,
        serde_json::to_string(&instant_status["cancellation"])
            .expect("serialize canonical live cancellation receipt")
    );
    assert!(
        !run_dir.join("credentials").exists(),
        "ephemeral credential materialization must be gone before cancel responds"
    );
    let flow_lock_dir = crate::task_lifecycle::run_dir_for_flow_id("flow_1825_managed_cancel_e2e")
        .expect("valid flow run directory")
        .join(".dispatch-dedupe");
    assert!(
        !flow_lock_dir.exists()
            || std::fs::read_dir(&flow_lock_dir)
                .expect("flow lock directory")
                .next()
                .is_none(),
        "flow dispatch slot must be released before cancel responds"
    );
    assert!(
        !server.managed_run_controls.contains(dispatch_id),
        "managed registry entry must be removed before cancel responds"
    );
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, dispatch_id).await,
        Some("TASK_STATE_CANCELED".to_string()),
        "confirmed managed cancellation must bypass watchdog FAILED kanban projection"
    );
    let cancellation_outcomes: i64 = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [dispatch_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count cancellation outcomes");
    assert_eq!(
        cancellation_outcomes, 0,
        "confirmed managed cancellation must not synthesize a watchdog failure outcome"
    );

    let (terminal, _result) = wait_for_staff_terminal(&run_dir).await;
    assert_no_cancellation_control_keys(&terminal["cancellation"]);
    wait_for_staff_cleanup(dispatch_id).await;
    assert!(
        !server.managed_run_controls.contains(dispatch_id),
        "background cleanup must remove managed cancellation authority"
    );
    cleanup_guard.disarm();
    assert_eq!(terminal_staff_state(&terminal), "TASK_STATE_CANCELED");
    assert_eq!(
        terminal["cancellation"]["receipt"],
        "cancellation_confirmed"
    );
    assert_eq!(
        terminal["cancellation"]["expected_status_revision"], accepted_revision,
        "request receipt precedes the confirmation written from the same checked revision"
    );
    assert_eq!(
        terminal["cancellation"]["observed_status_revision"], terminal["status_revision"],
        "the persisted receipt must carry the final committed root revision"
    );
    assert_eq!(
        terminal["cancellation"]["observed_status_revision"],
        accepted_revision + 2,
        "the final receipt is written after requested then confirmed revisions"
    );
    assert_eq!(cancel, terminal["cancellation"]);
    assert!(
        terminal["status_revision"]
            .as_u64()
            .expect("terminal revision")
            >= accepted_revision + 2,
        "terminal persistence must not regress the confirmed revision"
    );
    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("canonical trajectory");
    for event in [
        "dispatch_received",
        "execute_started",
        "subprocess_finished",
    ] {
        assert!(
            trajectory.contains(&format!(r#""event":"{event}""#)),
            "trajectory missing {event}: {trajectory}"
        );
    }
    assert!(
        wait_for_test_process_group_absence(root_before_cancel).await,
        "managed process group must be absent"
    );
    cleanup_guard.disarm();
}

/// Completion wins before the facade is asked to cancel: the post-cleanup
/// cancellation probe must not replace the terminal receipt or result.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // serializes process-global fake-worker environment through terminal cleanup
async fn staff_cancel_after_managed_custom_completion_preserves_terminal_receipt() {
    let _environment = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp canonical run root");
    let temp_bin = tempfile::tempdir().expect("temp fake worker bin");
    write_completed_managed_custom_worker(temp_bin.path());
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
    let repository = StaffRepositoryFixture::new(temp_home.path());
    let _cwd = CurrentDirGuard::set(&repository.repo);
    let temp_worktrees = tempfile::tempdir().expect("isolated Staff worktrees");
    let _worktrees =
        crate::test_support::EnvRestore::set_path("TACHI_WORKTREES_ROOT", temp_worktrees.path());
    let _fixture_cleanup = CertifiedWorktreeCleanupGuard::arm(temp_worktrees.path(), find_worktree);
    let _transport = crate::test_support::EnvRestore::set("TACHI_OPENCODE_TRANSPORT", "cli");
    let _v2 = crate::test_support::EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let _review = crate::test_support::EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let server = test_server();
    let (project, _) = server
        .resolve_server_named_project_binding("tachi")
        .expect("resolve the registered fixture project");
    let mut request = kernel_request(&project);
    request.profile = Some("kernel_opencode_write".to_string());
    request.worker = Some("custom".to_string());
    request.issue_ref = Some("kckylechen1/tachi#1825".to_string());
    request.flow_id = Some("flow_1825_managed_completion_winner".to_string());

    let raw = kernel_start(&server, request)
        .await
        .expect("managed custom Staff start is accepted");
    let mut cleanup_guard = StaffCleanupGuard::arm(&raw);
    let accepted: Value = serde_json::from_str(&raw).expect("accepted response JSON");
    let dispatch_id = accepted["dispatch_id"].as_str().expect("dispatch id");
    let run_dir = dispatch_runs_root().join(dispatch_id);
    let (terminal, terminal_result) = wait_for_staff_terminal(&run_dir).await;
    wait_for_staff_cleanup(dispatch_id).await;
    assert_eq!(terminal_staff_state(&terminal), "TASK_STATE_COMPLETED");
    assert!(terminal_result.contains("staff managed custom worker"));
    assert!(
        !server.managed_run_controls.contains(dispatch_id),
        "terminal cleanup must remove managed cancellation authority"
    );
    // Durable managed-run identity (additive S1 assertion): the real
    // managed-custom start path stamps a closed identity record carrying
    // the owning controller epoch, and the terminal rewrite carries it
    // forward without any process-control or secret material.
    let identity = terminal["managed_run_identity"]
        .as_object()
        .expect("durable managed-run identity on the terminal receipt");
    assert_eq!(
        identity["controller_epoch_id"], server.controller_epoch,
        "the accepting controller epoch is recorded"
    );
    assert_eq!(identity["lifecycle_mode"], "TachiManagedBatch");
    assert_eq!(identity["managed_run_id"], dispatch_id);
    assert_eq!(identity["work_claim_ref"], Value::Null);
    let identity_serialized =
        serde_json::to_string(&terminal["managed_run_identity"]).expect("serialize identity");
    for forbidden in [
        "\"pid\"",
        "\"pgid\"",
        "\"signal\"",
        "\"credential\"",
        "\"token\"",
    ] {
        assert!(
            !identity_serialized.contains(forbidden),
            "durable identity must not carry process-control/secret key {forbidden}"
        );
    }
    let terminal_revision = terminal["status_revision"]
        .as_u64()
        .expect("completed terminal revision");

    let cancel: Value = serde_json::from_str(
        &staff_cancel(
            &server,
            StaffCancelRequest {
                dispatch_id: dispatch_id.to_string(),
                expected_status_revision: terminal_revision,
            },
        )
        .await
        .expect("post-completion Staff cancel response"),
    )
    .expect("post-completion cancellation JSON");
    assert_eq!(cancel["receipt"], "cancellation_unavailable");

    let after: Value = serde_json::from_str(
        &staff_status(
            &server,
            StaffStatusRequest {
                dispatch_id: dispatch_id.to_string(),
            },
        )
        .await
        .expect("post-completion Staff status"),
    )
    .expect("post-completion canonical status JSON");
    let after_result = std::fs::read_to_string(run_dir.join("result.md"))
        .expect("post-completion canonical result");
    assert_eq!(
        after, terminal,
        "cancel must not replace a completed receipt"
    );
    assert_eq!(
        after_result, terminal_result,
        "cancel must not replace result.md"
    );
    assert_eq!(after["status_revision"], terminal_revision);
    cleanup_guard.disarm();
}
