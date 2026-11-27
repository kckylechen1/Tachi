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
