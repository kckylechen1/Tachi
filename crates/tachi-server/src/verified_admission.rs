//! Trusted server boundary for verified AgentIdentity admissions (#1938).
//!
//! The verifier capability, bounded evidence type, and persistence writer are
//! all crate-private. memcore exposes only exact-bound read verification, so a
//! normal admin caller cannot mint a `verified` row by constructing a low-level
//! write request. Raw database access remains trusted-code authority.

// #1938 lands the typed server port before #1170 supplies its production
// provider adapter; the production wrapper is intentionally not model-facing.
#![cfg_attr(not(test), allow(dead_code))]

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};

use crate::server_state::{MemoryServer, VerifiedAdmissionContext};

const MAX_VERIFIED_EVIDENCE_AGE: Duration = Duration::minutes(5);
const MAX_VERIFIED_EVIDENCE_LIFETIME: Duration = Duration::minutes(15);
const MAX_BOUND_FIELD_BYTES: usize = 256;
const SHA256_HEX_BYTES: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct VerifiedAdmissionRequest {
    pub agent_identity_id: String,
    pub connection_id: String,
    pub expected_issuer_id: String,
    pub expected_verification_method: String,
    pub expected_verification_version: String,
    pub trust_domain: String,
    pub verification_scope: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifiedEvidenceState {
    Active,
    Revoked,
}

/// Secret-negative reference assembled only from bounded identifier tokens.
/// Raw URLs, credentials, keys, and attestation bytes have no representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedEvidenceRef(String);

impl VerifiedEvidenceRef {
    pub(crate) fn attestation(provider: &str, opaque_id: &str) -> Result<Self, String> {
        fn token(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 96
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
        }
        if !token(provider) || !token(opaque_id) {
            return Err("verified evidence reference tokens are not canonical".to_string());
        }
        Ok(Self(format!("attestation:{provider}:{opaque_id}")))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedAdmissionEvidence {
    pub agent_identity_id: String,
    pub connection_id: String,
    pub issuer_id: String,
    pub verification_method: String,
    pub verification_version: String,
    pub trust_domain: String,
    pub verification_scope: String,
    pub evidence_digest: String,
    pub evidence_ref: VerifiedEvidenceRef,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    pub state: VerifiedEvidenceState,
}

/// Server-private verifier capability. No handler, transport field, model
/// input, or downstream crate can implement or obtain this trait object.
pub(crate) trait TrustedAdmissionVerifier {
    fn verify(
        &self,
        request: &VerifiedAdmissionRequest,
    ) -> Result<VerifiedAdmissionEvidence, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VerifiedAdmissionWriteOutcome {
    Created(memcore::VerifiedAdmissionReceipt),
    Replayed(memcore::VerifiedAdmissionReceipt),
}

impl VerifiedAdmissionWriteOutcome {
    fn receipt(&self) -> &memcore::VerifiedAdmissionReceipt {
        match self {
            Self::Created(receipt) | Self::Replayed(receipt) => receipt,
        }
    }
}

fn stable_receipt_ids(issuer_id: &str, idempotency_key: &str) -> (String, String) {
    let binding = format!("{issuer_id}\0{idempotency_key}");
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, binding.as_bytes());
    (
        format!("admission-verified-{id}"),
        format!("verified-admission-receipt-{id}"),
    )
}

fn valid_bound_field(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_BOUND_FIELD_BYTES
        && !value.as_bytes().contains(&0)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == SHA256_HEX_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_verified_evidence(
    request: &VerifiedAdmissionRequest,
    evidence: &VerifiedAdmissionEvidence,
    now: DateTime<Utc>,
) -> Result<(), String> {
    if !crate::session_identity::valid_agent_identity_assertion(&request.agent_identity_id)
        || [
            request.connection_id.as_str(),
            request.expected_issuer_id.as_str(),
            request.expected_verification_method.as_str(),
            request.expected_verification_version.as_str(),
            request.trust_domain.as_str(),
            request.verification_scope.as_str(),
            request.idempotency_key.as_str(),
        ]
        .iter()
        .any(|value| !valid_bound_field(value))
    {
        return Err("verified admission request binding is invalid".to_string());
    }
    if request.expected_verification_method != memcore::VERIFIED_ADMISSION_METHOD
        || request.expected_verification_version != memcore::VERIFIED_ADMISSION_VERSION
        || request.verification_scope != memcore::VERIFIED_ADMISSION_SCOPE
    {
        return Err("verified admission verifier policy is unsupported".to_string());
    }
    if evidence.state == VerifiedEvidenceState::Revoked {
        return Err("verified admission evidence is revoked".to_string());
    }
    if evidence.agent_identity_id != request.agent_identity_id {
        return Err("verified admission evidence binds the wrong AgentIdentity".to_string());
    }
    if evidence.connection_id != request.connection_id {
        return Err("verified admission evidence binds the wrong connection".to_string());
    }
    if evidence.issuer_id != request.expected_issuer_id {
        return Err("verified admission evidence comes from the wrong issuer".to_string());
    }
    if evidence.verification_method != request.expected_verification_method
        || evidence.verification_version != request.expected_verification_version
    {
        return Err("verified admission evidence uses the wrong method or version".to_string());
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
        .any(|value| !valid_bound_field(value) || tachi_lesson_forge::contains_secret_like(value))
        || memcore::catalog::endpoint::endpoint_credential_leak(evidence.evidence_ref.as_str())
            .is_some()
    {
        return Err("verified admission evidence contains invalid or secret-like material".into());
    }
    if !valid_sha256(&evidence.evidence_digest) {
        return Err("verified admission evidence digest is not canonical SHA-256".to_string());
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
    let lifetime = evidence
        .expires_at
        .signed_duration_since(evidence.issued_at);
    if lifetime <= Duration::zero() || lifetime > MAX_VERIFIED_EVIDENCE_LIFETIME {
        return Err("verified admission evidence lifetime exceeds the bounded window".to_string());
    }
    Ok(())
}

fn canonical_request_digest(
    request: &VerifiedAdmissionRequest,
    evidence: &VerifiedAdmissionEvidence,
    admission_id: &str,
    receipt_id: &str,
) -> String {
    memcore::canonical_json_digest_hex(&serde_json::json!({
        "admission_id": admission_id,
        "agent_identity_id": request.agent_identity_id,
        "connection_id": request.connection_id,
        "current_state": "verified",
        "evidence_digest": evidence.evidence_digest,
        "evidence_expires_at": evidence.expires_at.to_rfc3339(),
        "evidence_issued_at": evidence.issued_at.to_rfc3339(),
        "evidence_ref": evidence.evidence_ref.as_str(),
        "idempotency_key": request.idempotency_key,
        "issuer_id": evidence.issuer_id,
        "nonce": evidence.nonce,
        "receipt_id": receipt_id,
        "trust_domain": evidence.trust_domain,
        "verification_method": evidence.verification_method,
        "verification_scope": evidence.verification_scope,
        "verification_version": evidence.verification_version,
    }))
}

fn find_receipt_by_idempotency(
    tx: &Transaction<'_>,
    issuer_id: &str,
    idempotency_key: &str,
) -> Result<Option<memcore::VerifiedAdmissionReceipt>, String> {
    let admission_id: Option<String> = tx
        .query_row(
            "SELECT admission_id FROM identity_admission_verification_receipts
             WHERE issuer_id=?1 AND idempotency_key=?2",
            params![issuer_id, idempotency_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    admission_id
        .map(|admission_id| {
            memcore::get_verified_admission_receipt(tx, &admission_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "verified admission receipt lookup vanished".to_string())
        })
        .transpose()
}

fn persist_verified_admission(
    server: &MemoryServer,
    request: &VerifiedAdmissionRequest,
    evidence: &VerifiedAdmissionEvidence,
    authoritative_now: impl FnOnce() -> DateTime<Utc>,
) -> Result<VerifiedAdmissionWriteOutcome, String> {
    let (admission_id, receipt_id) =
        stable_receipt_ids(&evidence.issuer_id, &request.idempotency_key);
    let request_digest = canonical_request_digest(request, evidence, &admission_id, &receipt_id);
    server.with_global_store(|store| {
        let tx = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        // Freshness is authoritative only after the persistence write lock is
        // held. Verification and lock acquisition may both block long enough
        // for otherwise valid evidence to expire.
        let verified_at = authoritative_now();
        validate_verified_evidence(request, evidence, verified_at)?;
        if let Some(existing) = find_receipt_by_idempotency(
            &tx,
            &evidence.issuer_id,
            &request.idempotency_key,
        )? {
            if existing.request_digest != request_digest {
                return Err("verified admission idempotency conflict".to_string());
            }
            tx.commit().map_err(|error| error.to_string())?;
            return Ok(VerifiedAdmissionWriteOutcome::Replayed(existing));
        }
        let nonce_used: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM identity_admission_verification_receipts
                 WHERE issuer_id=?1 AND trust_domain=?2 AND nonce=?3)",
                params![evidence.issuer_id, evidence.trust_domain, evidence.nonce],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if nonce_used {
            return Err("verified admission nonce replay".to_string());
        }
        let connection_already_admitted: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM identity_admissions
                 WHERE agent_identity_id=?1 AND connection_id=?2)",
                params![request.agent_identity_id, request.connection_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if connection_already_admitted {
            return Err(
                "AgentIdentity connection already has an append-only admission; verified admission requires a new connection"
                    .to_string(),
            );
        }
        let verified_at = verified_at.to_rfc3339();
        tx.execute(
            "INSERT INTO agent_identities (agent_identity_id, created_at) VALUES (?1, ?2)
             ON CONFLICT(agent_identity_id) DO NOTHING",
            params![request.agent_identity_id, verified_at],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO identity_admissions
             (admission_id, agent_identity_id, connection_id, state, created_at)
             VALUES (?1, ?2, ?3, 'verified', ?4)",
            params![
                admission_id,
                request.agent_identity_id,
                request.connection_id,
                verified_at
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO identity_admission_verification_receipts
             (receipt_id, admission_id, agent_identity_id, connection_id, issuer_id,
              verification_method, verification_version, trust_domain, verification_scope,
              evidence_digest, evidence_ref, evidence_issued_at, evidence_expires_at, nonce,
              idempotency_key, request_digest, current_state, verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, 'verified', ?17)",
            params![
                receipt_id,
                admission_id,
                request.agent_identity_id,
                request.connection_id,
                evidence.issuer_id,
                evidence.verification_method,
                evidence.verification_version,
                evidence.trust_domain,
                evidence.verification_scope,
                evidence.evidence_digest,
                evidence.evidence_ref.as_str(),
                evidence.issued_at.to_rfc3339(),
                evidence.expires_at.to_rfc3339(),
                evidence.nonce,
                request.idempotency_key,
                request_digest,
                verified_at,
            ],
        )
        .map_err(|error| error.to_string())?;
        let receipt = memcore::get_verified_admission_receipt(&tx, &admission_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "verified admission receipt insert vanished".to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(VerifiedAdmissionWriteOutcome::Created(receipt))
    })
}

/// Production entry uses the server clock. A future #1170 adapter calls this
/// after provider proof verification; callers cannot inject freshness time.
pub(crate) fn admit_verified_agent_connection(
    server: &MemoryServer,
    request: &VerifiedAdmissionRequest,
    verifier: &dyn TrustedAdmissionVerifier,
) -> Result<(), String> {
    admit_verified_agent_connection_with_clock(server, request, verifier, Utc::now).map(|_| ())
}

fn admit_verified_agent_connection_at(
    server: &MemoryServer,
    request: &VerifiedAdmissionRequest,
    verifier: &dyn TrustedAdmissionVerifier,
    now: DateTime<Utc>,
) -> Result<VerifiedAdmissionWriteOutcome, String> {
    admit_verified_agent_connection_with_clock(server, request, verifier, || now)
}

fn admit_verified_agent_connection_with_clock(
    server: &MemoryServer,
    request: &VerifiedAdmissionRequest,
    verifier: &dyn TrustedAdmissionVerifier,
    authoritative_now: impl FnOnce() -> DateTime<Utc>,
) -> Result<VerifiedAdmissionWriteOutcome, String> {
    let evidence = verifier.verify(request)?;
    let outcome = persist_verified_admission(server, request, &evidence, authoritative_now)?;
    let receipt = outcome.receipt();
    server.set_verified_work_claim_connection(VerifiedAdmissionContext {
        admission_id: receipt.admission_id.clone(),
        agent_identity_id: receipt.agent_identity_id.clone(),
        connection_id: receipt.connection_id.clone(),
        issuer_id: receipt.issuer_id.clone(),
        verification_method: receipt.verification_method.clone(),
        verification_version: receipt.verification_version.clone(),
        trust_domain: receipt.trust_domain.clone(),
        verification_scope: receipt.verification_scope.clone(),
    });
    Ok(outcome)
}

pub(crate) fn require_current_verified_admission(server: &MemoryServer) -> Result<(), String> {
    let context = server
        .verified_work_claim_connection()
        .ok_or_else(|| "verified admission context is unavailable".to_string())?;
    let current = server.with_global_store_read(|store| {
        memcore::has_current_verified_admission(
            store.connection(),
            &context.admission_id,
            &context.agent_identity_id,
            &context.connection_id,
            &context.issuer_id,
            &context.verification_method,
            &context.verification_version,
            &context.trust_domain,
            &context.verification_scope,
        )
        .map_err(|error| error.to_string())
    })?;
    if current {
        Ok(())
    } else {
        Err("verified admission is expired, revoked, or no longer bound to this connection".into())
    }
}

/// Serialize a mutation with the durable authority check for the current
/// host admission. A committed revocation therefore either precedes the
/// check (and refuses the write) or follows the committed mutation; there is
/// no check/revoke/write gap. Explicit local SelfAsserted admissions retain
/// their existing local-only mutation path.
pub(crate) fn with_current_admission_write<T>(
    server: &MemoryServer,
    operation: impl FnOnce(&rusqlite::Connection) -> Result<T, memcore::MemoryError>,
) -> Result<T, String> {
    let (_, _, state) = server
        .work_claim_connection()
        .ok_or_else(|| "AgentIdentity admission is unavailable; initialize first".to_string())?;
    match state.as_str() {
        "self_asserted" => server.with_global_store(|store| {
            operation(store.connection()).map_err(|error| error.to_string())
        }),
        "verified" => {
            let context = server
                .verified_work_claim_connection()
                .ok_or_else(|| "verified admission context is unavailable".to_string())?;
            let binding = memcore::VerifiedAdmissionBinding {
                admission_id: context.admission_id,
                agent_identity_id: context.agent_identity_id,
                connection_id: context.connection_id,
                issuer_id: context.issuer_id,
                verification_method: context.verification_method,
                verification_version: context.verification_version,
                trust_domain: context.trust_domain,
                verification_scope: context.verification_scope,
            };
            server.with_global_store(|store| {
                memcore::with_current_verified_admission_write(
                    store.connection(),
                    &binding,
                    operation,
                )
                .map_err(|error| error.to_string())
            })
        }
        "rejected" => Err("AgentIdentity admission rejected".to_string()),
        _ => Err(
            "AgentIdentity admission unavailable; remote identity has no #1170 proof".to_string(),
        ),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedRevocationEvidence {
    pub admission_id: String,
    pub issuer_id: String,
    pub evidence_digest: String,
    pub evidence_ref: VerifiedEvidenceRef,
    pub nonce: String,
    pub revoked_at: DateTime<Utc>,
}

pub(crate) trait TrustedRevocationVerifier {
    fn verify_revocation(&self, admission_id: &str) -> Result<VerifiedRevocationEvidence, String>;
}

pub(crate) fn revoke_verified_admission(
    server: &MemoryServer,
    admission_id: &str,
    verifier: &dyn TrustedRevocationVerifier,
) -> Result<(), String> {
    revoke_verified_admission_at(server, admission_id, verifier, Utc::now())
}

fn revoke_verified_admission_at(
    server: &MemoryServer,
    admission_id: &str,
    verifier: &dyn TrustedRevocationVerifier,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let evidence = verifier.verify_revocation(admission_id)?;
    if evidence.admission_id != admission_id
        || !valid_bound_field(&evidence.issuer_id)
        || !valid_bound_field(&evidence.nonce)
        || !valid_sha256(&evidence.evidence_digest)
        || !valid_bound_field(evidence.evidence_ref.as_str())
        || tachi_lesson_forge::contains_secret_like(&evidence.issuer_id)
        || tachi_lesson_forge::contains_secret_like(&evidence.nonce)
        || tachi_lesson_forge::contains_secret_like(evidence.evidence_ref.as_str())
        || memcore::catalog::endpoint::endpoint_credential_leak(evidence.evidence_ref.as_str())
            .is_some()
        || evidence.revoked_at > now
        || now.signed_duration_since(evidence.revoked_at) > MAX_VERIFIED_EVIDENCE_AGE
    {
        return Err("verified admission revocation evidence is invalid or stale".into());
    }
    server.with_global_store(|store| {
        let receipt = memcore::get_verified_admission_receipt(store.connection(), admission_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "verified admission receipt is missing".to_string())?;
        if receipt.issuer_id != evidence.issuer_id {
            return Err("verified admission revocation comes from the wrong issuer".to_string());
        }
        let revocation_id = format!(
            "verified-admission-revocation-{}",
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("{}\0{}", evidence.issuer_id, evidence.nonce).as_bytes(),
            )
        );
        store
            .connection_mut()
            .execute(
                "INSERT INTO identity_admission_verification_revocations
                 (revocation_id, admission_id, issuer_id, evidence_digest, evidence_ref, nonce, revoked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    revocation_id,
                    evidence.admission_id,
                    evidence.issuer_id,
                    evidence.evidence_digest,
                    evidence.evidence_ref.as_str(),
                    evidence.nonce,
                    evidence.revoked_at.to_rfc3339(),
                ],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    })
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

    struct DelayedVerifier {
        evidence: VerifiedAdmissionEvidence,
        delay: std::time::Duration,
    }

    impl TrustedAdmissionVerifier for DelayedVerifier {
        fn verify(
            &self,
            _request: &VerifiedAdmissionRequest,
        ) -> Result<VerifiedAdmissionEvidence, String> {
            std::thread::sleep(self.delay);
            Ok(self.evidence.clone())
        }
    }

    struct FakeRevocation(VerifiedRevocationEvidence);

    impl TrustedRevocationVerifier for FakeRevocation {
        fn verify_revocation(
            &self,
            _admission_id: &str,
        ) -> Result<VerifiedRevocationEvidence, String> {
            Ok(self.0.clone())
        }
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn request(key: &str) -> VerifiedAdmissionRequest {
        VerifiedAdmissionRequest {
            agent_identity_id: "agent.remote.alpha".into(),
            connection_id: "placement/runner-alpha/connection-7".into(),
            expected_issuer_id: "issuer.device-trust.alpha".into(),
            expected_verification_method: memcore::VERIFIED_ADMISSION_METHOD.into(),
            expected_verification_version: memcore::VERIFIED_ADMISSION_VERSION.into(),
            trust_domain: "workspace:kckylechen1/tachi".into(),
            verification_scope: memcore::VERIFIED_ADMISSION_SCOPE.into(),
            idempotency_key: key.into(),
        }
    }

    fn evidence(at: DateTime<Utc>) -> VerifiedAdmissionEvidence {
        VerifiedAdmissionEvidence {
            agent_identity_id: "agent.remote.alpha".into(),
            connection_id: "placement/runner-alpha/connection-7".into(),
            issuer_id: "issuer.device-trust.alpha".into(),
            verification_method: memcore::VERIFIED_ADMISSION_METHOD.into(),
            verification_version: memcore::VERIFIED_ADMISSION_VERSION.into(),
            trust_domain: "workspace:kckylechen1/tachi".into(),
            verification_scope: memcore::VERIFIED_ADMISSION_SCOPE.into(),
            evidence_digest: format!("{:x}", Sha256::digest(RAW_SECRET.as_bytes())),
            evidence_ref: VerifiedEvidenceRef::attestation("device-envelope", "alpha-7").unwrap(),
            issued_at: at - Duration::minutes(1),
            expires_at: at + Duration::minutes(9),
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

    fn uncommitted_revocation(
        server: &MemoryServer,
        admission_id: &str,
        nonce: &str,
    ) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(server.global_db_path_buf()).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON; BEGIN IMMEDIATE")
            .unwrap();
        conn.execute(
            "INSERT INTO identity_admission_verification_revocations
             (revocation_id, admission_id, issuer_id, evidence_digest, evidence_ref, nonce, revoked_at)
             VALUES (?1, ?2, 'issuer.device-trust.alpha', ?3,
                     'attestation:device-envelope:concurrent-revocation', ?4, ?5)",
            params![
                format!("revocation-{nonce}"),
                admission_id,
                "c".repeat(64),
                nonce,
                Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();
        conn
    }

    #[test]
    fn trusted_verifier_writes_bound_secret_negative_receipt_and_exact_replay() {
        let server = make_server();
        let at = now();
        let first = admit_verified_agent_connection_at(
            &server,
            &request("verify-alpha-7"),
            &verifier(evidence(at)),
            at,
        )
        .expect("fresh trusted evidence must admit");
        assert!(matches!(first, VerifiedAdmissionWriteOutcome::Created(_)));
        let receipt = first.receipt();
        assert_eq!(receipt.connection_id, "placement/runner-alpha/connection-7");
        assert_eq!(
            receipt.verification_method,
            memcore::VERIFIED_ADMISSION_METHOD
        );

        let replay = admit_verified_agent_connection_at(
            &server,
            &request("verify-alpha-7"),
            &verifier(evidence(at)),
            at,
        )
        .expect("same issuer/key/digest must replay");
        assert!(matches!(replay, VerifiedAdmissionWriteOutcome::Replayed(_)));
        assert_eq!(replay.receipt(), receipt);
        assert_eq!(verified_count(&server), 1);
        require_current_verified_admission(&server).expect("fresh exact binding");
        let mut copied_context = server.verified_work_claim_connection().unwrap();
        copied_context.connection_id = "placement/runner-beta/connection-8".into();
        server.set_verified_work_claim_connection(copied_context);
        require_current_verified_admission(&server)
            .expect_err("fresh receipt must not authorize another connection");
        server.set_verified_work_claim_connection(VerifiedAdmissionContext {
            admission_id: receipt.admission_id.clone(),
            agent_identity_id: receipt.agent_identity_id.clone(),
            connection_id: receipt.connection_id.clone(),
            issuer_id: receipt.issuer_id.clone(),
            verification_method: receipt.verification_method.clone(),
            verification_version: receipt.verification_version.clone(),
            trust_domain: receipt.trust_domain.clone(),
            verification_scope: receipt.verification_scope.clone(),
        });

        server
            .with_global_store_read(|store| {
                let persisted: String = store
                    .connection()
                    .query_row(
                        "SELECT group_concat(value, '') FROM (
                           SELECT receipt_id AS value FROM identity_admission_verification_receipts
                           UNION ALL SELECT evidence_ref FROM identity_admission_verification_receipts
                           UNION ALL SELECT evidence_digest FROM identity_admission_verification_receipts
                         )",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                assert!(!persisted.contains(RAW_SECRET));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn production_wrappers_use_authoritative_clock_and_append_revocation() {
        let expired_server = make_server();
        let at = Utc::now();
        let mut expiring = evidence(at);
        expiring.issued_at = at - Duration::minutes(1);
        expiring.expires_at = at + Duration::milliseconds(100);
        let error = admit_verified_agent_connection(
            &expired_server,
            &request("production-expiry"),
            &DelayedVerifier {
                evidence: expiring,
                delay: std::time::Duration::from_millis(250),
            },
        )
        .expect_err("evidence expiring during verification must not be persisted");
        assert!(error.contains("expired"), "{error}");
        assert_eq!(verified_count(&expired_server), 0);
        assert!(expired_server.verified_work_claim_connection().is_none());

        let server = make_server();
        admit_verified_agent_connection(
            &server,
            &request("production-wrappers"),
            &verifier(evidence(Utc::now())),
        )
        .unwrap();
        let admission_id = stable_receipt_ids("issuer.device-trust.alpha", "production-wrappers").0;
        revoke_verified_admission(
            &server,
            &admission_id,
            &FakeRevocation(VerifiedRevocationEvidence {
                admission_id: admission_id.clone(),
                issuer_id: "issuer.device-trust.alpha".into(),
                evidence_digest: format!("{:x}", Sha256::digest(b"production-revocation")),
                evidence_ref: VerifiedEvidenceRef::attestation(
                    "device-envelope",
                    "production-revocation",
                )
                .unwrap(),
                nonce: "production-revocation-nonce".into(),
                revoked_at: Utc::now(),
            }),
        )
        .unwrap();
        require_current_verified_admission(&server)
            .expect_err("production revocation wrapper must remove authority");
    }

    #[test]
    fn append_only_existing_connection_requires_new_connection() {
        let server = make_server();
        let at = now();
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "INSERT INTO agent_identities (agent_identity_id, created_at)
                         VALUES (?1, ?2)",
                        params!["agent.remote.alpha", at.to_rfc3339()],
                    )
                    .map_err(|error| error.to_string())?;
                memcore::record_unverified_admission(
                    store.connection(),
                    "admission-bootstrap-unavailable",
                    "agent.remote.alpha",
                    "placement/runner-alpha/connection-7",
                    memcore::UnverifiedAdmissionState::Unavailable,
                )
                .map_err(|error| error.to_string())
            })
            .unwrap();
        let error = admit_verified_agent_connection_at(
            &server,
            &request("upgrade-existing-connection"),
            &verifier(evidence(at)),
            at,
        )
        .expect_err("append-only connection history cannot be upgraded in place");
        assert!(error.contains("requires a new connection"), "{error}");
        assert_eq!(verified_count(&server), 0);
        let state: String = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT state FROM identity_admissions
                         WHERE admission_id='admission-bootstrap-unavailable'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        assert_eq!(state, "unavailable");
    }

    #[test]
    fn committed_revocation_wins_before_work_claim_mutation_on_independent_connection() {
        let server = make_server();
        admit_verified_agent_connection(
            &server,
            &request("claim-revocation-race"),
            &verifier(evidence(Utc::now())),
        )
        .unwrap();
        let admission_id =
            stable_receipt_ids("issuer.device-trust.alpha", "claim-revocation-race").0;
        let locker = uncommitted_revocation(&server, &admission_id, "claim-race");
        let params: crate::tool_params::TachiTaskParams =
            serde_json::from_value(serde_json::json!({
                "action": "claim",
                "branch": "lane/revocation-race",
                "claim_role": "executor",
                "claim_mode": "writable",
                "worktree_path": "/tmp/revocation-race",
                "claim_scope": ["src/lib.rs"],
                "expected_head": "head",
                "lease_expires_at": "2099-01-01T00:00:00Z"
            }))
            .unwrap();
        let error = std::thread::scope(|scope| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let server = &server;
            let params = &params;
            let worker = scope.spawn(move || {
                started_tx.send(()).unwrap();
                crate::claims_ops::handle_task_claim(server, params)
            });
            started_rx.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            locker.execute_batch("COMMIT").unwrap();
            worker
                .join()
                .unwrap()
                .expect_err("committed revocation must win before claim insertion")
        });
        assert!(error.contains("expired, revoked"), "{error}");
        let claims: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM session_claims", [], |row| row.get(0))
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        assert_eq!(claims, 0);
    }

    #[test]
    fn committed_revocation_wins_before_delivery_mutation_on_independent_connection() {
        let server = make_server();
        admit_verified_agent_connection(
            &server,
            &request("delivery-revocation-race"),
            &verifier(evidence(Utc::now())),
        )
        .unwrap();
        server
            .with_global_store(|store| {
                memcore::mint_delivery_intent(
                    store.connection(),
                    &memcore::NewDeliveryIntent {
                        idempotency_key: "delivery-revocation-race".into(),
                        execution_source: memcore::DeliveryExecutionSource::ManagedDispatch,
                        execution_ref: "dispatch-revocation-race".into(),
                        terminal_receipt_revision: 1,
                        work_claim_id: None,
                        result_ref: "artifact://revocation-race/result".into(),
                        result_revision: 1,
                        payload_digest: "sha256-revocation-race".into(),
                        visibility_class: memcore::DeliveryVisibilityClass::Public,
                        delivery_policy: memcore::DeliveryPolicy::ReturnToCurrentCall,
                        protocol_capability: "result-ref-v1".into(),
                        requester: memcore::DeliveryRequesterBinding::default(),
                        expires_at: None,
                        correction: false,
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            })
            .unwrap();
        let admission_id =
            stable_receipt_ids("issuer.device-trust.alpha", "delivery-revocation-race").0;
        let locker = uncommitted_revocation(&server, &admission_id, "delivery-race");
        let params: crate::tool_params::TachiDeliveryParams =
            serde_json::from_value(serde_json::json!({
                "action": "claim_ready_delivery",
                "agent_identity_id": "agent.remote.alpha",
                "host_identity": "agent.remote.alpha",
                "claim_key": "delivery-race-claim",
                "lease_seconds": 300
            }))
            .unwrap();
        let error = std::thread::scope(|scope| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let server = &server;
            let worker = scope.spawn(move || {
                started_tx.send(()).unwrap();
                crate::delivery_ops::handle_tachi_delivery(server, params)
            });
            started_rx.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            locker.execute_batch("COMMIT").unwrap();
            worker
                .join()
                .unwrap()
                .expect_err("committed revocation must win before delivery claim")
        });
        assert!(error.contains("expired, revoked"), "{error}");
        let state: (String, i64) = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT delivery_state, attempt_count FROM delivery_intents
                         WHERE idempotency_key='delivery-revocation-race'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        assert_eq!(state, ("ready".to_string(), 0));
    }

    #[test]
    fn same_id_different_digest_and_fresh_cross_connection_conflict() {
        let server = make_server();
        let at = now();
        admit_verified_agent_connection_at(
            &server,
            &request("same-key"),
            &verifier(evidence(at)),
            at,
        )
        .unwrap();

        let mut changed = evidence(at);
        changed.evidence_digest = format!("{:x}", Sha256::digest(b"different-evidence"));
        let digest_error = admit_verified_agent_connection_at(
            &server,
            &request("same-key"),
            &verifier(changed),
            at,
        )
        .expect_err("same idempotency key with changed digest must conflict");
        assert!(
            digest_error.contains("idempotency conflict"),
            "{digest_error}"
        );

        let mut moved_request = request("same-key");
        moved_request.connection_id = "placement/runner-beta/connection-8".into();
        let mut moved_evidence = evidence(at);
        moved_evidence.connection_id = moved_request.connection_id.clone();
        moved_evidence.nonce = "nonce-beta-8".into();
        let connection_error = admit_verified_agent_connection_at(
            &server,
            &moved_request,
            &verifier(moved_evidence),
            at,
        )
        .expect_err("fresh evidence cannot replay one key onto another connection");
        assert!(
            connection_error.contains("idempotency conflict"),
            "{connection_error}"
        );
        assert_eq!(verified_count(&server), 1);
    }

    #[test]
    fn wrong_binding_freshness_policy_and_overlong_evidence_write_nothing() {
        let at = now();
        let cases: Vec<(
            &str,
            VerifiedAdmissionRequest,
            VerifiedAdmissionEvidence,
            &str,
        )> = vec![
            (
                "expired",
                request("expired"),
                VerifiedAdmissionEvidence {
                    expires_at: at - Duration::seconds(1),
                    ..evidence(at)
                },
                "expired",
            ),
            (
                "not-yet-valid",
                request("future"),
                VerifiedAdmissionEvidence {
                    issued_at: at + Duration::seconds(1),
                    ..evidence(at)
                },
                "not yet valid",
            ),
            (
                "stale",
                request("stale"),
                VerifiedAdmissionEvidence {
                    issued_at: at - Duration::minutes(6),
                    expires_at: at + Duration::minutes(1),
                    ..evidence(at)
                },
                "stale",
            ),
            (
                "wrong-connection",
                request("wrong-connection"),
                VerifiedAdmissionEvidence {
                    connection_id: "placement/attacker/connection-1".into(),
                    ..evidence(at)
                },
                "wrong connection",
            ),
            (
                "wrong-domain",
                request("wrong-domain"),
                VerifiedAdmissionEvidence {
                    trust_domain: "workspace:attacker/repo".into(),
                    ..evidence(at)
                },
                "wrong trust domain",
            ),
            (
                "wrong-issuer",
                request("wrong-issuer"),
                VerifiedAdmissionEvidence {
                    issuer_id: "issuer.attacker".into(),
                    ..evidence(at)
                },
                "wrong issuer",
            ),
            (
                "wrong-identity",
                request("wrong-identity"),
                VerifiedAdmissionEvidence {
                    agent_identity_id: "agent.remote.attacker".into(),
                    ..evidence(at)
                },
                "wrong AgentIdentity",
            ),
            (
                "wrong-method",
                request("wrong-method"),
                VerifiedAdmissionEvidence {
                    verification_method: "display-name".into(),
                    ..evidence(at)
                },
                "wrong method or version",
            ),
            (
                "wrong-scope",
                request("wrong-scope"),
                VerifiedAdmissionEvidence {
                    verification_scope: "agent_identity:display_only".into(),
                    ..evidence(at)
                },
                "wrong scope",
            ),
            (
                "overlong-lifetime",
                request("overlong-lifetime"),
                VerifiedAdmissionEvidence {
                    expires_at: at + MAX_VERIFIED_EVIDENCE_LIFETIME + Duration::seconds(1),
                    ..evidence(at)
                },
                "lifetime exceeds",
            ),
            (
                "revoked",
                request("revoked"),
                VerifiedAdmissionEvidence {
                    state: VerifiedEvidenceState::Revoked,
                    ..evidence(at)
                },
                "revoked",
            ),
            (
                "overlong-ref",
                request("overlong-ref"),
                VerifiedAdmissionEvidence {
                    evidence_ref: VerifiedEvidenceRef("x".repeat(MAX_BOUND_FIELD_BYTES + 1)),
                    ..evidence(at)
                },
                "invalid or secret-like",
            ),
        ];
        for (label, request, evidence, expected) in cases {
            let server = make_server();
            let error =
                admit_verified_agent_connection_at(&server, &request, &verifier(evidence), at)
                    .expect_err(label);
            assert!(error.contains(expected), "{label}: {error}");
            assert_eq!(verified_count(&server), 0, "{label}");
        }
        assert!(
            VerifiedEvidenceRef::attestation("https://attacker.invalid?token=secret", "one")
                .is_err(),
            "free-form secret-bearing evidence references must be structurally unavailable"
        );
        let mut unsupported_request = request("unsupported-method");
        unsupported_request.expected_verification_method = "display-name".into();
        let error = admit_verified_agent_connection_at(
            &make_server(),
            &unsupported_request,
            &verifier(evidence(at)),
            at,
        )
        .expect_err("caller-selected method policy must be rejected");
        assert!(error.contains("policy is unsupported"), "{error}");
    }

    #[test]
    fn nonce_replay_cannot_back_a_second_admission() {
        let server = make_server();
        let at = now();
        admit_verified_agent_connection_at(
            &server,
            &request("nonce-first"),
            &verifier(evidence(at)),
            at,
        )
        .unwrap();
        let error = admit_verified_agent_connection_at(
            &server,
            &request("nonce-second"),
            &verifier(evidence(at)),
            at,
        )
        .expect_err("one issuer/domain nonce cannot mint a second admission");
        assert!(error.contains("nonce replay"), "{error}");
        assert_eq!(verified_count(&server), 1);
    }

    #[test]
    fn authoritative_clock_expiry_and_append_only_revocation_remove_authority() {
        let server = make_server();
        let actual_now = now();
        let synthetic_admission_time = actual_now - Duration::seconds(2);
        let mut soon_expired = evidence(synthetic_admission_time);
        soon_expired.issued_at = synthetic_admission_time - Duration::minutes(1);
        soon_expired.expires_at = actual_now - Duration::seconds(1);
        let expired_admission = admit_verified_agent_connection_at(
            &server,
            &request("expired-at-consumer"),
            &verifier(soon_expired),
            synthetic_admission_time,
        )
        .expect("evidence is fresh at mint time");
        require_current_verified_admission(&server)
            .expect_err("authoritative read clock must reject post-mint expiry");
        let claim_params: crate::tool_params::TachiTaskParams =
            serde_json::from_value(serde_json::json!({
                "action": "claim",
                "branch": "lane/expired",
                "claim_role": "executor",
                "claim_mode": "writable",
                "worktree_path": "/tmp/expired",
                "claim_scope": ["src/lib.rs"],
                "expected_head": "head",
                "lease_expires_at": "2099-01-01T00:00:00Z"
            }))
            .unwrap();
        let claim_error = crate::claims_ops::handle_task_claim(&server, &claim_params)
            .expect_err("WorkClaim authority must not outlive verified evidence");
        assert!(claim_error.contains("expired, revoked"), "{claim_error}");
        let attachment_error = crate::agent_eval::current_host_admission(
            &server,
            Some("agent.remote.alpha".into()),
            Some(expired_admission.receipt().admission_id.clone()),
        )
        .expect_err("attachment authority must not outlive verified evidence");
        assert!(
            attachment_error.contains("expired, revoked"),
            "{attachment_error}"
        );

        let server = make_server();
        let at = now();
        let admitted = admit_verified_agent_connection_at(
            &server,
            &request("revocation"),
            &verifier(evidence(at)),
            at,
        )
        .unwrap();
        let admission_id = admitted.receipt().admission_id.clone();
        let revocation = VerifiedRevocationEvidence {
            admission_id: admission_id.clone(),
            issuer_id: "issuer.device-trust.alpha".into(),
            evidence_digest: format!("{:x}", Sha256::digest(b"revocation-proof")),
            evidence_ref: VerifiedEvidenceRef::attestation("device-envelope", "revoke-alpha-7")
                .unwrap(),
            nonce: "revocation-nonce-alpha-7".into(),
            revoked_at: at,
        };
        revoke_verified_admission_at(&server, &admission_id, &FakeRevocation(revocation), at)
            .unwrap();
        require_current_verified_admission(&server)
            .expect_err("append-only revocation must remove authority immediately");
        let claim_error = crate::claims_ops::handle_task_claim(&server, &claim_params)
            .expect_err("revocation must remove WorkClaim authority immediately");
        assert!(claim_error.contains("expired, revoked"), "{claim_error}");
        server
            .with_global_store_read(|store| {
                let receipt_count: i64 = store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM identity_admission_verification_receipts",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                let revocation_count: i64 = store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM identity_admission_verification_revocations",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                assert_eq!((receipt_count, revocation_count), (1, 1));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn concurrent_exact_requests_create_once_and_replay_once() {
        let server = make_server();
        let at = now();
        let outcomes = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                admit_verified_agent_connection_at(
                    &server,
                    &request("concurrent"),
                    &verifier(evidence(at)),
                    at,
                )
            });
            let second = scope.spawn(|| {
                admit_verified_agent_connection_at(
                    &server,
                    &request("concurrent"),
                    &verifier(evidence(at)),
                    at,
                )
            });
            vec![
                first.join().unwrap().unwrap(),
                second.join().unwrap().unwrap(),
            ]
        });
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, VerifiedAdmissionWriteOutcome::Created(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, VerifiedAdmissionWriteOutcome::Replayed(_)))
                .count(),
            1
        );
        assert_eq!(verified_count(&server), 1);
    }
}
