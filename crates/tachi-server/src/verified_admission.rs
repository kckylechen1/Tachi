//! Trusted server boundary for verified AgentIdentity admissions (#1938).
//!
//! Nothing in this module is public or model-facing. A future #1170 adapter
//! implements [`TrustedAdmissionVerifier`] inside `tachi-server`, validates its
//! provider evidence, and returns the bounded evidence type below. Public MCP
//! params, transport headers, environment variables, display names, and model
//! labels cannot construct either the verifier or its result.

// #1938 deliberately lands the typed server port before #1170 supplies its
// production provider adapter; the whole module becomes live through that adapter.
#![cfg_attr(not(test), allow(dead_code))]

use chrono::{DateTime, Duration, Utc};

use crate::server_state::MemoryServer;

const MAX_VERIFIED_EVIDENCE_AGE: Duration = Duration::minutes(5);
const MAX_VERIFIED_EVIDENCE_LIFETIME: Duration = Duration::minutes(15);

#[derive(Debug, Clone)]
pub(crate) struct VerifiedAdmissionRequest {
    pub agent_identity_id: String,
    pub connection_id: String,
    pub expected_issuer_id: String,
    pub trust_domain: String,
    pub verification_scope: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifiedEvidenceState {
    Active,
    Revoked,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedAdmissionEvidence {
    pub agent_identity_id: String,
    pub issuer_id: String,
    pub verification_method: String,
    pub verification_version: String,
    pub trust_domain: String,
    pub verification_scope: String,
    pub evidence_digest: String,
    pub evidence_ref: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    pub state: VerifiedEvidenceState,
}

/// Server-private verifier port. The untrusted assertion stays inside the
/// adapter; only bounded, secret-negative evidence crosses this interface.
pub(crate) trait TrustedAdmissionVerifier {
    fn verify(
        &self,
        request: &VerifiedAdmissionRequest,
    ) -> Result<VerifiedAdmissionEvidence, String>;
}

fn stable_receipt_ids(issuer_id: &str, idempotency_key: &str) -> (String, String) {
    let binding = format!("{issuer_id}\0{idempotency_key}");
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, binding.as_bytes());
    (
        format!("admission-verified-{id}"),
        format!("verified-admission-receipt-{id}"),
    )
}

fn validate_verified_evidence(
    request: &VerifiedAdmissionRequest,
    evidence: &VerifiedAdmissionEvidence,
    now: DateTime<Utc>,
) -> Result<(), String> {
    if !crate::session_identity::valid_agent_identity_assertion(&request.agent_identity_id)
        || request.connection_id.trim().is_empty()
        || request.idempotency_key.trim().is_empty()
    {
        return Err("verified admission request binding is invalid".to_string());
    }
    if evidence.state == VerifiedEvidenceState::Revoked {
        return Err("verified admission evidence is revoked".to_string());
    }
    if evidence.agent_identity_id != request.agent_identity_id {
        return Err("verified admission evidence binds the wrong AgentIdentity".to_string());
    }
    if evidence.issuer_id != request.expected_issuer_id {
        return Err("verified admission evidence comes from the wrong issuer".to_string());
    }
    if evidence.trust_domain != request.trust_domain {
        return Err("verified admission evidence binds the wrong trust domain".to_string());
    }
    if evidence.verification_scope != request.verification_scope {
        return Err("verified admission evidence binds the wrong scope".to_string());
    }
    let persisted_text = [
        request.agent_identity_id.as_str(),
        request.connection_id.as_str(),
        request.idempotency_key.as_str(),
        evidence.issuer_id.as_str(),
        evidence.verification_method.as_str(),
        evidence.verification_version.as_str(),
        evidence.trust_domain.as_str(),
        evidence.verification_scope.as_str(),
        evidence.evidence_ref.as_str(),
        evidence.nonce.as_str(),
    ];
    if persisted_text
        .iter()
        .any(|value| tachi_lesson_forge::contains_secret_like(value))
        || memcore::catalog::endpoint::endpoint_credential_leak(&evidence.evidence_ref).is_some()
    {
        return Err("verified admission evidence contains secret-like material".to_string());
    }
    if evidence.issued_at > now {
        return Err("verified admission evidence is not yet valid".to_string());
    }
    if now.signed_duration_since(evidence.issued_at) > MAX_VERIFIED_EVIDENCE_AGE {
        return Err("verified admission evidence is stale".to_string());
    }
    if evidence.expires_at <= now {
        return Err("verified admission evidence is expired".to_string());
    }
    if evidence.expires_at.signed_duration_since(evidence.issued_at)
        > MAX_VERIFIED_EVIDENCE_LIFETIME
    {
        return Err("verified admission evidence lifetime exceeds the bounded window".to_string());
    }
    Ok(())
}

/// Verify and persist one remote admission. This is intentionally crate-private
/// and has no handler, tool parameter, header, or environment-variable route.
/// Exact replay returns the original receipt; conflicts and invalid evidence
/// leave the prior append-only history untouched.
pub(crate) fn admit_verified_agent_connection(
    server: &MemoryServer,
    request: &VerifiedAdmissionRequest,
    verifier: &dyn TrustedAdmissionVerifier,
    now: DateTime<Utc>,
) -> Result<memcore::VerifiedAdmissionWriteOutcome, String> {
    let evidence = verifier.verify(request)?;
    validate_verified_evidence(request, &evidence, now)?;
    let (admission_id, receipt_id) =
        stable_receipt_ids(&evidence.issuer_id, &request.idempotency_key);
    let write = memcore::NewVerifiedAdmission {
        receipt_id,
        admission_id,
        agent_identity_id: evidence.agent_identity_id,
        connection_id: request.connection_id.clone(),
        issuer_id: evidence.issuer_id,
        verification_method: evidence.verification_method,
        verification_version: evidence.verification_version,
        trust_domain: evidence.trust_domain,
        verification_scope: evidence.verification_scope,
        evidence_digest: evidence.evidence_digest,
        evidence_ref: evidence.evidence_ref,
        evidence_issued_at: evidence.issued_at.to_rfc3339(),
        evidence_expires_at: evidence.expires_at.to_rfc3339(),
        nonce: evidence.nonce,
        idempotency_key: request.idempotency_key.clone(),
    };
    let outcome = server.with_global_store(|store| {
        memcore::record_verified_admission(store.connection_mut(), &write)
            .map_err(|error| error.to_string())
    })?;
    let receipt = outcome.receipt();
    server.set_work_claim_connection(
        Some(receipt.agent_identity_id.clone()),
        receipt.connection_id.clone(),
        "verified".to_string(),
    );
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::tests::make_server;

    const RAW_SECRET: &str = "raw-attestation-secret-must-not-persist";

    #[derive(Clone)]
    struct FakeVerifier {
        evidence: VerifiedAdmissionEvidence,
        raw_attestation: String,
    }

    impl TrustedAdmissionVerifier for FakeVerifier {
        fn verify(
            &self,
            _request: &VerifiedAdmissionRequest,
        ) -> Result<VerifiedAdmissionEvidence, String> {
            assert!(!self.raw_attestation.is_empty());
            Ok(self.evidence.clone())
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-19T12:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn request(key: &str) -> VerifiedAdmissionRequest {
        VerifiedAdmissionRequest {
            agent_identity_id: "agent.remote.alpha".into(),
            connection_id: "placement:runner-alpha:connection-7".into(),
            expected_issuer_id: "issuer.device-trust.alpha".into(),
            trust_domain: "workspace:kckylechen1/tachi".into(),
            verification_scope: "agent_identity:remote_admission".into(),
            idempotency_key: key.into(),
        }
    }

    fn evidence() -> VerifiedAdmissionEvidence {
        VerifiedAdmissionEvidence {
            agent_identity_id: "agent.remote.alpha".into(),
            issuer_id: "issuer.device-trust.alpha".into(),
            verification_method: "device-envelope".into(),
            verification_version: "v1".into(),
            trust_domain: "workspace:kckylechen1/tachi".into(),
            verification_scope: "agent_identity:remote_admission".into(),
            evidence_digest: format!("{:x}", Sha256::digest(RAW_SECRET.as_bytes())),
            evidence_ref: "attestation:device-envelope:alpha-7".into(),
            issued_at: now() - Duration::minutes(1),
            expires_at: now() + Duration::minutes(9),
            nonce: "nonce-alpha-7".into(),
            state: VerifiedEvidenceState::Active,
        }
    }

    fn verifier(evidence: VerifiedAdmissionEvidence) -> FakeVerifier {
        FakeVerifier {
            evidence,
            raw_attestation: RAW_SECRET.into(),
        }
    }

    fn verified_count(server: &MemoryServer) -> i64 {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM identity_admissions WHERE state='verified'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .unwrap()
    }

    #[test]
    fn trusted_verifier_writes_bound_secret_negative_receipt_and_exact_replay() {
        let server = make_server();
        let first = admit_verified_agent_connection(
            &server,
            &request("verify-alpha-7"),
            &verifier(evidence()),
            now(),
        )
        .expect("fresh trusted evidence must admit");
        assert!(matches!(
            first,
            memcore::VerifiedAdmissionWriteOutcome::Created(_)
        ));
        let receipt = first.receipt();
        assert_eq!(receipt.agent_identity_id, "agent.remote.alpha");
        assert_eq!(receipt.issuer_id, "issuer.device-trust.alpha");
        assert_eq!(receipt.trust_domain, "workspace:kckylechen1/tachi");
        assert_eq!(
            receipt.verification_scope,
            "agent_identity:remote_admission"
        );
        assert_eq!(receipt.current_state, "verified");

        let replay = admit_verified_agent_connection(
            &server,
            &request("verify-alpha-7"),
            &verifier(evidence()),
            now(),
        )
        .expect("same issuer/key/digest must replay");
        assert!(matches!(
            replay,
            memcore::VerifiedAdmissionWriteOutcome::Replayed(_)
        ));
        assert_eq!(replay.receipt(), receipt);
        assert_eq!(verified_count(&server), 1);

        server
            .with_global_store_read(|store| {
                let persisted: String = store
                    .connection()
                    .query_row(
                        "SELECT receipt_id || admission_id || agent_identity_id || connection_id || \
                                issuer_id || verification_method || verification_version || trust_domain || \
                                verification_scope || evidence_digest || evidence_ref || evidence_issued_at || \
                                evidence_expires_at || nonce || idempotency_key || request_digest || current_state \
                         FROM identity_admission_verification_receipts",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                assert!(!persisted.contains(RAW_SECRET));
                assert!(!persisted.contains("private-key"));
                assert!(!persisted.contains("credential"));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn same_idempotency_key_with_changed_digest_conflicts_without_second_row() {
        let server = make_server();
        admit_verified_agent_connection(
            &server,
            &request("verify-digest-conflict"),
            &verifier(evidence()),
            now(),
        )
        .unwrap();
        let mut changed = evidence();
        changed.evidence_digest = format!("{:x}", Sha256::digest(b"different-evidence"));
        let error = admit_verified_agent_connection(
            &server,
            &request("verify-digest-conflict"),
            &verifier(changed),
            now(),
        )
        .expect_err("same issuer/key with a different canonical digest must conflict");
        assert!(error.contains("idempotency conflict"), "{error}");
        assert_eq!(verified_count(&server), 1);
    }

    #[test]
    fn expired_wrong_domain_wrong_identity_stale_and_revoked_evidence_write_nothing() {
        let cases: Vec<(&str, VerifiedAdmissionEvidence, &str)> = vec![
            (
                "expired",
                VerifiedAdmissionEvidence {
                    expires_at: now() - Duration::seconds(1),
                    ..evidence()
                },
                "expired",
            ),
            (
                "wrong-domain",
                VerifiedAdmissionEvidence {
                    trust_domain: "workspace:attacker/repo".into(),
                    ..evidence()
                },
                "wrong trust domain",
            ),
            (
                "wrong-issuer",
                VerifiedAdmissionEvidence {
                    issuer_id: "issuer.attacker".into(),
                    ..evidence()
                },
                "wrong issuer",
            ),
            (
                "wrong-identity",
                VerifiedAdmissionEvidence {
                    agent_identity_id: "agent.remote.attacker".into(),
                    ..evidence()
                },
                "wrong AgentIdentity",
            ),
            (
                "wrong-scope",
                VerifiedAdmissionEvidence {
                    verification_scope: "agent_identity:display_only".into(),
                    ..evidence()
                },
                "wrong scope",
            ),
            (
                "stale",
                VerifiedAdmissionEvidence {
                    issued_at: now() - Duration::minutes(6),
                    expires_at: now() + Duration::minutes(1),
                    ..evidence()
                },
                "stale",
            ),
            (
                "revoked",
                VerifiedAdmissionEvidence {
                    state: VerifiedEvidenceState::Revoked,
                    ..evidence()
                },
                "revoked",
            ),
            (
                "secret-like-reference",
                VerifiedAdmissionEvidence {
                    evidence_ref: "https://principal@attestor.invalid/evidence/7".into(),
                    ..evidence()
                },
                "secret-like material",
            ),
        ];

        for (label, evidence, expected) in cases {
            let server = make_server();
            let error = admit_verified_agent_connection(
                &server,
                &request(&format!("reject-{label}")),
                &verifier(evidence),
                now(),
            )
            .expect_err(label);
            assert!(error.contains(expected), "{label}: {error}");
            assert_eq!(verified_count(&server), 0, "{label}");
        }
    }

    #[test]
    fn reused_nonce_and_copied_receipt_binding_cannot_mint_another_admission() {
        let server = make_server();
        admit_verified_agent_connection(
            &server,
            &request("verify-original"),
            &verifier(evidence()),
            now(),
        )
        .unwrap();

        let nonce_error = admit_verified_agent_connection(
            &server,
            &request("verify-reused-nonce"),
            &verifier(evidence()),
            now(),
        )
        .expect_err("one nonce cannot back a second idempotency key");
        assert!(nonce_error.contains("nonce replay"), "{nonce_error}");

        let mut copied_request = request("verify-original");
        copied_request.connection_id = "placement:forged-host:connection-9".into();
        let copied_error = admit_verified_agent_connection(
            &server,
            &copied_request,
            &verifier(evidence()),
            now(),
        )
        .expect_err("a copied receipt cannot bind another placement");
        assert!(copied_error.contains("idempotency conflict"), "{copied_error}");
        assert_eq!(verified_count(&server), 1);
    }
}
