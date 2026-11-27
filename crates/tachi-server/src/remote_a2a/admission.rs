use super::config::{public_key_digest, GatewayConfig, Peer};
use crate::verified_admission::{
    admit_verified_agent_connection, require_current_verified_admission, revoke_verified_admission,
    TrustedAdmissionVerifier, TrustedRevocationVerifier, VerifiedAdmissionEvidence,
    VerifiedAdmissionRequest, VerifiedEvidenceRef, VerifiedEvidenceState,
    VerifiedRevocationEvidence,
};
use crate::MemoryServer;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::server::danger::ClientCertVerifier;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub(super) struct Connection {
    pub server: MemoryServer,
    pub peer: Peer,
    pub config: Arc<GatewayConfig>,
    pub gateway_actor: memcore::db::a2a::A2aTransitionActor,
    certificates: Vec<CertificateDer<'static>>,
    verifier: Arc<dyn ClientCertVerifier>,
}

struct TlsProof(VerifiedAdmissionEvidence);

impl TrustedAdmissionVerifier for TlsProof {
    fn verify(
        &self,
        _request: &VerifiedAdmissionRequest,
    ) -> Result<VerifiedAdmissionEvidence, String> {
        Ok(self.0.clone())
    }
}

/// Why a connection was not admitted. `key` carries the first 16 hex digits
/// of the presented public-key digest when a certificate parsed, so an
/// operator can recognize or enroll the peer. Nothing else about the peer or
/// its request is kept for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rejection {
    pub reason: String,
    pub key: String,
}

impl From<&'static str> for Rejection {
    fn from(reason: &'static str) -> Self {
        Self {
            reason: reason.into(),
            key: String::new(),
        }
    }
}

/// Facts of one completed TLS handshake. Reading them touches neither the
/// store nor SQLite, so it stays on the async worker.
pub(super) struct TlsEvidence {
    /// SHA-256 of the enrolled SPKI, as written in the operator configuration.
    key: String,
    certificates: Vec<CertificateDer<'static>>,
    connection_id: String,
    exporter: [u8; 32],
}

impl Connection {
    // Only a completed rustls handshake supplies this proof. No HTTP header,
    // JSON field or local MCP route can construct a TlsProof.
    pub fn evidence(
        config: &GatewayConfig,
        tls: &rustls::ServerConnection,
    ) -> Result<(Peer, TlsEvidence), Rejection> {
        if tls.is_handshaking() {
            return Err("remote TLS handshake is incomplete".into());
        }
        let certificates = tls
            .peer_certificates()
            .ok_or_else(|| Rejection::from("remote peer certificate required"))?
            .to_vec();
        let leaf = certificates
            .first()
            .ok_or_else(|| Rejection::from("remote peer certificate required"))?;
        let key = public_key_digest(leaf).map_err(|_| "remote peer certificate is invalid")?;
        let peer = config
            .peers
            .iter()
            .find(|peer| peer.public_key_sha256 == key)
            .ok_or(Rejection {
                reason: "remote peer is not enrolled".into(),
                key: key.clone(),
            })?
            .clone();
        let connection_id = format!("remote-{}", uuid::Uuid::new_v4());
        let exporter = tls
            .export_keying_material(
                [0_u8; 32],
                b"EXPORTER-Tachi-Admission-v1",
                Some(connection_id.as_bytes()),
            )
            .map_err(|_| "remote TLS channel binding is unavailable")?;
        Ok((
            peer,
            TlsEvidence {
                key,
                certificates,
                connection_id,
                exporter,
            },
        ))
    }

    /// Store side of admission: the certificate-window check and the
    /// synchronous SQLite admission write. Run it on the blocking pool, never
    /// on a Tokio worker.
    pub fn admit(
        base: MemoryServer,
        config: Arc<GatewayConfig>,
        peer: Peer,
        evidence: TlsEvidence,
        verifier: Arc<dyn ClientCertVerifier>,
    ) -> Result<Self, Rejection> {
        let TlsEvidence {
            key,
            certificates,
            connection_id,
            exporter,
        } = evidence;
        let leaf = certificates
            .first()
            .ok_or_else(|| Rejection::from("remote peer certificate required"))?;
        let (Some(gateway_id), gateway_connection, state) = base
            .work_claim_connection()
            .ok_or("gateway local admission is unavailable")?
        else {
            return Err("gateway local admission is unavailable".into());
        };
        if gateway_id != config.agent_identity_id || state != "self_asserted" {
            return Err("gateway local admission does not match its operator identity".into());
        }
        let now = jiff::Timestamp::now();
        let expires_at = now + jiff::SignedDuration::from_mins(15);
        let end_seconds: u64 = expires_at
            .as_second()
            .try_into()
            .map_err(|_| "remote admission clock is invalid")?;
        // Downstream receipt consumers must not outlive the certificate chain,
        // even if they are not themselves handling the original TLS stream.
        verifier
            .verify_client_cert(
                leaf,
                &certificates[1..],
                UnixTime::since_unix_epoch(std::time::Duration::from_secs(end_seconds + 1)),
            )
            .map_err(|_| "remote certificate does not cover the admission window")?;
        let evidence = VerifiedAdmissionEvidence {
            agent_identity_id: peer.agent_identity_id.clone(),
            connection_id: connection_id.clone(),
            issuer_id: config.issuer_id.clone(),
            verification_method: memcore::VERIFIED_ADMISSION_MTLS_METHOD.into(),
            verification_version: memcore::VERIFIED_ADMISSION_VERSION.into(),
            trust_domain: config.trust_domain.clone(),
            verification_scope: memcore::VERIFIED_ADMISSION_SCOPE.into(),
            evidence_digest: format!(
                "{:x}",
                Sha256::digest(
                    [
                        leaf.as_ref(),
                        exporter.as_slice(),
                        peer.host_id.as_bytes(),
                        config.host_id.as_bytes()
                    ]
                    .concat()
                )
            ),
            evidence_ref: VerifiedEvidenceRef::attestation("mtls-peer", &key)
                .map_err(|_| Rejection::from("remote peer certificate is invalid"))?,
            issued_at: now,
            expires_at,
            nonce: uuid::Uuid::new_v4().to_string(),
            state: VerifiedEvidenceState::Active,
        };
        let request = VerifiedAdmissionRequest {
            agent_identity_id: peer.agent_identity_id.clone(),
            connection_id,
            expected_issuer_id: config.issuer_id.clone(),
            expected_verification_method: evidence.verification_method.clone(),
            expected_verification_version: evidence.verification_version.clone(),
            trust_domain: config.trust_domain.clone(),
            verification_scope: evidence.verification_scope.clone(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        };
        let server = base.clone_for_mcp_session();
        admit_verified_agent_connection(&server, &request, &TlsProof(evidence)).map_err(
            |_message| {
                // Keep log/rejection text static so dynamic SQLite errors from
                // verified_admission never leak into operational logs.
                Rejection::from("remote admission was refused by verified admission store")
            },
        )?;
        Ok(Self {
            server,
            peer,
            config,
            gateway_actor: memcore::db::a2a::A2aTransitionActor {
                agent_identity_id: gateway_id,
                connection_id: gateway_connection,
            },
            certificates,
            verifier,
        })
    }

    pub async fn from_tls(
        base: &MemoryServer,
        config: Arc<GatewayConfig>,
        tls: &rustls::ServerConnection,
        verifier: Arc<dyn ClientCertVerifier>,
    ) -> Result<Self, Rejection> {
        let (peer, evidence) = Self::evidence(&config, tls)?;
        let base = base.clone();
        tokio::task::spawn_blocking(move || Self::admit(base, config, peer, evidence, verifier))
            .await
            .map_err(|_| Rejection::from("remote admission task failed"))?
    }

    pub fn current(&self) -> Result<(), String> {
        self.verifier
            .verify_client_cert(
                &self.certificates[0],
                &self.certificates[1..],
                UnixTime::now(),
            )
            .map_err(|_| "remote peer certificate is no longer valid")?;
        require_current_verified_admission(&self.server)
    }
}

struct OperatorRevocation(VerifiedRevocationEvidence);

impl TrustedRevocationVerifier for OperatorRevocation {
    fn verify_revocation(&self, _admission_id: &str) -> Result<VerifiedRevocationEvidence, String> {
        Ok(self.0.clone())
    }
}

pub(super) fn revoke(
    server: &MemoryServer,
    config: &GatewayConfig,
    admission: &str,
) -> Result<(), String> {
    let receipt = server
        .with_global_store_read(|store| {
            memcore::get_verified_admission_receipt(store.connection(), admission)
                .map_err(|e| e.to_string())
        })?
        .ok_or("remote admission does not exist")?;
    if receipt.verification_method != memcore::VERIFIED_ADMISSION_MTLS_METHOD
        || receipt.issuer_id != config.issuer_id
        || receipt.trust_domain != config.trust_domain
    {
        return Err("remote admission is outside this gateway authority".into());
    }
    let nonce = uuid::Uuid::new_v4().to_string();
    let evidence = VerifiedRevocationEvidence {
        admission_id: admission.into(),
        issuer_id: config.issuer_id.clone(),
        evidence_digest: memcore::canonical_json_digest_hex(
            &serde_json::json!({"admission": admission, "peer_key": receipt.evidence_ref, "nonce": nonce}),
        ),
        evidence_ref: VerifiedEvidenceRef::attestation("mtls-revocation", &nonce)?,
        nonce,
        revoked_at: jiff::Timestamp::now(),
    };
    revoke_verified_admission(server, admission, &OperatorRevocation(evidence))
}
