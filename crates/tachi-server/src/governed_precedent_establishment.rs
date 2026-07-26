//! #1077 PR5 — governed, append-only precedent establishment.
//!
//! `precedent_candidate_ops` deliberately captures only pending candidate
//! signals. This module is the separate mutation boundary: it requires typed
//! outcome and independent-verification evidence to agree with an immutable
//! subject binding, then revalidates a server-verified approval receipt
//! immediately before appending one event. It never edits candidate rows,
//! treats GitHub/wiki/distill/model signals as inert metadata, and has no
//! overturn path (PR6 owns that transition).

use std::collections::BTreeMap;

use memcore::{AuthorityLevel, EffectScope, MemoryEntry, TachiEventQuery, TachiEventRecord};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tachi_params::{
    ApprovalReceiptV1, ApprovalTargetV1, ApproverAuthorityProbe, ApproverAuthorizationPolicyV1,
    CallerAssertedContextV1, CurrentApprovalContextV1, GovernedActionV1, RepoRevisionPinV1,
};

use crate::approver_authority::{
    authorize_governed_mutation, authorize_governed_mutation_with_probe,
    revalidate_governed_mutation, revalidate_governed_mutation_with_probe,
};
use crate::continuity_ops::storage::{insert_event_if_absent, read_events};
use crate::continuity_ops::ContinuityEventTarget;
use crate::utils::stable_hash;
use crate::MemoryServer;

const EVENT_TYPE: &str = "precedent.established.v1";
const EVENT_DOMAIN: &str = "precedent";
const EVENT_ADAPTER: &str = "governed_precedent_establishment_v1";
const EVENT_SCAN_LIMIT: usize = 4096;

/// The immutable tuple every evidence source and approval must bind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ImmutablePrecedentBindingV1 {
    pub repo: String,
    pub packet_id: String,
    pub subject_ref: String,
    pub subject_revision: i64,
    pub principle: String,
    pub proposal_hash: String,
    pub source_bundle_hash: String,
    pub source_snapshot_hashes: Vec<String>,
    pub repo_revision_pins: Vec<RepoRevisionPinV1>,
}

impl ImmutablePrecedentBindingV1 {
    fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("repo", self.repo.as_str()),
            ("packet_id", self.packet_id.as_str()),
            ("subject_ref", self.subject_ref.as_str()),
            ("principle", self.principle.as_str()),
            ("proposal_hash", self.proposal_hash.as_str()),
            ("source_bundle_hash", self.source_bundle_hash.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("immutable precedent binding missing {field}"));
            }
        }
        if self.subject_revision < 0 {
            return Err("immutable precedent binding has negative subject_revision".to_string());
        }
        if self.source_snapshot_hashes.is_empty()
            || self
                .source_snapshot_hashes
                .iter()
                .any(|hash| hash.trim().is_empty())
        {
            return Err("immutable precedent binding requires source snapshot hashes".to_string());
        }
        if self
            .source_snapshot_hashes
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(
                "immutable precedent binding source_snapshot_hashes must be strictly sorted"
                    .to_string(),
            );
        }
        if self.repo_revision_pins.is_empty()
            || self.repo_revision_pins.iter().any(|pin| {
                pin.repo != self.repo
                    || pin.git_ref.trim().is_empty()
                    || pin.commit_sha.trim().is_empty()
            })
        {
            return Err(
                "immutable precedent binding requires non-empty pins for its repository"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn approval_target(&self) -> ApprovalTargetV1 {
        ApprovalTargetV1 {
            repo: self.repo.clone(),
            action: GovernedActionV1::EstablishPrecedent,
            target_ref: format!("{}@revision:{}", self.subject_ref, self.subject_revision),
            packet_id: self.packet_id.clone(),
            proposal_hash: self.proposal_hash.clone(),
            source_bundle_hash: self.source_bundle_hash.clone(),
            source_snapshot_hashes: self.source_snapshot_hashes.clone(),
            repo_revision_pins: self.repo_revision_pins.clone(),
        }
    }
}

/// The only admissible origins for an establishment subject. Closed/merged
/// GitHub state and model agreement are intentionally absent from this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PendingPrecedentOriginV1 {
    Candidate,
    DirectOwnerRuling,
}

/// One principle-level pending subject. `Candidate` is constructed from the
/// #1076 row shape; `DirectOwnerRuling` bypasses model decomposition but must
/// still carry the identical immutable binding and authority gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingPrecedentSubjectV1 {
    pub origin: PendingPrecedentOriginV1,
    pub binding: ImmutablePrecedentBindingV1,
}

impl PendingPrecedentSubjectV1 {
    /// `#[allow(dead_code)]` until PR6 wires this mutation boundary to a tool
    /// surface. The item IS exercised by this module's discrimination tests, so
    /// `--all-targets` sees it as live and `#[expect(dead_code)]` would go red
    /// there as an unfulfilled expectation; only the `--lib` gate (tests are not
    /// roots) reports it. Delete these attributes in the PR that adds the caller.
    #[allow(dead_code)]
    pub(crate) fn from_pending_candidate(
        entry: &MemoryEntry,
        binding: ImmutablePrecedentBindingV1,
    ) -> Result<Self, String> {
        if !entry.path.starts_with("/precedent_candidates/")
            || entry.metadata.get("kind").and_then(Value::as_str) != Some("precedent_candidate")
            || entry
                .metadata
                .get("candidate_status")
                .and_then(Value::as_str)
                != Some("pending")
        {
            return Err(
                "establishment refuses a non-pending #1076 precedent candidate row".to_string(),
            );
        }
        if binding.subject_ref != entry.id || binding.subject_revision != entry.revision {
            return Err(
                "candidate identity/revision differs from the immutable establishment binding"
                    .to_string(),
            );
        }
        if entry.metadata.get("principle").and_then(Value::as_str)
            != Some(binding.principle.as_str())
        {
            return Err(
                "candidate principle differs from the immutable establishment binding".to_string(),
            );
        }
        binding.validate()?;
        Ok(Self {
            origin: PendingPrecedentOriginV1::Candidate,
            binding,
        })
    }

    /// Unreached until PR6 wires the boundary; live under `--all-targets`
    /// via this module's tests, so `allow` not `expect`. See the note above.
    #[allow(dead_code)]
    pub(crate) fn direct_owner_ruling(
        binding: ImmutablePrecedentBindingV1,
    ) -> Result<Self, String> {
        binding.validate()?;
        Ok(Self {
            origin: PendingPrecedentOriginV1::DirectOwnerRuling,
            binding,
        })
    }
}

/// Outcome validation is independent evidence, not the candidate row's
/// descriptive `outcome` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OutcomeValidationEvidenceV1 {
    pub binding: ImmutablePrecedentBindingV1,
    pub validation_receipt_hash: String,
}

/// Independent verification/adjudication is a separate receipt and must name
/// a verifier distinct from the candidate producer. There is deliberately no
/// model-agreement variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IndependentVerificationEvidenceV1 {
    pub binding: ImmutablePrecedentBindingV1,
    pub verifier_ref: String,
    pub producer_ref: String,
    pub verification_receipt_hash: String,
}

/// Candidate signals are retained only so callers can carry their diagnostics
/// without accidentally promoting them into evidence. The gate never reads
/// these fields to decide establishment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CandidateSignalsV1 {
    pub github_closed_or_merged: bool,
    pub wiki_present: bool,
    pub distill_present: bool,
    pub confidence_present: bool,
    pub model_agreement_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GovernedPrecedentEstablishmentInputV1 {
    pub subject: PendingPrecedentSubjectV1,
    pub outcome_validation: OutcomeValidationEvidenceV1,
    pub independent_verification: IndependentVerificationEvidenceV1,
    pub candidate_signals: CandidateSignalsV1,
}

impl GovernedPrecedentEstablishmentInputV1 {
    fn validate_evidence(&self) -> Result<ApprovalTargetV1, String> {
        self.subject.binding.validate()?;
        if self.outcome_validation.binding != self.subject.binding
            || self.independent_verification.binding != self.subject.binding
        {
            return Err(
                "outcome validation and independent verification must match the immutable subject binding"
                    .to_string(),
            );
        }
        if self
            .outcome_validation
            .validation_receipt_hash
            .trim()
            .is_empty()
        {
            return Err("establishment requires outcome validation evidence".to_string());
        }
        if self.independent_verification.verifier_ref.trim().is_empty()
            || self.independent_verification.producer_ref.trim().is_empty()
            || self
                .independent_verification
                .verification_receipt_hash
                .trim()
                .is_empty()
            || self.independent_verification.verifier_ref
                == self.independent_verification.producer_ref
        {
            return Err(
                "establishment requires independent verification/adjudication evidence".to_string(),
            );
        }
        Ok(self.subject.binding.approval_target())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EstablishedPrecedentEventV1 {
    event_version: String,
    establishment_key: String,
    subject: PendingPrecedentSubjectV1,
    outcome_validation: OutcomeValidationEvidenceV1,
    independent_verification: IndependentVerificationEvidenceV1,
    approval_receipt: ApprovalReceiptV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EstablishedPrecedentProjectionV1 {
    pub establishment_key: String,
    pub subject: PendingPrecedentSubjectV1,
    pub verified_approver: String,
    pub approval_receipt_hash: String,
}

/// A deterministic active view derived exclusively from append-only events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActivePrecedentProjectionV1 {
    pub precedents: BTreeMap<String, EstablishedPrecedentProjectionV1>,
}

fn establishment_key(input: &GovernedPrecedentEstablishmentInputV1) -> Result<String, String> {
    serde_json::to_string(&input.subject)
        .map(|serialized| stable_hash(&serialized))
        .map_err(|err| {
            format!("serialize immutable precedent subject for establishment key: {err}")
        })
}

fn event_for(
    input: &GovernedPrecedentEstablishmentInputV1,
    receipt: ApprovalReceiptV1,
) -> Result<(String, TachiEventRecord), String> {
    let establishment_key = establishment_key(input)?;
    let payload = EstablishedPrecedentEventV1 {
        event_version: "governed_precedent_established_v1".to_string(),
        establishment_key: establishment_key.clone(),
        subject: input.subject.clone(),
        outcome_validation: input.outcome_validation.clone(),
        independent_verification: input.independent_verification.clone(),
        approval_receipt: receipt.clone(),
    };
    let payload = serde_json::to_value(payload)
        .map_err(|err| format!("serialize governed precedent event: {err}"))?;
    let event_id = format!("precedent-establishment-{establishment_key}");
    Ok((
        event_id.clone(),
        TachiEventRecord {
            id: event_id,
            source_repo: input.subject.binding.repo.clone(),
            adapter: EVENT_ADAPTER.to_string(),
            project: input.subject.binding.repo.clone(),
            domain: EVENT_DOMAIN.to_string(),
            session_id: String::new(),
            actor: receipt.principal.login,
            event_type: EVENT_TYPE.to_string(),
            authority: AuthorityLevel::ExecutionGate,
            effects: vec![EffectScope::None],
            projection_hints: Vec::new(),
            payload,
            provenance: json!({
                "approval_receipt_hash": receipt.receipt_hash,
                "verified_principal_id": receipt.principal.user_id,
            }),
            created_at: chrono::Utc::now().to_rfc3339(),
        },
    ))
}

fn parse_event(event: &TachiEventRecord) -> Result<EstablishedPrecedentEventV1, String> {
    serde_json::from_value(event.payload.clone()).map_err(|err| {
        format!(
            "governed precedent event {} has an invalid typed payload: {err}",
            event.id
        )
    })
}

fn event_target(server: &MemoryServer) -> ContinuityEventTarget {
    // The event's repository binding is in the immutable payload; the storage
    // target follows the same default-route primitive as other continuity
    // writes so a bound server cannot split ledger and projection reads.
    ContinuityEventTarget::from_default_write(server, None)
}

fn existing_event(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    id: &str,
) -> Result<Option<TachiEventRecord>, String> {
    let events = read_events(
        server,
        target,
        &TachiEventQuery {
            domain: Some(EVENT_DOMAIN.to_string()),
            event_type: Some(EVENT_TYPE.to_string()),
            limit: EVENT_SCAN_LIMIT,
            ..TachiEventQuery::default()
        },
    )?;
    Ok(events.into_iter().find(|event| event.id == id))
}

fn require_same_establishment(
    existing: &TachiEventRecord,
    input: &GovernedPrecedentEstablishmentInputV1,
    key: &str,
) -> Result<(), String> {
    let payload = parse_event(existing)?;
    if payload.event_version != "governed_precedent_established_v1"
        || payload.establishment_key != key
        || payload.subject != input.subject
        || payload.outcome_validation != input.outcome_validation
        || payload.independent_verification != input.independent_verification
    {
        return Err(format!(
            "existing event {} conflicts with the governed establishment being replayed",
            existing.id
        ));
    }
    Ok(())
}

/// Rebuild the active projection from the append-only ledger. It intentionally
/// has no write side effect, so a rebuild can neither establish nor erase a
/// precedent.
pub(crate) fn rebuild_active_precedent_projection(
    server: &MemoryServer,
) -> Result<ActivePrecedentProjectionV1, String> {
    let target = event_target(server);
    let events = read_events(
        server,
        &target,
        &TachiEventQuery {
            domain: Some(EVENT_DOMAIN.to_string()),
            event_type: Some(EVENT_TYPE.to_string()),
            limit: EVENT_SCAN_LIMIT,
            ..TachiEventQuery::default()
        },
    )?;
    let mut projection = ActivePrecedentProjectionV1::default();
    for event in events {
        let payload = parse_event(&event)?;
        payload.subject.binding.validate()?;
        let key = payload.establishment_key.clone();
        let entry = EstablishedPrecedentProjectionV1 {
            establishment_key: key.clone(),
            subject: payload.subject,
            verified_approver: payload.approval_receipt.principal.login,
            approval_receipt_hash: payload.approval_receipt.receipt_hash,
        };
        if let Some(existing) = projection.precedents.insert(key.clone(), entry.clone()) {
            if existing != entry {
                return Err(format!(
                    "append-only precedent ledger has conflicting events for {key}"
                ));
            }
        }
    }
    Ok(projection)
}

fn persist_after_revalidation(
    server: &MemoryServer,
    input: &GovernedPrecedentEstablishmentInputV1,
    receipt: ApprovalReceiptV1,
) -> Result<ActivePrecedentProjectionV1, String> {
    let (event_id, event) = event_for(input, receipt)?;
    let key = establishment_key(input)?;
    let target = event_target(server);
    if let Some(existing) = existing_event(server, &target, &event_id)? {
        require_same_establishment(&existing, input, &key)?;
        return rebuild_active_precedent_projection(server);
    }

    match insert_event_if_absent(server, &target, &event) {
        Ok(_) => rebuild_active_precedent_projection(server),
        Err(error) => {
            // A concurrent replay may have won with a newly-issued receipt.
            // Its immutable establishment must still match exactly; otherwise
            // preserve the ledger's conflict refusal rather than overwriting
            // history or reporting a false idempotent success.
            if let Some(existing) = existing_event(server, &target, &event_id)? {
                require_same_establishment(&existing, input, &key)?;
                rebuild_active_precedent_projection(server)
            } else {
                Err(error)
            }
        }
    }
}

/// Establish with a receipt issued earlier by the existing #1382 authority
/// path. Revalidation is immediately before the only write.
pub(crate) fn establish_precedent_with_receipt(
    server: &MemoryServer,
    policy: &ApproverAuthorizationPolicyV1,
    input: &GovernedPrecedentEstablishmentInputV1,
    receipt: ApprovalReceiptV1,
) -> Result<ActivePrecedentProjectionV1, String> {
    let target = input.validate_evidence()?;
    revalidate_governed_mutation(
        server,
        policy,
        &receipt,
        &CurrentApprovalContextV1 { target },
    )
    .map_err(|denial| format!("precedent establishment refused at revalidation: {denial}"))?;
    persist_after_revalidation(server, input, receipt)
}

/// Unreached until PR6 wires the boundary; live under `--all-targets` via
/// this module's tests, so `allow` not `expect`.
#[allow(dead_code)]
fn establish_precedent_with_receipt_and_probe<P: ApproverAuthorityProbe + ?Sized>(
    server: &MemoryServer,
    probe: &P,
    policy: &ApproverAuthorizationPolicyV1,
    input: &GovernedPrecedentEstablishmentInputV1,
    receipt: ApprovalReceiptV1,
) -> Result<ActivePrecedentProjectionV1, String> {
    let target = input.validate_evidence()?;
    revalidate_governed_mutation_with_probe(
        probe,
        policy,
        &receipt,
        &CurrentApprovalContextV1 { target },
    )
    .map_err(|denial| format!("precedent establishment refused at revalidation: {denial}"))?;
    persist_after_revalidation(server, input, receipt)
}

/// Production convenience boundary: issue a current receipt, then revalidate
/// it immediately before the append-only write. Caller assertions remain
/// descriptive-only inside #1382's resolver.
#[allow(dead_code)]
pub(crate) fn authorize_and_establish_precedent(
    server: &MemoryServer,
    policy: &ApproverAuthorizationPolicyV1,
    input: &GovernedPrecedentEstablishmentInputV1,
    caller_asserted: &CallerAssertedContextV1,
) -> Result<ActivePrecedentProjectionV1, String> {
    let target = input.validate_evidence()?;
    let receipt = authorize_governed_mutation(server, policy, &target, caller_asserted)
        .map_err(|denial| format!("precedent establishment authorization refused: {denial}"))?;
    establish_precedent_with_receipt(server, policy, input, receipt)
}

/// The same production mutation boundary with the existing #1382 probe trait
/// injected for discrimination tests. This is not a second authority engine:
/// both paths call the resolver/revalidator wrappers above.
/// Unreached until PR6 wires the boundary; live under `--all-targets` via
/// this module's tests, so `allow` not `expect`.
#[allow(dead_code)]
fn authorize_and_establish_precedent_with_probe<P: ApproverAuthorityProbe + ?Sized>(
    server: &MemoryServer,
    probe: &P,
    policy: &ApproverAuthorizationPolicyV1,
    input: &GovernedPrecedentEstablishmentInputV1,
    caller_asserted: &CallerAssertedContextV1,
) -> Result<ActivePrecedentProjectionV1, String> {
    let target = input.validate_evidence()?;
    let receipt =
        authorize_governed_mutation_with_probe(probe, policy, &target, caller_asserted)
            .map_err(|denial| format!("precedent establishment authorization refused: {denial}"))?;
    establish_precedent_with_receipt_and_probe(server, probe, policy, input, receipt)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use memcore::MemoryEntry;
    use tachi_params::{
        credential_fingerprint, ApproverAuthorityProbe, AuthorityDenialV1, CredentialContextV1,
        RepoFactsV1, RepoPermissionV1, RepoRevisionV1, TeamMembershipProbeV1, VerifiedPrincipalV1,
    };

    use super::*;

    const REPO: &str = "kckylechen1/tachi";
    const HEAD: &str = "66e5a1add67ef349194738c924d6766caea2099e";

    #[derive(Clone)]
    struct FakeProbe {
        principal: Result<VerifiedPrincipalV1, AuthorityDenialV1>,
        repo: Result<RepoFactsV1, AuthorityDenialV1>,
        revisions: BTreeMap<String, Result<RepoRevisionV1, AuthorityDenialV1>>,
    }

    impl FakeProbe {
        fn owner() -> Self {
            let mut revisions = BTreeMap::new();
            revisions.insert(
                format!("{REPO}@refs/heads/main"),
                Ok(RepoRevisionV1 {
                    repo: REPO.to_string(),
                    git_ref: "refs/heads/main".to_string(),
                    commit_sha: HEAD.to_string(),
                    verified_at: "2026-07-25T00:00:00Z".to_string(),
                }),
            );
            Self {
                principal: Ok(principal("owner", 7)),
                repo: Ok(repo_facts(7)),
                revisions,
            }
        }

        fn non_owner() -> Self {
            Self {
                principal: Ok(principal("forged-owner", 99)),
                repo: Ok(repo_facts(7)),
                revisions: Self::owner().revisions,
            }
        }
    }

    impl ApproverAuthorityProbe for FakeProbe {
        fn authenticated_principal(&self) -> Result<VerifiedPrincipalV1, AuthorityDenialV1> {
            self.principal.clone()
        }

        fn repo_facts(&self, _: &str) -> Result<RepoFactsV1, AuthorityDenialV1> {
            self.repo.clone()
        }

        fn team_membership(
            &self,
            org: &str,
            team_slug: &str,
            login: &str,
        ) -> Result<TeamMembershipProbeV1, AuthorityDenialV1> {
            Ok(TeamMembershipProbeV1::NotMember {
                org: org.to_string(),
                team_slug: team_slug.to_string(),
                login: login.to_string(),
            })
        }

        fn repo_revision(
            &self,
            repo: &str,
            git_ref: &str,
        ) -> Result<RepoRevisionV1, AuthorityDenialV1> {
            self.revisions
                .get(&format!("{repo}@{git_ref}"))
                .cloned()
                .unwrap_or_else(|| {
                    Err(AuthorityDenialV1::AuthorityUnavailable {
                        probe: "fixture revision".to_string(),
                        detail: "unconfigured revision".to_string(),
                    })
                })
        }
    }

    fn principal(login: &str, id: u64) -> VerifiedPrincipalV1 {
        VerifiedPrincipalV1 {
            login: login.to_string(),
            user_id: id,
            node_id: format!("node-{id}"),
            account_type: "User".to_string(),
            credential_context: CredentialContextV1 {
                source: "fixture".to_string(),
                credential_fingerprint: credential_fingerprint("fixture-token"),
            },
            verified_at: "2026-07-25T00:00:00Z".to_string(),
        }
    }

    fn repo_facts(owner_id: u64) -> RepoFactsV1 {
        RepoFactsV1 {
            full_name: REPO.to_string(),
            owner_login: "owner".to_string(),
            owner_id,
            owner_type: "User".to_string(),
            permissions: RepoPermissionV1 {
                admin: true,
                maintain: true,
                push: true,
                triage: true,
                pull: true,
            },
            observed_at: "2026-07-25T00:00:00Z".to_string(),
        }
    }

    fn policy() -> ApproverAuthorizationPolicyV1 {
        ApproverAuthorizationPolicyV1::repository_owner_only("precedent-establishment-test")
    }

    fn binding(subject_ref: &str) -> ImmutablePrecedentBindingV1 {
        ImmutablePrecedentBindingV1 {
            repo: REPO.to_string(),
            packet_id: "packet-1077-pr5".to_string(),
            subject_ref: subject_ref.to_string(),
            subject_revision: 3,
            principle: "verified evidence is required".to_string(),
            proposal_hash: "proposal-sha256".to_string(),
            source_bundle_hash: "source-bundle-sha256".to_string(),
            source_snapshot_hashes: vec!["comment-sha256".to_string(), "issue-sha256".to_string()],
            repo_revision_pins: vec![RepoRevisionPinV1 {
                repo: REPO.to_string(),
                git_ref: "refs/heads/main".to_string(),
                commit_sha: HEAD.to_string(),
            }],
        }
    }

    fn candidate_subject() -> PendingPrecedentSubjectV1 {
        PendingPrecedentSubjectV1 {
            origin: PendingPrecedentOriginV1::Candidate,
            binding: binding("candidate-1077-1"),
        }
    }

    fn valid_input() -> GovernedPrecedentEstablishmentInputV1 {
        let subject = candidate_subject();
        GovernedPrecedentEstablishmentInputV1 {
            outcome_validation: OutcomeValidationEvidenceV1 {
                binding: subject.binding.clone(),
                validation_receipt_hash: "outcome-validation-receipt".to_string(),
            },
            independent_verification: IndependentVerificationEvidenceV1 {
                binding: subject.binding.clone(),
                verifier_ref: "reviewer:independent".to_string(),
                producer_ref: "producer:source".to_string(),
                verification_receipt_hash: "independent-verification-receipt".to_string(),
            },
            subject,
            candidate_signals: CandidateSignalsV1::default(),
        }
    }

    fn forged_context() -> CallerAssertedContextV1 {
        CallerAssertedContextV1 {
            actor: Some("owner".to_string()),
            adjudicator: Some("owner".to_string()),
            agent_identity: Some("agent-owner".to_string()),
            work_claim: Some("claim-owner".to_string()),
            model: Some("model-owner".to_string()),
            seat: Some("seat-owner".to_string()),
            execution_receipt: Some("execution-owner".to_string()),
            delegation_capability: Some("precedent.establish".to_string()),
        }
    }

    fn server() -> (MemoryServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");
        (server, dir)
    }

    fn approved_receipt(
        probe: &FakeProbe,
        input: &GovernedPrecedentEstablishmentInputV1,
    ) -> ApprovalReceiptV1 {
        authorize_governed_mutation_with_probe(
            probe,
            &policy(),
            &input.validate_evidence().expect("valid evidence"),
            &forged_context(),
        )
        .expect("owner receipt")
    }

    fn event_count(server: &MemoryServer) -> usize {
        rebuild_active_precedent_projection(server)
            .expect("projection")
            .precedents
            .len()
    }

    #[test]
    fn missing_outcome_or_independent_evidence_appends_nothing() {
        for missing_outcome in [true, false] {
            let (server, _dir) = server();
            let mut input = valid_input();
            let receipt = approved_receipt(&FakeProbe::owner(), &input);
            if missing_outcome {
                input.outcome_validation.validation_receipt_hash.clear();
            } else {
                input
                    .independent_verification
                    .verification_receipt_hash
                    .clear();
            }
            assert!(establish_precedent_with_receipt_and_probe(
                &server,
                &FakeProbe::owner(),
                &policy(),
                &input,
                receipt,
            )
            .is_err());
            assert_eq!(event_count(&server), 0);
        }
    }

    #[test]
    fn forged_context_cannot_replace_verified_authority() {
        let (server, _dir) = server();
        let result = authorize_and_establish_precedent_with_probe(
            &server,
            &FakeProbe::non_owner(),
            &policy(),
            &valid_input(),
            &forged_context(),
        );
        assert!(result.is_err());
        assert_eq!(event_count(&server), 0);
    }

    #[test]
    fn stale_proposal_source_or_repository_binding_appends_nothing() {
        for stale_axis in 0..3 {
            let (server, _dir) = server();
            let input = valid_input();
            let receipt = approved_receipt(&FakeProbe::owner(), &input);
            let mut stale = input.clone();
            match stale_axis {
                0 => stale.subject.binding.proposal_hash = "stale-proposal".to_string(),
                1 => stale.subject.binding.source_bundle_hash = "stale-source".to_string(),
                _ => {
                    stale.subject.binding.repo_revision_pins[0].commit_sha =
                        "stale-repo".to_string()
                }
            }
            stale.outcome_validation.binding = stale.subject.binding.clone();
            stale.independent_verification.binding = stale.subject.binding.clone();
            assert!(establish_precedent_with_receipt_and_probe(
                &server,
                &FakeProbe::owner(),
                &policy(),
                &stale,
                receipt,
            )
            .is_err());
            assert_eq!(event_count(&server), 0);
        }
    }

    #[test]
    fn valid_receipt_appends_one_event_and_one_projection() {
        let (server, _dir) = server();
        let input = valid_input();
        let receipt = approved_receipt(&FakeProbe::owner(), &input);
        let projection = establish_precedent_with_receipt_and_probe(
            &server,
            &FakeProbe::owner(),
            &policy(),
            &input,
            receipt,
        )
        .expect("established");
        assert_eq!(projection.precedents.len(), 1);
        assert_eq!(event_count(&server), 1);
    }

    #[test]
    fn replay_is_idempotent_with_byte_equivalent_projection() {
        let (server, _dir) = server();
        let input = valid_input();
        let receipt = approved_receipt(&FakeProbe::owner(), &input);
        let first = establish_precedent_with_receipt_and_probe(
            &server,
            &FakeProbe::owner(),
            &policy(),
            &input,
            receipt.clone(),
        )
        .expect("first establishment");
        let replay = establish_precedent_with_receipt_and_probe(
            &server,
            &FakeProbe::owner(),
            &policy(),
            &input,
            receipt,
        )
        .expect("replay");
        assert_eq!(event_count(&server), 1);
        assert_eq!(
            serde_json::to_vec(&first).expect("first projection bytes"),
            serde_json::to_vec(&replay).expect("replay projection bytes")
        );
    }

    #[test]
    fn weak_candidate_signals_are_not_evidence() {
        let (server, _dir) = server();
        let mut input = valid_input();
        input.outcome_validation.validation_receipt_hash.clear();
        input.candidate_signals = CandidateSignalsV1 {
            github_closed_or_merged: true,
            wiki_present: true,
            distill_present: true,
            confidence_present: true,
            model_agreement_present: true,
        };
        let receipt = approved_receipt(&FakeProbe::owner(), &valid_input());
        assert!(establish_precedent_with_receipt_and_probe(
            &server,
            &FakeProbe::owner(),
            &policy(),
            &input,
            receipt,
        )
        .is_err());
        assert_eq!(event_count(&server), 0);
    }

    #[test]
    fn rebuild_is_byte_equivalent_to_the_live_projection() {
        let (server, _dir) = server();
        let input = valid_input();
        let receipt = approved_receipt(&FakeProbe::owner(), &input);
        let established = establish_precedent_with_receipt_and_probe(
            &server,
            &FakeProbe::owner(),
            &policy(),
            &input,
            receipt,
        )
        .expect("established");
        let rebuilt = rebuild_active_precedent_projection(&server).expect("rebuilt");
        assert_eq!(
            serde_json::to_vec(&established).expect("established bytes"),
            serde_json::to_vec(&rebuilt).expect("rebuilt bytes")
        );
    }

    #[test]
    fn direct_typed_owner_ruling_uses_the_same_gate() {
        let (server, _dir) = server();
        let mut input = valid_input();
        input.subject = PendingPrecedentSubjectV1::direct_owner_ruling(binding("owner-ruling-1"))
            .expect("typed owner ruling");
        input.outcome_validation.binding = input.subject.binding.clone();
        input.independent_verification.binding = input.subject.binding.clone();
        let receipt = approved_receipt(&FakeProbe::owner(), &input);
        let projection = establish_precedent_with_receipt_and_probe(
            &server,
            &FakeProbe::owner(),
            &policy(),
            &input,
            receipt,
        )
        .expect("owner ruling established");
        assert_eq!(projection.precedents.len(), 1);
        assert_eq!(event_count(&server), 1);
    }

    #[test]
    fn only_a_pending_candidate_row_can_enter_the_candidate_constructor() {
        let binding = binding("candidate-row");
        let mut entry = MemoryEntry {
            id: "candidate-row".to_string(),
            path: "/precedent_candidates/tachi/candidate-row".to_string(),
            summary: String::new(),
            text: String::new(),
            importance: 0.6,
            timestamp: "2026-07-25T00:00:00Z".to_string(),
            valid_from: "2026-07-25T00:00:00Z".to_string(),
            valid_until: None,
            category: "decision".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "tachi".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 3,
            vector: None,
            retention_policy: None,
            domain: Some("precedent_candidate".to_string()),
            metadata: json!({
                "kind": "precedent_candidate",
                "candidate_status": "pending",
                "principle": "verified evidence is required"
            }),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        assert!(PendingPrecedentSubjectV1::from_pending_candidate(&entry, binding.clone()).is_ok());
        entry.metadata["candidate_status"] = json!("established");
        assert!(PendingPrecedentSubjectV1::from_pending_candidate(&entry, binding).is_err());
    }
}
