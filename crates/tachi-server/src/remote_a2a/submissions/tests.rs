use super::*;

fn fixture() -> (RemoteSubmission, Value) {
    let now = jiff::Timestamp::now().to_string();
    let submission = RemoteSubmission {
        submission_id: "remote-submit-test".into(),
        origin: "origin".into(),
        origin_admission_id: "local-admission".into(),
        binding: RemoteSubmissionBinding {
            issuer_id: "issuer".into(),
            canonical_project: "owner/repo".into(),
            destination: "destination".into(),
            destination_host_id: "host-destination".into(),
            destination_key_sha256: "b".repeat(64),
            endpoint: "https://localhost:9443".into(),
            repository: "owner/repo".into(),
            job: "review_revision".into(),
            head_sha: "a".repeat(40),
            origin_claim_id: "claim".into(),
            origin_claim_version: 4,
        },
        idempotency_key: "key".into(),
        nonce: "nonce".into(),
        trust_domain: "owner:domain".into(),
        body: None,
        body_digest: "c".repeat(64),
        issued_at: now.clone(),
        expires_at: now.clone(),
        current_state: "in_flight".into(),
        state_version: 2,
        attempts: 1,
        attempt_until: None,
        last_error: None,
        retry_allowed: true,
        remote_task_id: None,
        remote_receipt: None,
        updated_at: now.clone(),
    };
    let task = json!({"id":"remote-task-test","contextId":"context:remote-task-test",
    "status":{"state":"TASK_STATE_REJECTED","timestamp":now},"metadata":{(PROVENANCE):{
        "schema_version":"tachi.remote_receipt.v1","task_id":"remote-task-test","input_digest":"c".repeat(64),
        "origin":"origin","origin_admission_ref":"verified-origin","destination":"destination","destination_host_id":"host-destination",
        "trust_domain":"owner:domain","repository":"owner/repo","head_sha":"a".repeat(40),"job":"review_revision",
        "origin_claim":{"claim_id":"claim","transition_version":4},"local_claim":null,"local_run_id":null,"revision":2,
        "receipts":[{"receipt_id":"received-1","revision":1,"fact":"received","occurred_at":now,"actor":"destination","admission_ref":"gateway-local","detail":null},
            {"receipt_id":"refused-2","revision":2,"fact":"authorization_refused","occurred_at":now,"actor":"destination","admission_ref":"gateway-local","detail":{"reason":"execution_unavailable"}}]
    }}});
    (submission, task)
}

#[test]
fn only_exact_host_refusal_can_become_terminal_origin_truth() {
    let (submission, task) = fixture();
    assert!(valid_refusal(&task, &submission));
    for pointer in [
        "/status/state",
        "/metadata/urn:tachi:a2a:ext:provenance-receipt:v1/input_digest",
        "/metadata/urn:tachi:a2a:ext:provenance-receipt:v1/destination_host_id",
        "/metadata/urn:tachi:a2a:ext:provenance-receipt:v1/origin_claim/transition_version",
        "/metadata/urn:tachi:a2a:ext:provenance-receipt:v1/receipts/1/actor",
        "/metadata/urn:tachi:a2a:ext:provenance-receipt:v1/local_run_id",
    ] {
        let mut changed = task.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("untrusted completion or other task");
        assert!(!valid_refusal(&changed, &submission), "{pointer}");
    }
    let response = json!({"jsonrpc":"2.0","id":submission.submission_id,"result":{"task":task}});
    assert!(matches!(
        interpret_response(&serde_json::to_vec(&response).unwrap(), &submission),
        RemoteSubmissionResult::Refused { .. }
    ));
    let refusal = json!({"jsonrpc":"2.0","id":submission.submission_id,"error":{"code":-32602,"message":"Invalid parameters"}});
    assert!(matches!(
        interpret_response(&serde_json::to_vec(&refusal).unwrap(), &submission),
        RemoteSubmissionResult::Unknown {
            retry_allowed: false,
            ..
        }
    ));
}

#[test]
fn response_duplicates_and_unexpected_content_never_become_receipts() {
    let (submission, mut task) = fixture();
    task["artifacts"] = json!([{"private":"unexpected content"}]);
    assert!(!valid_refusal(&task, &submission));
    assert!(matches!(
        interpret_response(
            br#"{"jsonrpc":"2.0","id":"remote-submit-test","id":"other","error":{}}"#,
            &submission
        ),
        RemoteSubmissionResult::Unknown {
            retry_allowed: false,
            ..
        }
    ));
}

#[test]
fn refusal_receipts_stamped_by_a_clock_ahead_are_accepted_within_the_tolerance() {
    let (submission, mut task) = fixture();
    let half = REMOTE_CLOCK_SKEW_TOLERANCE_SECS / 2;
    // The destination stamped both receipts with a clock that runs `ahead`
    // seconds faster than this origin.
    for ahead in [half, REMOTE_CLOCK_SKEW_TOLERANCE_SECS + half] {
        let now = jiff::Timestamp::now();
        let received = (now + jiff::SignedDuration::from_secs(ahead)).to_string();
        let refused = (now + jiff::SignedDuration::from_secs(ahead + 1)).to_string();
        task["metadata"][PROVENANCE]["receipts"][0]["occurred_at"] = json!(received);
        task["metadata"][PROVENANCE]["receipts"][1]["occurred_at"] = json!(refused);
        task["status"]["timestamp"] = json!(refused);
        assert_eq!(
            valid_refusal(&task, &submission),
            ahead <= REMOTE_CLOCK_SKEW_TOLERANCE_SECS,
            "{ahead}s ahead"
        );
    }
}

#[test]
fn only_a_real_authority_change_ends_an_attempt_before_egress() {
    assert!(admit_egress(GatewayKeyCheck::Current, Ok(OriginAuthority::Current)).is_ok());
    for (key, origin, class, retry_allowed) in [
        (
            GatewayKeyCheck::Revoked,
            Ok(OriginAuthority::Current),
            "destination_authority_changed",
            false,
        ),
        (
            GatewayKeyCheck::Unavailable,
            Ok(OriginAuthority::Current),
            "destination_authority_unavailable",
            true,
        ),
        (
            GatewayKeyCheck::Current,
            Ok(OriginAuthority::Changed),
            "origin_authority_changed",
            false,
        ),
        (
            GatewayKeyCheck::Current,
            Ok(OriginAuthority::Unavailable),
            "origin_authority_unavailable",
            true,
        ),
        (
            GatewayKeyCheck::Current,
            Err(String::from("global store is unavailable")),
            "origin_authority_unavailable",
            true,
        ),
    ] {
        assert!(
            matches!(
                admit_egress(key, origin),
                Err(RemoteSubmissionResult::Unknown {
                    error_class,
                    retry_allowed: retry
                }) if error_class == class && retry == retry_allowed
            ),
            "{class}"
        );
    }
    // An unreadable store is not an authority verdict.
    assert_eq!(
        classify_attempt_check(Err(memcore::MemoryError::InvalidArg(
            "claim changed".into()
        ))),
        OriginAuthority::Changed
    );
    assert_eq!(
        classify_attempt_check(Err(memcore::MemoryError::Io(std::io::Error::other(
            "temporary I/O failure"
        )))),
        OriginAuthority::Unavailable
    );
    assert_eq!(
        classify_key_check(Err(String::from("store is unavailable"))),
        GatewayKeyCheck::Unavailable
    );
    assert_eq!(classify_key_check(Ok(true)), GatewayKeyCheck::Revoked);
    assert_eq!(classify_key_check(Ok(false)), GatewayKeyCheck::Current);
}

#[test]
fn validate_task_snapshot_accepts_lifecycle_and_rejects_forged_or_contradictory_snapshots() {
    let now = jiff::Timestamp::now();
    let t1 = (now - jiff::SignedDuration::from_secs(10)).to_string();
    let t2 = (now - jiff::SignedDuration::from_secs(9)).to_string();
    let t3 = (now - jiff::SignedDuration::from_secs(8)).to_string();
    let t4 = (now - jiff::SignedDuration::from_secs(7)).to_string();
    let t5 = (now - jiff::SignedDuration::from_secs(6)).to_string();
    let (submission, base_rejected) = fixture();

    let mut admitted_task = base_rejected.clone();
    admitted_task["status"] = json!({"state": "TASK_STATE_SUBMITTED", "timestamp": t2});
    admitted_task["metadata"][PROVENANCE]["local_claim"] =
        json!({"claim_id": "rclaim-1", "transition_version": 0});
    admitted_task["metadata"][PROVENANCE]["local_run_id"] = json!("rrun-1");
    admitted_task["metadata"][PROVENANCE]["receipts"] = json!([
        {"receipt_id":"r-1","revision":1,"fact":"received","occurred_at":t1,"actor":"destination","admission_ref":"gw-1","detail":null},
        {"receipt_id":"r-2","revision":2,"fact":"admitted","occurred_at":t2,"actor":"destination","admission_ref":"gw-1","detail":{
            "local_run_id":"rrun-1","local_claim_id":"rclaim-1","local_claim_version":0,
            "adapter_kind":"native_review","policy_profile":"codex_sol_review",
            "resource_slot":"slot-1","lease_expires_at":t5
        }}
    ]);
    assert_eq!(
        validate_task_snapshot(&admitted_task, &submission),
        Some("submitted")
    );

    let mut cancel_before_start = admitted_task.clone();
    cancel_before_start["status"] = json!({"state": "TASK_STATE_SUBMITTED", "timestamp": t3});
    cancel_before_start["metadata"][PROVENANCE]["revision"] = json!(3);
    cancel_before_start["metadata"][PROVENANCE]["receipts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"receipt_id":"r-3","revision":3,"fact":"cancel_requested","occurred_at":t3,"actor":"destination","admission_ref":"gw-1","detail":{"local_run_id":"rrun-1","requested_by_identity_id":"origin"}}));
    assert_eq!(
        validate_task_snapshot(&cancel_before_start, &submission),
        Some("submitted")
    );
    cancel_before_start["status"]["state"] = json!("TASK_STATE_CANCELED");
    assert_eq!(
        validate_task_snapshot(&cancel_before_start, &submission),
        None
    );

    // Completed without Started must fail.
    let mut forged_completed = admitted_task.clone();
    forged_completed["status"] = json!({"state": "TASK_STATE_COMPLETED", "timestamp": t3});
    forged_completed["metadata"][PROVENANCE]["revision"] = json!(3);
    forged_completed["metadata"][PROVENANCE]["receipts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"receipt_id":"r-3","revision":3,"fact":"completed","occurred_at":t3,"actor":"destination","admission_ref":"gw-1","detail":{"local_run_id":"rrun-1","outcome":"completed"}}));
    assert_eq!(validate_task_snapshot(&forged_completed, &submission), None);

    // Valid Started -> Completed -> Cleanup preserves TASK_STATE_COMPLETED & terminal timestamp.
    let mut completed = admitted_task;
    completed["status"] = json!({"state": "TASK_STATE_COMPLETED", "timestamp": t4});
    completed["metadata"][PROVENANCE]["revision"] = json!(5);
    completed["metadata"][PROVENANCE]["receipts"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({"receipt_id":"r-3","revision":3,"fact":"started","occurred_at":t3,"actor":"destination","admission_ref":"gw-1","detail":{"local_run_id":"rrun-1","adapter_kind":"native_review"}}),
            json!({"receipt_id":"r-4","revision":4,"fact":"completed","occurred_at":t4,"actor":"destination","admission_ref":"gw-1","detail":{"local_run_id":"rrun-1","outcome":"completed"}}),
            json!({"receipt_id":"r-5","revision":5,"fact":"cleanup","occurred_at":t5,"actor":"destination","admission_ref":"gw-1","detail":{"local_run_id":"rrun-1","cleanup_state":"cleaned","cleaned_at":t5}}),
        ]);
    assert_eq!(
        validate_task_snapshot(&completed, &submission),
        Some("completed")
    );

    // Cleanup pointing status.timestamp to t5 instead of terminal t4 fails.
    completed["status"]["timestamp"] = json!(t5);
    assert_eq!(validate_task_snapshot(&completed, &submission), None);

    // Mismatched adapter_kind on started receipt must fail.
    let mut bad_adapter = completed.clone();
    bad_adapter["status"]["timestamp"] = json!(t4);
    bad_adapter["metadata"][PROVENANCE]["receipts"][2]["detail"]["adapter_kind"] =
        json!("dgx2_verify_pr");
    assert_eq!(validate_task_snapshot(&bad_adapter, &submission), None);

    // Started after lease_expires_at must fail.
    let mut late_start = completed.clone();
    late_start["status"]["timestamp"] = json!(t4);
    late_start["metadata"][PROVENANCE]["receipts"][1]["detail"]["lease_expires_at"] = json!(t2);
    assert_eq!(validate_task_snapshot(&late_start, &submission), None);
}
