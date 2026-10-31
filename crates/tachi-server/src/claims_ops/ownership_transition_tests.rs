use super::*;
use crate::tool_params::TachiTaskParams;
use serde_json::json;

fn local(server: &MemoryServer, project: Option<&str>, identity: &str) {
    server.set_session_identity(None, project.map(str::to_string), None);
    admit_agent_connection(server, Some(identity.into()), true).unwrap();
}

fn request(action: &str, claim_id: &str, version: i64) -> TachiTaskParams {
    serde_json::from_value(
        json!({"action":action,"claim_id":claim_id,"transition_version":version,
        "claim_role":"executor","claim_mode":"writable","claim_scope":["src/**"],
        "worktree_path":format!("/tmp/ownership-{}", uuid::Uuid::new_v4()),
        "expected_head":"head","lease_expires_at":"2035-01-01T00:00:00Z"}),
    )
    .unwrap()
}

fn create(server: &MemoryServer) -> String {
    handle_task_claim(server, &request("claim", "unused", 0)).unwrap()["claim_id"]
        .as_str()
        .unwrap()
        .into()
}

fn invoke(
    server: &MemoryServer,
    action: &str,
    id: &str,
    version: i64,
) -> Result<serde_json::Value, String> {
    let p = request(action, id, version);
    match action {
        "heartbeat" => handle_task_heartbeat(server, &p),
        "release" => handle_task_release(server, &p),
        "handoff" => handle_task_handoff(server, &p),
        _ => unreachable!(),
    }
}

fn row(server: &MemoryServer, id: &str) -> memcore::SessionClaim {
    server
        .with_global_store_read(|store| {
            memcore::get_claim(store.connection(), id).map_err(|e| e.to_string())
        })
        .unwrap()
        .unwrap()
}

fn verified(server: &MemoryServer) {
    use crate::verified_admission::*;
    struct Verifier;
    impl TrustedAdmissionVerifier for Verifier {
        fn verify(
            &self,
            r: &VerifiedAdmissionRequest,
        ) -> Result<VerifiedAdmissionEvidence, String> {
            let now = chrono::Utc::now();
            Ok(VerifiedAdmissionEvidence {
                agent_identity_id: r.agent_identity_id.clone(),
                connection_id: r.connection_id.clone(),
                issuer_id: r.expected_issuer_id.clone(),
                verification_method: r.expected_verification_method.clone(),
                verification_version: r.expected_verification_version.clone(),
                trust_domain: r.trust_domain.clone(),
                verification_scope: r.verification_scope.clone(),
                evidence_digest: "ab".repeat(32),
                evidence_ref: VerifiedEvidenceRef::attestation("test", "ownership-transition")
                    .unwrap(),
                issued_at: now - chrono::Duration::seconds(1),
                expires_at: now + chrono::Duration::minutes(9),
                nonce: "ownership-transition-nonce".into(),
                state: VerifiedEvidenceState::Active,
            })
        }
    }
    server.set_session_identity(None, None, None);
    admit_verified_agent_connection(
        server,
        &VerifiedAdmissionRequest {
            agent_identity_id: "remote.actor".into(),
            connection_id: "remote.connection".into(),
            expected_issuer_id: "test.issuer".into(),
            expected_verification_method: memcore::VERIFIED_ADMISSION_METHOD.into(),
            expected_verification_version: memcore::VERIFIED_ADMISSION_VERSION.into(),
            trust_domain: "test:ownership".into(),
            verification_scope: memcore::VERIFIED_ADMISSION_SCOPE.into(),
            idempotency_key: "ownership-transition".into(),
        },
        &Verifier,
    )
    .unwrap();
}

#[test]
fn real_transition_handlers_mask_canonical_and_preserve_legacy_diagnostics() {
    let server = crate::tests::make_server();
    local(&server, None, "owner");
    let legacy_foreign = create(&server);
    local(&server, Some("alpha"), "owner");
    let canonical = create(&server);
    let original = row(&server, &canonical);
    for context in ["bound", "unbound", "verified"] {
        if context == "verified" {
            verified(&server);
        } else {
            local(&server, None, "other");
        }
        let legacy_own = create(&server);
        if context == "bound" {
            server.set_session_identity(None, Some("alpha".into()), None);
        }
        for action in ["heartbeat", "release", "handoff"] {
            let absent = invoke(&server, action, "claim-absent", 0)
                .unwrap_err()
                .replace("claim-absent", "target");
            let hidden = invoke(&server, action, &canonical, 0)
                .unwrap_err()
                .replace(&canonical, "target");
            assert_eq!(hidden, absent, "{context}/{action}");
            let before = row(&server, &legacy_own);
            assert!(invoke(&server, action, &legacy_own, 99)
                .unwrap_err()
                .contains("expected 99"));
            assert_eq!(row(&server, &legacy_own), before);
            assert!(invoke(&server, action, &legacy_foreign, 0)
                .unwrap_err()
                .contains("holder"));
        }
        for (scope, path) in [
            (vec![], Some("/tmp/handoff".to_string())),
            (vec!["  ".to_string()], Some("/tmp/handoff".to_string())),
            (vec!["src/**".to_string()], None),
        ] {
            let mut hidden = request("handoff", &canonical, 0);
            hidden.claim_scope = scope;
            hidden.worktree_path = path;
            let mut missing = hidden.clone();
            missing.claim_id = Some("claim-absent".into());
            assert_eq!(
                handle_task_handoff(&server, &hidden).unwrap_err(),
                handle_task_handoff(&server, &missing).unwrap_err(),
                "pure validation precedes target visibility"
            );
        }
        assert_eq!(row(&server, &canonical), original);
        assert!(invoke(&server, "heartbeat", &legacy_own, 0).is_ok());
        assert!(invoke(&server, "release", &legacy_own, 1).is_ok());
        assert!(invoke(&server, "heartbeat", &legacy_own, 2)
            .unwrap_err()
            .contains("released"));
    }
    local(&server, Some("beta"), "owner");
    assert!(invoke(&server, "release", &canonical, 0)
        .unwrap_err()
        .contains("Not found"));
    local(&server, Some("alpha"), "owner");
    assert!(invoke(&server, "heartbeat", &canonical, 99)
        .unwrap_err()
        .contains("expected 99"));
    assert!(invoke(&server, "heartbeat", &canonical, 0).is_ok());
}

#[test]
fn canonical_orphan_handoff_requires_explicit_project_qualification_and_rolls_back_errors() {
    let server = crate::tests::make_server();
    local(&server, Some("alpha"), "owner");
    let target = create(&server);
    local(&server, Some("alpha"), "successor");
    assert!(invoke(&server, "handoff", &target, 0)
        .unwrap_err()
        .contains("Not found"));
    let grant = create(&server);
    assert!(invoke(&server, "handoff", &target, 0)
        .unwrap_err()
        .contains("holder"));
    server.with_global_store(|store| {
        store.connection().execute("UPDATE session_claims SET heartbeat_at='2000-01-01T00:00:00Z' WHERE claim_id=?1", [&target]).map_err(|e|e.to_string())?;
        memcore::gc_session_claims(store.connection(), chrono::Utc::now(), 7, 30).map_err(|e|e.to_string())?;
        Ok(())
    }).unwrap();
    assert!(invoke(&server, "handoff", &target, 0).is_ok());
    let before = row(&server, &grant);
    let error = project_ownership::with_claim_transition(
        &server,
        &grant,
        |conn| -> Result<(), memcore::MemoryError> {
            conn.execute(
                "UPDATE session_claims SET transition_version=99 WHERE claim_id=?1",
                [&grant],
            )?;
            Err(memcore::MemoryError::WorkClaimConflict(
                "rollback sentinel".into(),
            ))
        },
    )
    .unwrap_err();
    assert!(error.contains("rollback sentinel"));
    assert_eq!(
        row(&server, &grant),
        before,
        "Err must reach the transaction rollback"
    );
}
