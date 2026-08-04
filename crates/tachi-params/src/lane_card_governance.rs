//! Pure, versioned governance artifacts for reviewed lane-card appends (#1306).
//!
//! These types deliberately contain no filesystem, database, network, or identity
//! lookup. Actor identifiers and evidence snapshots are assertions supplied by
//! the caller; orchestration must not describe them as authenticated or live.

use blake2::{Blake2s256, Digest};
use serde::{Deserialize, Serialize};

pub const GOVERNANCE_VERSION: &str = "tachi.cards.governance.v1";
pub const ENTRY_MARKER_PREFIX: &str = "<!-- tachi-lane-card-entry:";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaneAuthority {
    LaneOperationalEvidence,
    EngineeringPrecedent,
    UniversalLaw,
    UserModel,
    Soul,
    Unknown,
}

impl LaneAuthority {
    pub fn reroute(&self) -> Option<&'static str> {
        match self {
            Self::LaneOperationalEvidence => None,
            Self::EngineeringPrecedent => Some("#950"),
            Self::UniversalLaw => Some("#871/#1467"),
            Self::UserModel => Some("#953"),
            Self::Soul => Some("#858"),
            Self::Unknown => Some("refused: authority is unknown"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRelation {
    Supports,
    Contradicts,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Current,
    Corrected,
    Retracted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceSnapshot {
    pub id: String,
    pub subject_role: String,
    pub subject_vendor: String,
    #[serde(default)]
    pub subject_agent: Option<String>,
    pub source_ref: String,
    pub source_kind: String,
    pub immutable_revision: String,
    pub assertion_hash: String,
    pub relation: EvidenceRelation,
    pub state: EvidenceState,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DraftRequest {
    pub schema_version: String,
    pub seat: String,
    pub role: String,
    pub vendor: String,
    #[serde(default)]
    pub agent: Option<String>,
    pub author: String,
    pub observed_at: String,
    pub observed_failure_or_capability: String,
    pub recurrence_context: String,
    pub counter_clause: String,
    pub authority: LaneAuthority,
    pub evidence: Vec<EvidenceSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DraftArtifact {
    /// Complete canonical input. Governance verification re-renders this
    /// packet rather than trusting duplicated, caller-controlled fields.
    pub normalized_request: DraftRequest,
    pub schema_version: String,
    pub seat: String,
    pub role: String,
    pub vendor: String,
    pub agent: Option<String>,
    pub author: String,
    pub evidence_hash: String,
    pub evidence_pins: Vec<EvidenceSnapshot>,
    pub append_markdown: String,
    pub dedupe_key: String,
    pub draft_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub schema_version: String,
    pub draft: DraftArtifact,
    pub reviewer: String,
    pub decision: ReviewDecision,
    pub notes: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Accepted,
    Rejected,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceipt {
    pub schema_version: String,
    pub draft_hash: String,
    pub evidence_hash: String,
    pub reviewer: String,
    pub decision: ReviewDecision,
    pub notes: String,
    pub review_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub schema_version: String,
    pub draft: DraftArtifact,
    pub review: ReviewReceipt,
    pub leader: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalArtifact {
    pub schema_version: String,
    pub draft: DraftArtifact,
    pub review: ReviewReceipt,
    pub leader: String,
    pub decision: String,
    pub source_hash: String,
    pub source_byte_len: u64,
    pub append_offset: u64,
    pub append_bytes: Vec<u8>,
    pub append_bytes_hash: String,
    pub expected_result_hash: String,
    pub approval_hash: String,
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut h = Blake2s256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
pub fn hash_json<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_vec(value)
        .map(|v| hash_bytes(&v))
        .map_err(|e| e.to_string())
}
fn safe_markdown_field(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        Err(format!("{label} must not be empty"))
    } else if value.trim() != value {
        Err(format!("{label} must not have surrounding whitespace"))
    } else if value.chars().any(|c| c.is_ascii_control())
        || value.contains('`')
        || value.contains("<!--")
        || value.contains("-->")
        || value.contains("tachi-lane-card-entry:")
    {
        Err(format!("{label} contains a Markdown structural breaker"))
    } else {
        Ok(())
    }
}
fn safe_seat(seat: &str) -> bool {
    !seat.is_empty()
        && seat != "."
        && seat != ".."
        && !seat.eq_ignore_ascii_case("readme")
        && seat
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
fn matching_subject(e: &EvidenceSnapshot, r: &DraftRequest) -> bool {
    e.subject_role == r.role && e.subject_vendor == r.vendor && e.subject_agent == r.agent
}

fn validate_evidence(evidence: &[EvidenceSnapshot]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    for e in evidence {
        for (label, value) in [
            ("evidence id", &e.id),
            ("evidence subject_role", &e.subject_role),
            ("evidence subject_vendor", &e.subject_vendor),
            ("evidence source_ref", &e.source_ref),
            ("evidence source_kind", &e.source_kind),
            ("evidence immutable_revision", &e.immutable_revision),
            ("evidence assertion_hash", &e.assertion_hash),
        ] {
            safe_markdown_field(label, value)?;
        }
        if let Some(agent) = e.subject_agent.as_deref() {
            safe_markdown_field("evidence subject_agent", agent)?;
        }
        if !ids.insert(&e.id) {
            return Err(format!("duplicate evidence id: {}", e.id));
        }
    }
    Ok(())
}

/// Validate and canonicalize every user-controlled draft field. Evidence is
/// sorted by stable id so hashing, rendering, and dedupe are order-independent.
pub fn normalize_draft_request(mut req: DraftRequest) -> Result<DraftRequest, String> {
    if req.schema_version != GOVERNANCE_VERSION {
        return Err("unsupported governance schema_version".into());
    }
    if !safe_seat(&req.seat) {
        return Err("unsafe seat filename".into());
    }
    if let Some(route) = req.authority.reroute() {
        return Err(format!(
            "authority reroute: {route}; no appendable Markdown produced"
        ));
    }
    for (n, v) in [
        ("author", &req.author),
        ("role", &req.role),
        ("vendor", &req.vendor),
        ("observed_at", &req.observed_at),
        (
            "observed_failure_or_capability",
            &req.observed_failure_or_capability,
        ),
        ("recurrence_context", &req.recurrence_context),
        ("counter_clause", &req.counter_clause),
    ] {
        safe_markdown_field(n, v)?;
    }
    if let Some(agent) = req.agent.as_deref() {
        safe_markdown_field("agent", agent)?;
    }
    // safe_markdown_field rejects CR/LF and controls; keep the explicit
    // atomic-clause invariant visible at this boundary.
    if req.counter_clause.lines().count() != 1 {
        return Err("counter_clause must be exactly one atomic packet-ready clause".into());
    }
    if req.evidence.is_empty() {
        return Err("supporting evidence is required".into());
    }
    validate_evidence(&req.evidence)?;
    if !req
        .evidence
        .iter()
        .any(|e| e.relation == EvidenceRelation::Supports)
    {
        return Err("supporting evidence is required".into());
    }
    if req
        .evidence
        .iter()
        .filter(|e| e.relation == EvidenceRelation::Supports)
        .any(|e| !matching_subject(e, &req))
    {
        return Err("supporting evidence subject does not exactly match role/vendor/agent".into());
    }
    if req
        .evidence
        .iter()
        .any(|e| e.state != EvidenceState::Current)
    {
        return Err("draft evidence must be current".into());
    }
    req.evidence.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(req)
}

/// Canonical pure normalization + rendering entry point.
pub fn draft_lane_card(req: DraftRequest) -> Result<DraftArtifact, String> {
    let req = normalize_draft_request(req)?;
    let evidence_hash = hash_json(&req.evidence)?;
    let dedupe_key = hash_json(&(
        req.seat.clone(),
        req.role.clone(),
        req.vendor.clone(),
        req.agent.clone(),
        evidence_hash.clone(),
        req.counter_clause.clone(),
    ))?;
    let attribution = req
        .evidence
        .iter()
        .map(|e| {
            format!(
                "- `{}` subject=`{}/{}/{}` source={} `{}` @ `{}` assertion=`{}` ({:?}, {:?})",
                e.id,
                e.subject_role,
                e.subject_vendor,
                e.subject_agent.as_deref().unwrap_or("none"),
                e.source_kind,
                e.source_ref,
                e.immutable_revision,
                e.assertion_hash,
                e.relation,
                e.state
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let marker = format!("{ENTRY_MARKER_PREFIX}{dedupe_key} -->");
    let markdown = format!("\n\n## {} reviewed lane evidence\n{}\n- Target: role=`{}` vendor=`{}` agent=`{}`\n- Author (asserted): `{}`\n- Observation: {}\n- Recurrence: {}\n- Evidence and counterevidence attribution:\n{}\n\n### Counter\n- {}\n", req.observed_at, marker, req.role, req.vendor, req.agent.as_deref().unwrap_or("none"), req.author, req.observed_failure_or_capability, req.recurrence_context, attribution, req.counter_clause);
    let mut out = DraftArtifact {
        normalized_request: req.clone(),
        schema_version: GOVERNANCE_VERSION.into(),
        seat: req.seat,
        role: req.role,
        vendor: req.vendor,
        agent: req.agent,
        author: req.author,
        evidence_hash,
        evidence_pins: req.evidence,
        append_markdown: markdown,
        dedupe_key,
        draft_hash: String::new(),
    };
    out.draft_hash = hash_json(&out)?;
    Ok(out)
}

pub fn verify_draft(d: &DraftArtifact) -> Result<(), String> {
    let expected = draft_lane_card(d.normalized_request.clone())?;
    if &expected != d {
        return Err("draft differs from canonical governed rendering".into());
    }
    Ok(())
}
pub fn review_lane_card(req: ReviewRequest) -> Result<ReviewReceipt, String> {
    if req.schema_version != GOVERNANCE_VERSION {
        return Err("unsupported governance schema_version".into());
    }
    verify_draft(&req.draft)?;
    if req.reviewer == req.draft.author {
        return Err("author cannot self-review".into());
    }
    safe_markdown_field("reviewer", &req.reviewer)?;
    let mut out = ReviewReceipt {
        schema_version: GOVERNANCE_VERSION.into(),
        draft_hash: req.draft.draft_hash,
        evidence_hash: req.draft.evidence_hash,
        reviewer: req.reviewer,
        decision: req.decision,
        notes: req.notes,
        review_hash: String::new(),
    };
    out.review_hash = hash_json(&out)?;
    Ok(out)
}
pub fn verify_review(r: &ReviewReceipt) -> Result<(), String> {
    if r.schema_version != GOVERNANCE_VERSION {
        return Err("unsupported review schema_version".into());
    }
    let mut x = r.clone();
    let got = x.review_hash.clone();
    x.review_hash.clear();
    if hash_json(&x)? != got {
        Err("review hash mismatch".into())
    } else {
        Ok(())
    }
}

/// Verify every approval-internal binding that does not require filesystem
/// state. Filesystem orchestration additionally verifies source hash/length.
pub fn verify_approval(a: &ApprovalArtifact) -> Result<(), String> {
    if a.schema_version != GOVERNANCE_VERSION || a.decision != "approved" {
        return Err("invalid approval decision/version".into());
    }
    verify_draft(&a.draft)?;
    verify_review(&a.review)?;
    if a.review.decision != ReviewDecision::Accepted
        || a.review.draft_hash != a.draft.draft_hash
        || a.review.evidence_hash != a.draft.evidence_hash
    {
        return Err("approval review is not accepted and bound to its draft/evidence".into());
    }
    for (label, actor) in [
        ("author", &a.draft.author),
        ("reviewer", &a.review.reviewer),
        ("leader", &a.leader),
    ] {
        safe_markdown_field(label, actor)?;
    }
    if a.draft.author == a.review.reviewer
        || a.leader == a.draft.author
        || a.leader == a.review.reviewer
    {
        return Err("author, reviewer, and leader must be three distinct asserted actors".into());
    }
    if a.source_byte_len != a.append_offset {
        return Err("append offset must equal source byte length".into());
    }
    if a.append_bytes != a.draft.append_markdown.as_bytes()
        || hash_bytes(&a.append_bytes) != a.append_bytes_hash
    {
        return Err("approval append bytes do not equal the canonical draft".into());
    }
    let mut unhashed = a.clone();
    let claimed = std::mem::take(&mut unhashed.approval_hash);
    if hash_json(&unhashed)? != claimed {
        return Err("approval hash mismatch".into());
    }
    Ok(())
}

pub fn validate_fresh_evidence(
    pins: &[EvidenceSnapshot],
    fresh: &[EvidenceSnapshot],
) -> Result<(), String> {
    validate_evidence(pins)?;
    validate_evidence(fresh)?;
    for pin in pins {
        let now = fresh
            .iter()
            .find(|e| e.id == pin.id)
            .ok_or_else(|| format!("evidence {} is missing/unavailable", pin.id))?;
        if now.state != EvidenceState::Current {
            return Err(format!("evidence {} is corrected or retracted", pin.id));
        }
        if now != pin {
            return Err(format!("evidence {} drifted", pin.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> DraftRequest {
        DraftRequest {
            schema_version: GOVERNANCE_VERSION.into(),
            seat: "codex-review".into(),
            role: "reviewer".into(),
            vendor: "openai".into(),
            agent: Some("codex".into()),
            author: "author-a".into(),
            observed_at: "2026-07-19".into(),
            observed_failure_or_capability: "Missed a stale-source guard.".into(),
            recurrence_context: "Observed in two reviewed runs.".into(),
            counter_clause: "Recompute the source hash immediately before write.".into(),
            authority: LaneAuthority::LaneOperationalEvidence,
            evidence: vec![
                EvidenceSnapshot {
                    id: "ev-1".into(),
                    subject_role: "reviewer".into(),
                    subject_vendor: "openai".into(),
                    subject_agent: Some("codex".into()),
                    source_ref: "run/1".into(),
                    source_kind: "dispatch_run".into(),
                    immutable_revision: "sha256:1".into(),
                    assertion_hash: "b2:1".into(),
                    relation: EvidenceRelation::Supports,
                    state: EvidenceState::Current,
                },
                EvidenceSnapshot {
                    id: "ev-2".into(),
                    subject_role: "other".into(),
                    subject_vendor: "other".into(),
                    subject_agent: None,
                    source_ref: "run/2".into(),
                    source_kind: "dispatch_run".into(),
                    immutable_revision: "sha256:2".into(),
                    assertion_hash: "b2:2".into(),
                    relation: EvidenceRelation::Contradicts,
                    state: EvidenceState::Current,
                },
            ],
        }
    }
    #[test]
    fn draft_is_deterministic_atomic_and_preserves_counterevidence() {
        let a = draft_lane_card(request()).unwrap();
        let b = draft_lane_card(request()).unwrap();
        assert_eq!(a, b);
        assert!(a.append_markdown.contains("ev-2"));
        assert!(a.append_markdown.contains("### Counter"));
    }
    #[test]
    fn authority_reroutes_never_render_markdown() {
        for (a, r) in [
            (LaneAuthority::EngineeringPrecedent, "#950"),
            (LaneAuthority::UniversalLaw, "#871/#1467"),
            (LaneAuthority::UserModel, "#953"),
            (LaneAuthority::Soul, "#858"),
            (LaneAuthority::Unknown, "unknown"),
        ] {
            let mut q = request();
            q.authority = a;
            let e = draft_lane_card(q).unwrap_err();
            assert!(e.contains(r), "{e}");
            assert!(e.contains("no appendable Markdown"));
        }
    }
    #[test]
    fn supporting_subject_mismatch_fails_closed() {
        let mut q = request();
        q.evidence[0].subject_vendor = "wrong".into();
        assert!(draft_lane_card(q).unwrap_err().contains("subject"));
    }
    #[test]
    fn self_review_and_unaccepted_chain_are_rejected() {
        let d = draft_lane_card(request()).unwrap();
        let self_review = ReviewRequest {
            schema_version: GOVERNANCE_VERSION.into(),
            draft: d.clone(),
            reviewer: d.author.clone(),
            decision: ReviewDecision::Accepted,
            notes: String::new(),
        };
        assert!(review_lane_card(self_review).is_err());
        let rejected = review_lane_card(ReviewRequest {
            schema_version: GOVERNANCE_VERSION.into(),
            draft: d,
            reviewer: "reviewer-b".into(),
            decision: ReviewDecision::Rejected,
            notes: String::new(),
        })
        .unwrap();
        assert_eq!(rejected.decision, ReviewDecision::Rejected);
    }
    #[test]
    fn fresh_evidence_rejects_missing_corrected_retracted_and_drift() {
        let pins = request().evidence;
        assert!(validate_fresh_evidence(&pins, &pins).is_ok());
        assert!(validate_fresh_evidence(&pins, &pins[..1]).is_err());
        for state in [EvidenceState::Corrected, EvidenceState::Retracted] {
            let mut f = pins.clone();
            f[0].state = state;
            assert!(validate_fresh_evidence(&pins, &f).is_err());
        }
        let mut drift = pins.clone();
        drift[0].assertion_hash = "changed".into();
        assert!(validate_fresh_evidence(&pins, &drift).is_err());
    }

    #[test]
    fn markdown_injection_and_actor_aliases_are_rejected() {
        for bad in [
            "observation\n### Counter\n- injected",
            "observation `breakout`",
            "observation <!-- hidden -->",
        ] {
            let mut q = request();
            q.observed_failure_or_capability = bad.into();
            assert!(draft_lane_card(q).is_err(), "accepted {bad:?}");
        }
        let mut q = request();
        q.author = " author-a".into();
        assert!(draft_lane_card(q).is_err());
    }

    #[test]
    fn evidence_order_is_canonical_and_forged_unsafe_draft_is_rejected() {
        let a = draft_lane_card(request()).unwrap();
        let mut reordered = request();
        reordered.evidence.reverse();
        let b = draft_lane_card(reordered).unwrap();
        assert_eq!(a, b);

        let mut forged = a;
        forged.normalized_request.author = " forged".into();
        forged.author = forged.normalized_request.author.clone();
        forged.draft_hash.clear();
        forged.draft_hash = hash_json(&forged).unwrap();
        assert!(verify_draft(&forged).is_err());
    }
}
