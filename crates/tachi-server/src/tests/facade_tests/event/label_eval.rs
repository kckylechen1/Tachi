use super::*;

#[tokio::test]
async fn tachi_event_label_eval_compares_outcomes_to_gold_reviews() {
    let server = make_server();

    let mut label = tachi_event_params("emit");
    label.id = Some("label-event-1".to_string());
    label.source_repo = Some("sigil".to_string());
    label.adapter = Some("labeler".to_string());
    label.domain = Some("coding".to_string());
    label.session_id = Some("session-label-eval".to_string());
    label.actor = Some("continuity_labeler".to_string());
    label.event_type = Some("session.outcome".to_string());
    label.authority = Some("review_signal_only".to_string());
    label.effects = vec!["scoring".to_string()];
    label.projection_hints = vec!["outcome".to_string()];
    label.payload = Some(json!({
        "outcome": "partial_reframe",
        "evidence_basis": "external_evidence",
    }));
    crate::event_ops::handle_tachi_event(&server, label)
        .await
        .expect("emit label");

    let mut review = tachi_event_params("emit");
    review.id = Some("label-review-1".to_string());
    review.source_repo = Some("sigil".to_string());
    review.adapter = Some("human-review".to_string());
    review.domain = Some("coding".to_string());
    review.session_id = Some("session-label-eval".to_string());
    review.actor = Some("reviewer".to_string());
    review.event_type = Some("session.outcome.review".to_string());
    review.authority = Some("derived_evidence".to_string());
    review.projection_hints = vec!["evidence_gate".to_string()];
    review.payload = Some(json!({
        "target_event_id": "label-event-1",
        "gold_outcome": "partial_reframe",
        "gold_evidence_basis": "external_evidence",
    }));
    crate::event_ops::handle_tachi_event(&server, review)
        .await
        .expect("emit review");

    let eval = crate::event_ops::handle_tachi_event(&server, tachi_event_params("label_eval"))
        .await
        .expect("label eval");
    let parsed: Value = serde_json::from_str(&eval).expect("label eval JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["reviewed"], json!(1));
    assert_eq!(parsed["missing_targets"], json!(0));
    assert_eq!(parsed["outcome_accuracy"], json!(1.0));
    assert_eq!(parsed["evidence_basis_accuracy"], json!(1.0));
    assert_eq!(parsed["full_match_rate"], json!(1.0));
}

#[tokio::test]
async fn tachi_event_label_eval_runs_heldout_fixture() {
    let server = make_server();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../fixtures/continuity_label_eval_heldout.json"
    ))
    .expect("heldout fixture JSON");
    for event in fixture["events"].as_array().expect("fixture events") {
        let mut emit = tachi_event_params("emit");
        emit.id = event.get("id").and_then(Value::as_str).map(str::to_string);
        emit.source_repo = event
            .get("source_repo")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.adapter = event
            .get("adapter")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.domain = event
            .get("domain")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.session_id = event
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.actor = event
            .get("actor")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.event_type = event
            .get("event_type")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.authority = event
            .get("authority")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.effects = event
            .get("effects")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        emit.projection_hints = event
            .get("projection_hints")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        emit.payload = event.get("payload").cloned();
        crate::event_ops::handle_tachi_event(&server, emit)
            .await
            .expect("emit heldout event");
    }

    let eval = crate::event_ops::handle_tachi_event(&server, tachi_event_params("label_eval"))
        .await
        .expect("label eval");
    let parsed: Value = serde_json::from_str(&eval).expect("label eval JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["reviewed"], json!(1));
    assert_eq!(parsed["outcome_accuracy"], json!(1.0));
    assert_eq!(parsed["evidence_basis_accuracy"], json!(1.0));
}

struct LabelTargetFixtureChild(std::process::Child);

impl Drop for LabelTargetFixtureChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn run_label_target_fixture(test: &str, body: impl FnOnce()) {
    const SELECTOR: &str = "TACHI_LABEL_TARGET_FIXTURE_SELECTOR";
    const ROOT: &str = "TACHI_LABEL_TARGET_FIXTURE_ROOT";
    if std::env::var(SELECTOR).ok().as_deref() == Some(test) {
        let root = std::path::PathBuf::from(std::env::var_os(ROOT).expect("private fixture root"));
        assert_eq!(std::env::current_dir().expect("child cwd"), root);
        body();
        std::fs::write(root.join("completed"), test).expect("child completion witness");
        return;
    }

    // Only filesystem/process setup occurs in the ambient parent; no server or LLM exists here.
    let root = tempfile::tempdir().expect("private label target fixture process root");
    let root_path = root.path().canonicalize().expect("canonical private root");
    let home = root_path.join("home");
    let temp = root_path.join("tmp");
    let empty_path = root_path.join("empty-path");
    for directory in [&home, &temp, &empty_path] {
        std::fs::create_dir(directory).expect("private child directory");
    }
    let log_path = root_path.join("child.log");
    let output = std::fs::File::create(&log_path).expect("private child log");
    let exact = format!("tests::facade_tests::event::label_eval::{test}");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .env_clear()
        .current_dir(&root_path)
        .args(["--exact", exact.as_str(), "--nocapture"])
        .env(SELECTOR, test)
        .env(ROOT, &root_path)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("TACHI_HOME", &home)
        .env(
            "TACHI_TEST_TEMPLATE_CACHE_ROOT",
            root_path.join("template-cache"),
        )
        .env("TMPDIR", &temp)
        .env("TMP", &temp)
        .env("TEMP", &temp)
        .env("PATH", &empty_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(
            output.try_clone().expect("clone child log"),
        ))
        .stderr(std::process::Stdio::from(output));
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    let mut child = LabelTargetFixtureChild(command.spawn().expect("spawn private test child"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        if let Some(status) = child.0.try_wait().expect("poll owned child") {
            assert!(
                status.success(),
                "private fixture failed: {status}\n{}",
                std::fs::read_to_string(&log_path).expect("read private child log")
            );
            assert_eq!(
                std::fs::read_to_string(root_path.join("completed"))
                    .expect("selected child actually completed"),
                test
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "private label target fixture timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

async fn emit_target_fixture_event(
    server: &crate::MemoryServer,
    id: &str,
    session: &str,
    review: bool,
    payload: Value,
) {
    let mut params = tachi_event_params("emit");
    params.id = Some(id.to_string());
    params.source_repo = Some("private-label-fixture".to_string());
    params.adapter = Some("fixture".to_string());
    params.domain = Some("coding".to_string());
    params.session_id = Some(session.to_string());
    params.actor = Some("fixture".to_string());
    params.event_type = Some(
        if review {
            "session.outcome.review"
        } else {
            "session.outcome"
        }
        .to_string(),
    );
    params.authority = Some(
        if review {
            "derived_evidence"
        } else {
            "review_signal_only"
        }
        .to_string(),
    );
    params.payload = Some(payload);
    crate::event_ops::handle_tachi_event(server, params)
        .await
        .expect("emit private event");
}

#[test]
fn tachi_event_label_eval_explicit_target_never_substitutes_session_outcome() {
    run_label_target_fixture(
        "tachi_event_label_eval_explicit_target_never_substitutes_session_outcome",
        || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("private runtime");
            runtime.block_on(async {
            for alias in ["target_event_id", "event_id", "label_event_id"] {
                for case in ["missing", "exact", "absent", "no_session_outcome", "zero"] {
                    let server = make_server();
                    if case != "no_session_outcome" && case != "zero" {
                        emit_target_fixture_event(&server, "outcome-B", "session-S", false, json!({"outcome":"partial_reframe", "evidence_basis":"external_evidence"})).await;
                    }
                    if case == "exact" {
                        emit_target_fixture_event(&server, "outcome-A", "session-S", false, json!({"outcome":"ai_corrected", "evidence_basis":"external_evidence"})).await;
                    }
                    if case != "zero" {
                        let mut payload = json!({"gold_outcome":"partial_reframe", "gold_evidence_basis":"external_evidence"});
                        if case != "absent" {
                            payload[alias] = json!("outcome-A");
                        }
                        emit_target_fixture_event(&server, "review-A", "session-S", true, payload).await;
                    }
                    let before = crate::event_ops::handle_tachi_event(&server, tachi_event_params("query")).await.expect("snapshot stored events");
                    let result = crate::event_ops::handle_tachi_event(&server, tachi_event_params("label_eval")).await.expect("evaluate private labels");
                    let result: Value = serde_json::from_str(&result).expect("evaluation JSON");
                    let missing = case == "missing" || case == "no_session_outcome";
                    assert_eq!(result["missing_targets"], json!(usize::from(missing)), "{alias}/{case}: explicit missing target must not substitute outcome-B");
                    assert_eq!(result["reviewed"], json!(usize::from(case == "exact" || case == "absent")), "{alias}/{case}");
                    if missing || case == "zero" {
                        for ratio in ["outcome_accuracy", "evidence_basis_accuracy", "full_match_rate"] {
                            assert!(result[ratio].is_null(), "{alias}/{case}/{ratio}");
                        }
                    }
                    if missing {
                        assert_eq!(result["rows"][0]["target_event_id"], json!("outcome-A"));
                        assert_eq!(result["rows"][0]["status"], json!("missing_target"));
                        assert!(result["rows"][0].get("label_event_id").is_none());
                    } else if case == "exact" {
                        assert_eq!(result["rows"][0]["label_event_id"], json!("outcome-A"));
                        assert_eq!(result["outcome_accuracy"], json!(0.0));
                    } else if case == "absent" {
                        assert_eq!(result["rows"][0]["label_event_id"], json!("outcome-B"));
                        assert_eq!(result["full_match_rate"], json!(1.0));
                    } else {
                        assert_eq!(result["rows"], json!([]));
                    }
                    let after = crate::event_ops::handle_tachi_event(&server, tachi_event_params("query")).await.expect("read stored events after evaluation");
                    assert_eq!(before, after, "label evaluation must preserve stored event JSON");
                }
            }
        });
        },
    );
}
