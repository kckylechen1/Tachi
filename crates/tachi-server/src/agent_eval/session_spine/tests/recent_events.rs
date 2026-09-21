use super::*;

async fn read_tail(server: &MemoryServer, attachment_id: &str, limit: Option<usize>) -> Value {
    eval(
        server,
        TachiAgentEvalParams {
            limit,
            ..spine_params("get_session_state", attachment_id)
        },
    )
    .await
}

#[tokio::test]
async fn recent_events_bounds_empty_zero_default_and_oversize_over_facade() {
    let server = test_server();
    seed_valid_admission(&server, GRANT_DELEGATE);
    let id = attach(&server, "tail-bounds", &["observe", "events"]).await;
    let empty = read_tail(&server, &id, None).await;
    assert_eq!(empty["event_limit"], 20);
    assert_eq!(empty["events"], json!([]));
    assert_eq!(empty["events_truncated"], false);
    assert_eq!(empty["canonical_state"]["canonical_state"], Value::Null);
    let zero_empty = read_tail(&server, &id, Some(0)).await;
    assert_eq!(zero_empty["events_truncated"], false);

    for revision in 1..=101 {
        eval(
            &server,
            event_params(&id, &format!("progress-{revision}"), "progress", revision),
        )
        .await;
    }
    let default = read_tail(&server, &id, None).await;
    assert_eq!(default["events"].as_array().unwrap().len(), 20);
    assert_eq!(default["events"][0]["event_id"], "progress-82");
    assert_eq!(default["events_truncated"], true);
    let capped = read_tail(&server, &id, Some(usize::MAX)).await;
    assert_eq!(capped["event_limit"], 100);
    assert_eq!(capped["events"].as_array().unwrap().len(), 100);
    assert_eq!(capped["events"][0]["event_id"], "progress-2");
    assert_eq!(capped["events"][99]["event_id"], "progress-101");
    assert_eq!(capped["events_truncated"], true);
    let zero = read_tail(&server, &id, Some(0)).await;
    assert_eq!(zero["event_limit"], 0);
    assert_eq!(zero["events"], json!([]));
    assert_eq!(zero["events_truncated"], true);
    assert_eq!(zero["canonical_state"], default["canonical_state"]);
}

#[tokio::test]
async fn recent_events_keep_late_redundant_conflicting_receipts_without_replays() {
    let server = test_server();
    seed_valid_admission(&server, GRANT_DELEGATE);
    let id = attach(&server, "tail-order", &["observe", "events"]).await;
    let terminal = TachiAgentEvalParams {
        session_event_outcome: Some("completed".to_string()),
        payload_digest: Some("sha256:abc".to_string()),
        authority_confirmation_ref: Some("host-confirmation".to_string()),
        event_summary: Some("完整的 bounded result".to_string()),
        ..event_params(&id, "terminal", "terminal", 10)
    };
    eval(&server, terminal.clone()).await;
    eval(&server, event_params(&id, "late", "progress", 2)).await;
    eval(&server, terminal).await;
    eval(
        &server,
        TachiAgentEvalParams {
            session_event_outcome: Some("completed".to_string()),
            ..event_params(&id, "redundant", "terminal", 11)
        },
    )
    .await;
    let conflict = eval(
        &server,
        TachiAgentEvalParams {
            session_event_outcome: Some("failed".to_string()),
            ..event_params(&id, "conflict", "terminal", 12)
        },
    )
    .await;
    let full = read_tail(&server, &id, Some(4)).await;
    let events = full["events"].as_array().unwrap();
    assert_eq!(events.len(), 4, "replayed event must not append a row");
    assert_eq!(
        full["events_truncated"], false,
        "exact boundary is complete"
    );
    assert_eq!(events[0]["summary"], "完整的 bounded result");
    assert_eq!(events[0]["payload_digest"], "sha256:abc");
    assert_eq!(events[0]["authority_confirmation_ref"], "host-confirmation");
    assert_eq!(events[0]["attachment_id"], id);
    assert_eq!(events[0]["source_host_identity"], "host-1");
    assert_eq!(events[0]["occurred_at"], "2026-08-29T00:00:00Z");
    assert!(!events[0]["ingested_at"].as_str().unwrap().is_empty());
    assert_eq!(events[0].as_object().unwrap().len(), 12);
    assert_eq!(events[0]["kind"], "terminal");
    assert_eq!(events[0]["outcome"], "completed");
    assert_eq!(events[1]["event_id"], "late");
    assert_eq!(events[1]["source_revision"], 2);
    assert_eq!(events[1]["outcome"], Value::Null);
    assert_eq!(events[2]["event_id"], "redundant");
    assert_eq!(events[3]["event_id"], "conflict");
    assert!(events
        .windows(2)
        .all(|pair| pair[0]["event_row_id"].as_i64() < pair[1]["event_row_id"].as_i64()));
    assert_eq!(full["canonical_state"], conflict["canonical_state"]);
    assert_eq!(
        full["canonical_state"]["canonical_state"],
        "inconsistent_reconciling"
    );
    let tail = read_tail(&server, &id, Some(3)).await;
    assert_eq!(tail["events"][0]["event_id"], "late");
    assert_eq!(tail["events_truncated"], true);
    assert_eq!(
        read_tail(&server, &id, Some(4)).await,
        full,
        "read is non-mutating"
    );
}

#[tokio::test]
async fn recent_events_allow_observe_and_released_claim_but_authorize_zero() {
    let server = test_server();
    seed_valid_admission(&server, GRANT_DELEGATE);
    let id = attach(&server, "tail-auth", &["observe", "events"]).await;
    eval(&server, event_params(&id, "started", "started", 1)).await;
    server
        .with_global_store(|store| {
            memcore::release_work_claim(store.connection(), "claim-1", "agent-1", 0, "done")
                .map_err(|error| error.to_string())
        })
        .expect("release claim");
    server.set_tool_profile(Some(tachi_hub::ToolProfile::observe()));
    let released = read_tail(&server, &id, None).await;
    assert_eq!(released["events"][0]["event_id"], "started");

    // A foreign current host must not learn whether the attachment exists,
    // including when no event content was requested.
    server.set_work_claim_connection(
        Some("host-2".to_string()),
        "connection-2".to_string(),
        "self_asserted".to_string(),
    );
    let mut errors = Vec::new();
    for attachment_id in [id.as_str(), "missing-attachment"] {
        let error = crate::agent_eval::handle_agent_eval(
            &server,
            TachiAgentEvalParams {
                host_identity: Some("host-2".to_string()),
                admission_receipt_ref: Some("admission-2".to_string()),
                limit: Some(0),
                ..spine_params("get_session_state", attachment_id)
            },
        )
        .await
        .expect_err("foreign host must be denied even at limit zero");
        assert!(error.contains("not found"), "{error}");
        errors.push(error);
    }
    assert_eq!(errors[0], errors[1]);
}

#[tokio::test]
async fn recent_events_reject_stale_admission_after_reconnect_even_at_zero() {
    let server = test_server();
    seed_valid_admission(&server, GRANT_DELEGATE);
    let id = attach(&server, "tail-reconnect", &["observe", "events"]).await;
    eval(&server, event_params(&id, "started", "started", 1)).await;
    server
        .with_global_store(|store| {
            record_unverified_admission(
                store.connection(),
                "admission-2",
                "host-1",
                "connection-2",
                UnverifiedAdmissionState::SelfAsserted,
            )
            .map_err(|error| error.to_string())
        })
        .expect("fresh admission");
    server.set_work_claim_connection(
        Some("host-1".to_string()),
        "connection-2".to_string(),
        "self_asserted".to_string(),
    );
    eval(
        &server,
        TachiAgentEvalParams {
            admission_receipt_ref: Some("admission-2".to_string()),
            ..spine_params("reconnect_session", &id)
        },
    )
    .await;
    for limit in [Some(0), None] {
        let error = crate::agent_eval::handle_agent_eval(
            &server,
            TachiAgentEvalParams {
                limit,
                ..spine_params("get_session_state", &id)
            },
        )
        .await
        .expect_err("old admission cannot read receipts");
        assert!(error.contains("not found"), "{error}");
    }
    let current = eval(
        &server,
        TachiAgentEvalParams {
            admission_receipt_ref: Some("admission-2".to_string()),
            ..spine_params("get_session_state", &id)
        },
    )
    .await;
    assert_eq!(current["events"][0]["event_id"], "started");
}
