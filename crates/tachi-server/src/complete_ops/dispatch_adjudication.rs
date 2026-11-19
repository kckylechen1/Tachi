//! S1 (`tachi_complete`) and S2 (`tachi_task(action="adjudicate")`) dispatch
//! adjudication write paths (#1035, #2012 Slice 2).
//!
//! Extracted from `dispatch_outcome.rs` so that module stays bounded while
//! adding server-side provenance receipts and explicit `prior_event_key`
//! correction entry.

use serde_json::{json, Value};

use crate::tool_params::TachiCompleteParams;
use crate::MemoryServer;

/// Record the leader's terminal adjudication linked to the `dispatch_outcomes`
/// row written by [`super::dispatch_outcome::record_complete_outcome`] (#1035).
///
/// Rejects `prior_event_key` (corrections must go through the S2 post-hoc entry
/// so S1 same-key conflicts never silently auto-convert into corrections).
/// Constructs the `AdjudicationReceipt` strictly from server-held session
/// admission facts before opening any store lock.
pub(crate) fn record_complete_adjudication(
    server: &MemoryServer,
    params: &TachiCompleteParams,
    dispatch_outcome_status: &Value,
) -> Value {
    let Some(adjudication) = params.adjudication.as_ref() else {
        return json!("skipped (no adjudication)");
    };

    let recorded = dispatch_outcome_status
        .get("recorded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !recorded {
        return json!({
            "recorded": false,
            "reason": "dispatch outcome was not recorded; adjudication skipped",
        });
    }
    let outcome_id = dispatch_outcome_status
        .get("outcome_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if outcome_id.is_empty() {
        return json!({
            "recorded": false,
            "reason": "outcome_id missing from dispatch_outcome status; adjudication skipped",
        });
    }

    if adjudication.prior_event_key.is_some() {
        return json!({
            "recorded": false,
            "outcome_id": outcome_id,
            "error": "prior_event_key is only supported on post-hoc tachi_task(action='adjudicate'); tachi_complete cannot submit corrections",
        });
    }

    if let Err(error) = adjudication.validate_exactly_one() {
        return json!({
            "recorded": false,
            "outcome_id": outcome_id,
            "error": error,
        });
    }

    let signatures = match resolve_adjudication_signatures(&params.signatures) {
        Ok(sigs) => sigs,
        Err(error) => {
            return json!({
                "recorded": false,
                "outcome_id": outcome_id,
                "error": error,
            });
        }
    };

    // Server-held provenance receipt built from connection admission facts
    // outside any store write closure.
    let receipt =
        crate::provenance::server_adjudication_receipt(server, &adjudication.adjudicator, None);

    // Display-only default when omitted: `evidence_ref == outcome_id` is a
    // self-witness and never contributes to authority or independence.
    let evidence_ref = adjudication
        .evidence_ref
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&outcome_id)
        .to_string();
    let event_key = format!("complete:{outcome_id}");
    let new_adjudication = memcore::NewDispatchAdjudication {
        adjudication_id: uuid::Uuid::new_v4().to_string(),
        outcome_id: outcome_id.clone(),
        event_key,
        verdict: adjudication.verdict.clone(),
        not_required_reason: adjudication.not_required_reason.clone(),
        actor: adjudication.adjudicator.clone(),
        evidence_ref,
        signatures,
        adjudication_receipt: receipt,
    };

    let (scope, _) = server.resolve_write_scope(params.scope.as_deref().unwrap_or(""));
    write_adjudication_event(
        server,
        params.project.as_deref(),
        scope,
        &outcome_id,
        new_adjudication,
    )
}

fn resolve_adjudication_signatures(
    signatures: &[crate::tool_params::SignatureRecordParams],
) -> Result<Vec<memcore::DispatchAdjudicationSignature>, String> {
    let mut resolved = Vec::with_capacity(signatures.len());
    for sig in signatures {
        match tachi_dispatch::resolve_signature_id(&sig.signature) {
            Some(canonical) => resolved.push(memcore::DispatchAdjudicationSignature {
                signature_id: canonical.to_string(),
                evidence_ref: sig.evidence_ref.clone(),
                resolved: sig.resolved,
            }),
            None => {
                return Err(format!(
                    "unknown signature id '{}' in adjudication signatures; \
                     each id must resolve via the error-signature taxonomy",
                    sig.signature
                ));
            }
        }
    }
    Ok(resolved)
}

fn write_adjudication_event(
    server: &MemoryServer,
    project: Option<&str>,
    scope: crate::DbScope,
    outcome_id: &str,
    new_adjudication: memcore::NewDispatchAdjudication,
) -> Value {
    let write_result = if let Some(project) = project.filter(|s| !s.trim().is_empty()) {
        server.with_named_project_store(project, |store| {
            memcore::append_dispatch_adjudication(store.connection(), &new_adjudication)
                .map_err(|e| e.to_string())
        })
    } else {
        server.with_store_for_scope(scope, |store| {
            memcore::append_dispatch_adjudication(store.connection(), &new_adjudication)
                .map_err(|e| e.to_string())
        })
    };

    match write_result {
        Ok(row) => json!({
            "recorded": true,
            "adjudication_id": row.adjudication_id,
            "outcome_id": row.outcome_id,
            "event_key": row.event_key,
            "verdict": row.verdict,
            "not_required_reason": row.not_required_reason,
            "signatures": row.signatures.len(),
            "scope": scope.as_str(),
            "adjudication_attribution_basis": row.adjudication_attribution_basis,
            "evidence_ref_self_witness": row.evidence_ref == row.outcome_id,
        }),
        Err(error) => {
            tracing::warn!(
                error = %error,
                outcome_id = %outcome_id,
                "failed to persist dispatch adjudication row"
            );
            json!({
                "recorded": false,
                "outcome_id": outcome_id,
                "error": error,
            })
        }
    }
}

/// Post-hoc leader adjudication for an ALREADY-RECORDED dispatch outcome
/// (`tachi_task(action="adjudicate")`, #1035 FIX-3, #2012 Slice 2).
///
/// When `adjudication.prior_event_key` is present, validates the prior event
/// exists on the same outcome and appends via
/// [`memcore::append_dispatch_adjudication_correction`] (generating
/// `correction:{outcome_id}:{prior_event_key}`).
pub(crate) fn record_posthoc_adjudication(
    server: &MemoryServer,
    outcome_id: Option<&str>,
    dispatch_id: Option<&str>,
    adjudication: &crate::tool_params::AdjudicationParams,
    signatures: &[crate::tool_params::SignatureRecordParams],
    project: Option<&str>,
    scope: Option<&str>,
) -> Value {
    if let Err(error) = adjudication.validate_exactly_one() {
        return json!({
            "recorded": false,
            "error": error,
        });
    }
    let resolved_signatures = match resolve_adjudication_signatures(signatures) {
        Ok(sigs) => sigs,
        Err(error) => {
            return json!({
                "recorded": false,
                "error": error,
            });
        }
    };

    let direct_outcome_id = outcome_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let dispatch_id = dispatch_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if direct_outcome_id.is_none() && dispatch_id.is_none() {
        return json!({
            "recorded": false,
            "error": "adjudicate requires either outcome_id or dispatch_id",
        });
    }

    // Build the server-held provenance receipt outside any store closure.
    let receipt =
        crate::provenance::server_adjudication_receipt(server, &adjudication.adjudicator, None);

    let write_fn = |conn: &rusqlite::Connection| -> Result<memcore::DispatchAdjudication, String> {
        let resolved_outcome_id = match direct_outcome_id.as_deref() {
            Some(id) => id.to_string(),
            None => {
                let did = dispatch_id.as_deref().expect("checked non-empty above");
                let ids =
                    memcore::list_outcome_ids_for_dispatch(conn, did).map_err(|e| e.to_string())?;
                match ids.len() {
                    0 => {
                        return Err(format!(
                            "no dispatch_outcomes row found for dispatch_id '{did}'; \
                             complete the dispatch first or pass outcome_id directly"
                        ));
                    }
                    1 => ids[0].clone(),
                    _ => {
                        return Err(format!(
                            "dispatch_id '{did}' resolves to {} outcome rows ({}); \
                             pass outcome_id directly to disambiguate",
                            ids.len(),
                            ids.join(", ")
                        ));
                    }
                }
            }
        };

        let evidence_ref = adjudication
            .evidence_ref
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&resolved_outcome_id)
            .to_string();

        if let Some(prior_event_key) = adjudication.prior_event_key.as_deref() {
            let new_correction = memcore::NewDispatchAdjudication {
                adjudication_id: uuid::Uuid::new_v4().to_string(),
                outcome_id: resolved_outcome_id,
                event_key: String::new(),
                verdict: adjudication.verdict.clone(),
                not_required_reason: adjudication.not_required_reason.clone(),
                actor: adjudication.adjudicator.clone(),
                evidence_ref,
                signatures: resolved_signatures.clone(),
                adjudication_receipt: receipt.clone(),
            };
            return memcore::append_dispatch_adjudication_correction(
                conn,
                prior_event_key.trim(),
                &new_correction,
            )
            .map_err(|e| e.to_string());
        }

        let event_key = format!(
            "adjudicate:{}",
            crate::utils::stable_hash(&posthoc_payload_repr(
                &resolved_outcome_id,
                adjudication.verdict.as_deref(),
                adjudication.not_required_reason.as_deref(),
                &adjudication.adjudicator,
                &evidence_ref,
                &resolved_signatures,
            ))
        );
        let new_adjudication = memcore::NewDispatchAdjudication {
            adjudication_id: uuid::Uuid::new_v4().to_string(),
            outcome_id: resolved_outcome_id,
            event_key,
            verdict: adjudication.verdict.clone(),
            not_required_reason: adjudication.not_required_reason.clone(),
            actor: adjudication.adjudicator.clone(),
            evidence_ref,
            signatures: resolved_signatures.clone(),
            adjudication_receipt: receipt.clone(),
        };
        memcore::append_dispatch_adjudication(conn, &new_adjudication).map_err(|e| e.to_string())
    };

    let (write_scope, _) = server.resolve_write_scope(scope.unwrap_or(""));
    let write_result = if let Some(project) = project.map(str::trim).filter(|s| !s.is_empty()) {
        server.with_named_project_store(project, |store| write_fn(store.connection()))
    } else {
        server.with_store_for_scope(write_scope, |store| write_fn(store.connection()))
    };

    match write_result {
        Ok(row) => json!({
            "recorded": true,
            "adjudication_id": row.adjudication_id,
            "outcome_id": row.outcome_id,
            "event_key": row.event_key,
            "verdict": row.verdict,
            "not_required_reason": row.not_required_reason,
            "signatures": row.signatures.len(),
            "scope": write_scope.as_str(),
            "adjudication_attribution_basis": row.adjudication_attribution_basis,
            "evidence_ref_self_witness": row.evidence_ref == row.outcome_id,
        }),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to persist post-hoc dispatch adjudication row"
            );
            json!({
                "recorded": false,
                "error": error,
            })
        }
    }
}

fn posthoc_payload_repr(
    outcome_id: &str,
    verdict: Option<&str>,
    not_required_reason: Option<&str>,
    adjudicator: &str,
    evidence_ref: &str,
    signatures: &[memcore::DispatchAdjudicationSignature],
) -> String {
    let mut sorted: Vec<&memcore::DispatchAdjudicationSignature> = signatures.iter().collect();
    sorted.sort_by_key(|s| s.signature_id.as_str());
    let sig_triples: Vec<Value> = sorted
        .iter()
        .map(|s| json!([s.signature_id, s.evidence_ref, s.resolved]))
        .collect();
    serde_json::to_string(&json!([
        outcome_id,
        verdict,
        not_required_reason,
        adjudicator,
        evidence_ref,
        sig_triples,
    ]))
    .expect("payload representation must serialize")
}

#[cfg(test)]
mod provenance_tests {
    use super::*;
    use crate::tool_params::AdjudicationParams;
    use crate::verified_admission::{
        admit_verified_agent_connection, revoke_verified_admission, TrustedAdmissionVerifier,
        TrustedRevocationVerifier, VerifiedAdmissionEvidence, VerifiedAdmissionRequest,
        VerifiedEvidenceRef, VerifiedEvidenceState, VerifiedRevocationEvidence,
    };

    struct TestAdmissionVerifier {
        evidence: VerifiedAdmissionEvidence,
    }

    impl TrustedAdmissionVerifier for TestAdmissionVerifier {
        fn verify(
            &self,
            _: &VerifiedAdmissionRequest,
        ) -> Result<VerifiedAdmissionEvidence, String> {
            Ok(self.evidence.clone())
        }
    }

    struct TestRevocationVerifier {
        evidence: VerifiedRevocationEvidence,
    }

    impl TrustedRevocationVerifier for TestRevocationVerifier {
        fn verify_revocation(&self, _: &str) -> Result<VerifiedRevocationEvidence, String> {
            Ok(self.evidence.clone())
        }
    }

    fn test_server() -> (MemoryServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = MemoryServer::new(dir.path().join("global.db"), None).expect("server");
        (server, dir)
    }

    fn base_complete_params() -> TachiCompleteParams {
        TachiCompleteParams {
            task_id: None,
            task: "S1/S2 provenance test".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: None,
            profile: None,
            risk: None,
            duration_ms: None,
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: None,
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            eval_run_ids: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: None,
            scope: Some("global".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
        }
    }

    fn seed_outcome(server: &MemoryServer, dispatch_id: &str) -> String {
        let mut params = base_complete_params();
        params.dispatch_id = Some(dispatch_id.to_string());
        let outcome = super::super::dispatch_outcome::record_complete_outcome(
            server,
            &params,
            "eval-prov",
            "success",
            "completed",
            None,
            true,
            true,
            &[],
        );
        assert_eq!(outcome["recorded"], json!(true));
        outcome["outcome_id"].as_str().unwrap().to_string()
    }

    fn admit_verified(server: &MemoryServer, agent_id: &str) -> String {
        let now = chrono::Utc::now();
        let req = VerifiedAdmissionRequest {
            agent_identity_id: agent_id.to_string(),
            connection_id: format!("conn-verified-{agent_id}"),
            expected_issuer_id: "issuer.tachi.test".to_string(),
            expected_verification_method: memcore::VERIFIED_ADMISSION_METHOD.to_string(),
            expected_verification_version: memcore::VERIFIED_ADMISSION_VERSION.to_string(),
            trust_domain: "tachi.local".to_string(),
            verification_scope: memcore::VERIFIED_ADMISSION_SCOPE.to_string(),
            idempotency_key: format!("idem-{agent_id}"),
        };
        let evidence = VerifiedAdmissionEvidence {
            agent_identity_id: req.agent_identity_id.clone(),
            connection_id: req.connection_id.clone(),
            issuer_id: req.expected_issuer_id.clone(),
            verification_method: req.expected_verification_method.clone(),
            verification_version: req.expected_verification_version.clone(),
            trust_domain: req.trust_domain.clone(),
            verification_scope: req.verification_scope.clone(),
            evidence_digest: "a".repeat(64),
            evidence_ref: VerifiedEvidenceRef::attestation("oidc", "test-1").unwrap(),
            issued_at: now - chrono::Duration::seconds(5),
            expires_at: now + chrono::Duration::minutes(10),
            nonce: format!("nonce-{agent_id}"),
            state: VerifiedEvidenceState::Active,
        };
        admit_verified_agent_connection(server, &req, &TestAdmissionVerifier { evidence })
            .expect("verified admission");
        server
            .verified_work_claim_connection()
            .expect("verified context")
            .admission_id
    }

    #[test]
    fn s1_admission_channels_match_and_mismatch_degrade_to_unknown() {
        let (server, _dir) = test_server();
        let out_unauth = seed_outcome(&server, "disp-s1-unauth");
        let mut p_unauth = base_complete_params();
        p_unauth.adjudication = Some(AdjudicationParams {
            verdict: Some("accepted".to_string()),
            adjudicator: "alice".to_string(),
            ..Default::default()
        });
        let r_unauth = record_complete_adjudication(
            &server,
            &p_unauth,
            &json!({"recorded": true, "outcome_id": out_unauth}),
        );
        assert_eq!(r_unauth["adjudication_attribution_basis"], "unknown");
        assert_eq!(r_unauth["evidence_ref_self_witness"], true);

        // Self-asserted local matching principal -> planned_unconfirmed.
        crate::claims_ops::admit_agent_connection(&server, Some("alice".into()), true).unwrap();
        let out_local = seed_outcome(&server, "disp-s1-local");
        let r_local = record_complete_adjudication(
            &server,
            &p_unauth,
            &json!({"recorded": true, "outcome_id": out_local}),
        );
        assert_eq!(
            r_local["adjudication_attribution_basis"],
            "planned_unconfirmed"
        );

        // Self-asserted local mismatching adjudicator -> unknown.
        let mut p_local_mismatch = p_unauth.clone();
        p_local_mismatch.adjudication = Some(AdjudicationParams {
            verdict: Some("accepted".to_string()),
            adjudicator: "bob".to_string(),
            ..Default::default()
        });
        let out_local_mm = seed_outcome(&server, "disp-s1-local-mm");
        let r_local_mm = record_complete_adjudication(
            &server,
            &p_local_mismatch,
            &json!({"recorded": true, "outcome_id": out_local_mm}),
        );
        assert_eq!(r_local_mm["adjudication_attribution_basis"], "unknown");

        // Verified admission matching principal -> acknowledged_overlay.
        let admission_id = admit_verified(&server, "alice");
        let out_verified = seed_outcome(&server, "disp-s1-verified");
        let r_verified = record_complete_adjudication(
            &server,
            &p_unauth,
            &json!({"recorded": true, "outcome_id": out_verified}),
        );
        assert_eq!(
            r_verified["adjudication_attribution_basis"],
            "acknowledged_overlay"
        );

        // Verified admission mismatching principal -> unknown.
        let out_verified_mm = seed_outcome(&server, "disp-s1-verified-mm");
        let r_verified_mm = record_complete_adjudication(
            &server,
            &p_local_mismatch,
            &json!({"recorded": true, "outcome_id": out_verified_mm}),
        );
        assert_eq!(r_verified_mm["adjudication_attribution_basis"], "unknown");

        // Revoked verified admission -> unknown even when principal matches.
        revoke_verified_admission(
            &server,
            &admission_id,
            &TestRevocationVerifier {
                evidence: VerifiedRevocationEvidence {
                    admission_id: admission_id.clone(),
                    issuer_id: "issuer.tachi.test".to_string(),
                    evidence_digest: "b".repeat(64),
                    evidence_ref: VerifiedEvidenceRef::attestation("oidc", "revoke-1").unwrap(),
                    nonce: "revoke-nonce-1".to_string(),
                    revoked_at: chrono::Utc::now(),
                },
            },
        )
        .expect("revoke");
        let out_revoked = seed_outcome(&server, "disp-s1-revoked");
        let r_revoked = record_complete_adjudication(
            &server,
            &p_unauth,
            &json!({"recorded": true, "outcome_id": out_revoked}),
        );
        assert_eq!(r_revoked["adjudication_attribution_basis"], "unknown");
    }

    #[test]
    fn s1_rejects_prior_event_key_and_s2_correction_contract_holds() {
        let (server, _dir) = test_server();
        let outcome_id = seed_outcome(&server, "disp-s2-corr");
        let other_outcome_id = seed_outcome(&server, "disp-s2-other");

        // S1 must reject prior_event_key outright.
        let mut s1_corr = base_complete_params();
        s1_corr.adjudication = Some(AdjudicationParams {
            verdict: Some("accepted".to_string()),
            adjudicator: "alice".to_string(),
            prior_event_key: Some("complete:anything".to_string()),
            ..Default::default()
        });
        let s1_rejected = record_complete_adjudication(
            &server,
            &s1_corr,
            &json!({"recorded": true, "outcome_id": outcome_id}),
        );
        assert_eq!(s1_rejected["recorded"], json!(false));
        assert!(s1_rejected["error"]
            .as_str()
            .unwrap()
            .contains("prior_event_key"));

        // Seed initial S1 event under unauthenticated caller -> unknown.
        let mut s1_initial = base_complete_params();
        s1_initial.adjudication = Some(AdjudicationParams {
            verdict: Some("accepted".to_string()),
            adjudicator: "alice".to_string(),
            evidence_ref: Some("review://1".to_string()),
            ..Default::default()
        });
        let initial = record_complete_adjudication(
            &server,
            &s1_initial,
            &json!({"recorded": true, "outcome_id": outcome_id}),
        );
        assert_eq!(initial["recorded"], json!(true));
        let initial_key = initial["event_key"].as_str().unwrap().to_string();
        assert_eq!(initial["adjudication_attribution_basis"], "unknown");

        // S1 same-key conflict (even proof-only upgrade after verified admission) must NOT auto-convert.
        admit_verified(&server, "alice");
        let conflict = record_complete_adjudication(
            &server,
            &s1_initial,
            &json!({"recorded": true, "outcome_id": outcome_id}),
        );
        assert_eq!(conflict["recorded"], json!(false));
        assert!(conflict["error"]
            .as_str()
            .unwrap()
            .contains("payload mismatch"));

        // S2 explicit correction citing prior_event_key succeeds with verified admission -> acknowledged_overlay.
        let corr_adj = AdjudicationParams {
            verdict: Some("accepted".to_string()),
            adjudicator: "alice".to_string(),
            evidence_ref: Some("review://1".to_string()),
            prior_event_key: Some(initial_key.clone()),
            ..Default::default()
        };
        let corr_1 = record_posthoc_adjudication(
            &server,
            Some(&outcome_id),
            None,
            &corr_adj,
            &[],
            None,
            Some("global"),
        );
        assert_eq!(corr_1["recorded"], json!(true));
        let corr_1_key = corr_1["event_key"].as_str().unwrap().to_string();
        assert_eq!(corr_1_key, format!("correction:{outcome_id}:{initial_key}"));
        assert_eq!(
            corr_1["adjudication_attribution_basis"],
            "acknowledged_overlay"
        );

        // Identical correction replay returns the original correction row.
        let corr_replay = record_posthoc_adjudication(
            &server,
            Some(&outcome_id),
            None,
            &corr_adj,
            &[],
            None,
            Some("global"),
        );
        assert_eq!(corr_replay["recorded"], json!(true));
        assert_eq!(corr_replay["adjudication_id"], corr_1["adjudication_id"]);

        // Conflicting content under same prior_event_key is rejected.
        let corr_conflict = record_posthoc_adjudication(
            &server,
            Some(&outcome_id),
            None,
            &AdjudicationParams {
                verdict: Some("rejected".to_string()),
                ..corr_adj.clone()
            },
            &[],
            None,
            Some("global"),
        );
        assert_eq!(corr_conflict["recorded"], json!(false));

        // Chained correction citing corr_1_key succeeds.
        let corr_chained = record_posthoc_adjudication(
            &server,
            Some(&outcome_id),
            None,
            &AdjudicationParams {
                verdict: Some("rejected".to_string()),
                adjudicator: "alice".to_string(),
                evidence_ref: Some("review://2".to_string()),
                prior_event_key: Some(corr_1_key.clone()),
                ..Default::default()
            },
            &[],
            None,
            Some("global"),
        );
        assert_eq!(corr_chained["recorded"], json!(true));
        assert_eq!(
            corr_chained["event_key"],
            format!("correction:{outcome_id}:{corr_1_key}")
        );

        // Cross-outcome prior_event_key is rejected.
        let wrong_outcome = record_posthoc_adjudication(
            &server,
            Some(&other_outcome_id),
            None,
            &corr_adj,
            &[],
            None,
            Some("global"),
        );
        assert_eq!(wrong_outcome["recorded"], json!(false));
        assert!(wrong_outcome["error"]
            .as_str()
            .unwrap()
            .contains("different outcome"));
    }
}
