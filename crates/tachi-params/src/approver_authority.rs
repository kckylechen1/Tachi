//! #1382 — server-verified human approver authority for governed apply gates.
//!
//! Frozen invariant (issue #1382): *a caller-supplied name, model or seat,
//! `AgentIdentity`, personality projection, `WorkClaim`, execution receipt,
//! or delegation capability does **not** establish human owner/delegated-
//! approver authority.* Owner-ratified trust root (#1382 comment
//! 2026-07-23): the principal authenticated by the active GitHub credential,
//! verified through the GitHub API; authority is repository ownership or
//! membership in an owner-authorized GitHub organization team holding the
//! required **live** repository permission; delegation and revocation are
//! read from live permission and team-membership queries only.
//!
//! ## Why the whole decision lives in this crate
//!
//! Every authorization branch here is pure: it consumes *probe results*, not
//! I/O. The only way to feed it facts is [`ApproverAuthorityProbe`], whose
//! every method returns `Result<_, AuthorityDenialV1>` — an implementation
//! physically cannot report "GitHub was unreachable" as anything but a
//! denial, so a network error, timeout, or rate limit can never be widened
//! into a pass by a lossy `unwrap_or_default()` at a call site. The live
//! GitHub implementation is `tachi_server::approver_authority`.
//!
//! ## Structural fail-closed properties (each is testable, none is prose)
//!
//! 1. **No default-allow branch.** Every `match` that decides authority
//!    enumerates its allowed cases and ends in an explicit denial; there is
//!    no `_ => true` and no `else { true }` (the #919 fail-open lesson).
//! 2. **No lexicographic time comparison.** Every timestamp is parsed with
//!    [`chrono::DateTime::parse_from_rfc3339`] into a `DateTime<Utc>` before
//!    any ordering test, and an unparseable timestamp denies rather than
//!    falling back to string order. Formats: RFC 3339 with offset, semantic
//!    layer: instant on the UTC timeline, failure direction: deny.
//! 3. **Caller assertions are structurally inert.** Everything forgeable is
//!    corralled into [`CallerAssertedContextV1`], which is never passed to
//!    any evaluation function — it is only carried on the receipt (and into
//!    the receipt hash, so it cannot be swapped after issuance) as
//!    description.
//! 4. **One-inhabitant decision.** [`ApprovalDecisionV1`] has exactly one
//!    constructible variant, `Approved` — a receipt can never assert
//!    anything else, so no code path can misread a denial as a receipt (the
//!    same discipline `lesson_forge::LessonCandidateStatusV1` already uses).
//! 5. **Permission floor is a type, not a config value.**
//!    [`RepoPermissionLevelV1`] has no `Pull`/`Triage` variant, so no policy
//!    can configure a governed mutation to be authorized by read access.
//! 6. **No credential value is ever held here.** The only credential-derived
//!    datum is a domain-separated SHA-256 fingerprint used exclusively for
//!    change detection ([`CredentialContextV1::credential_fingerprint`]).
//!
//! ## Scope boundary
//!
//! This module is **not** a general identity framework. It answers exactly
//! one question — "may this GitHub-authenticated human approve *this*
//! governed precedent mutation, right now?" — and it adds no credential
//! store, no carrier/process control, and no session concept. #748 keeps
//! credential authority; #1077 owns the establishment/overturn transition
//! that calls this gate.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::refinery::{canonical_json_sha256, sha256_hex, RepoRevisionV1};

/// Hard ceiling on how long an owner may configure an approval receipt to
/// stay usable. A policy asking for more is rejected as unusable rather
/// than silently clamped — a mis-set ceiling must be loud, not quietly
/// tightened into something the owner did not ask for.
pub const MAX_RECEIPT_AGE_CEILING_SECS: i64 = 3_600;

/// Hard ceiling on tolerated clock skew for a receipt timestamp that sits in
/// the future relative to the revalidating server.
pub const MAX_FUTURE_SKEW_CEILING_SECS: i64 = 120;

/// Domain separator for the credential fingerprint, so the digest that
/// leaves this module is not a bare hash of a bearer token reusable as a
/// cross-system correlator.
const CREDENTIAL_FINGERPRINT_DOMAIN: &str =
    "tachi/approver-authority/credential-fingerprint/v1\u{0}";

/// One-way, non-reversible fingerprint of a GitHub credential, used *only*
/// to detect that the credential backing an approval changed between
/// issuance and apply. The credential value itself never enters this crate's
/// types and is never returned by this function.
pub fn credential_fingerprint(credential: &str) -> String {
    let mut basis = String::with_capacity(CREDENTIAL_FINGERPRINT_DOMAIN.len() + credential.len());
    basis.push_str(CREDENTIAL_FINGERPRINT_DOMAIN);
    basis.push_str(credential);
    sha256_hex(basis.as_bytes())
}

// ─── typed loud denial ──────────────────────────────────────────────────────

/// The single denial type. Every failure mode — unreachable GitHub, an
/// unauthorized principal, a drifted revision, an expired receipt — is one
/// of these, and each one carries a named reason. There is deliberately no
/// `Ok(false)`, no `Option::None` denial, and no boolean anywhere in the
/// authority path: a silent no-op is treated as a worse outcome than either
/// a refusal or a pass, so refusal is always a value someone must handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "denial", rename_all = "snake_case")]
pub enum AuthorityDenialV1 {
    /// A GitHub probe could not be completed: network failure, timeout,
    /// rate limit, 5xx, missing/expired credential, or an unparseable
    /// response body. **Never** a pass, and never downgraded to "the user
    /// simply isn't a member".
    AuthorityUnavailable { probe: String, detail: String },
    /// The active credential authenticates something that is not a human
    /// GitHub user account (a GitHub App installation, a bot account, an
    /// unknown account type).
    PrincipalNotHuman { login: String, account_type: String },
    /// The credential authenticated a real human, but that human is neither
    /// the repository owner nor a member of any owner-authorized team.
    NotAuthorized {
        login: String,
        repo: String,
        detail: String,
    },
    /// Live repository permission for the authenticated principal is below
    /// the policy floor.
    InsufficientRepoPermission {
        login: String,
        repo: String,
        required: RepoPermissionLevelV1,
        observed: RepoPermissionV1,
    },
    /// The principal appears on an authorized team but the membership is not
    /// usable: a pending invitation, a removed member, or a role below what
    /// the policy requires.
    TeamMembershipNotUsable {
        org: String,
        team_slug: String,
        login: String,
        state: String,
        role: String,
        detail: String,
    },
    /// The repository the probe answered for is not the repository the
    /// approval targets (a rename/redirect, or a caller-supplied repo string
    /// that GitHub resolved elsewhere).
    RepositoryMismatch { expected: String, actual: String },
    /// The authorization policy itself cannot be used.
    PolicyUnusable { detail: String },
    /// The verified principal changed between issuance and revalidation.
    PrincipalChanged { detail: String },
    /// The credential backing the approval changed between issuance and
    /// revalidation. The detail never contains a credential value or a
    /// fingerprint — only the fact of the change.
    CredentialContextChanged,
    /// The owner-authorized policy changed between issuance and
    /// revalidation, so the receipt was issued under rules that no longer
    /// hold.
    AuthorizationRevisionChanged { expected: String, actual: String },
    /// Live permission/team evidence no longer matches what the receipt
    /// bound — this is how revocation takes effect.
    AuthorityEvidenceChanged { detail: String },
    /// A pinned proposal/source/repository revision drifted, or was never
    /// pinned at all.
    RevisionDrift { reasons: Vec<RevisionDriftV1> },
    /// A timestamp could not be parsed as RFC 3339. Denies — this is the
    /// branch that exists so no code path ever falls back to comparing
    /// timestamp strings lexicographically.
    TimestampUnparseable { field: String, value: String },
    /// The receipt is older than the policy allows.
    ReceiptExpired {
        checked_at: String,
        now: String,
        age_secs: i64,
        max_age_secs: i64,
    },
    /// The receipt claims to have been checked in the future beyond tolerated
    /// clock skew.
    ReceiptFromFuture {
        checked_at: String,
        now: String,
        ahead_secs: i64,
        tolerance_secs: i64,
    },
    /// The receipt is bound to a different target/action than the one being
    /// applied.
    TargetMismatch { detail: String },
    /// The receipt's own hash does not recompute — the stored receipt was
    /// edited after issuance.
    ReceiptTampered { detail: String },
    /// A hashing/serialization failure while computing a binding. Denies.
    BindingComputationFailed { detail: String },
}

impl AuthorityDenialV1 {
    /// Stable machine-readable discriminator for logs/ledgers.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AuthorityUnavailable { .. } => "authority_unavailable",
            Self::PrincipalNotHuman { .. } => "principal_not_human",
            Self::NotAuthorized { .. } => "not_authorized",
            Self::InsufficientRepoPermission { .. } => "insufficient_repo_permission",
            Self::TeamMembershipNotUsable { .. } => "team_membership_not_usable",
            Self::RepositoryMismatch { .. } => "repository_mismatch",
            Self::PolicyUnusable { .. } => "policy_unusable",
            Self::PrincipalChanged { .. } => "principal_changed",
            Self::CredentialContextChanged => "credential_context_changed",
            Self::AuthorizationRevisionChanged { .. } => "authorization_revision_changed",
            Self::AuthorityEvidenceChanged { .. } => "authority_evidence_changed",
            Self::RevisionDrift { .. } => "revision_drift",
            Self::TimestampUnparseable { .. } => "timestamp_unparseable",
            Self::ReceiptExpired { .. } => "receipt_expired",
            Self::ReceiptFromFuture { .. } => "receipt_from_future",
            Self::TargetMismatch { .. } => "target_mismatch",
            Self::ReceiptTampered { .. } => "receipt_tampered",
            Self::BindingComputationFailed { .. } => "binding_computation_failed",
        }
    }

    /// True when the denial is caused by GitHub being unreachable/unusable
    /// rather than by a decided negative answer. Callers use this to report
    /// "refused because evidence was unavailable" distinctly from "refused
    /// because the answer was no" — both refuse; neither is silent.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::AuthorityUnavailable { .. })
    }
}

impl fmt::Display for AuthorityDenialV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorityUnavailable { probe, detail } => write!(
                f,
                "approval refused: GitHub authority probe '{probe}' could not be completed \
                 ({detail}); unavailable evidence is never treated as approval"
            ),
            Self::PrincipalNotHuman {
                login,
                account_type,
            } => write!(
                f,
                "approval refused: the active GitHub credential authenticates '{login}' of type \
                 '{account_type}', which is not a human user account"
            ),
            Self::NotAuthorized {
                login,
                repo,
                detail,
            } => write!(
                f,
                "approval refused: verified principal '{login}' holds no approval authority over \
                 '{repo}' ({detail})"
            ),
            Self::InsufficientRepoPermission {
                login,
                repo,
                required,
                observed,
            } => write!(
                f,
                "approval refused: verified principal '{login}' has live permission {observed} on \
                 '{repo}', below the required floor '{}'",
                required.as_str()
            ),
            Self::TeamMembershipNotUsable {
                org,
                team_slug,
                login,
                state,
                role,
                detail,
            } => write!(
                f,
                "approval refused: '{login}' membership in {org}/{team_slug} is unusable \
                 (state={state}, role={role}): {detail}"
            ),
            Self::RepositoryMismatch { expected, actual } => write!(
                f,
                "approval refused: authority was probed for repository '{actual}' but the \
                 approval targets '{expected}'"
            ),
            Self::PolicyUnusable { detail } => write!(
                f,
                "approval refused: the owner-authorized approver policy is unusable ({detail})"
            ),
            Self::PrincipalChanged { detail } => write!(
                f,
                "apply refused: the verified principal changed since the receipt was issued \
                 ({detail})"
            ),
            Self::CredentialContextChanged => write!(
                f,
                "apply refused: the GitHub credential backing this approval changed since the \
                 receipt was issued"
            ),
            Self::AuthorizationRevisionChanged { expected, actual } => write!(
                f,
                "apply refused: the owner-authorized approver policy changed since the receipt \
                 was issued (authorization_revision {expected} -> {actual})"
            ),
            Self::AuthorityEvidenceChanged { detail } => write!(
                f,
                "apply refused: live permission/team evidence no longer matches the receipt \
                 ({detail})"
            ),
            Self::RevisionDrift { reasons } => {
                write!(f, "apply refused: pinned revisions are no longer current (")?;
                for (i, reason) in reasons.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{reason}")?;
                }
                write!(f, ")")
            }
            Self::TimestampUnparseable { field, value } => write!(
                f,
                "refused: field '{field}' value '{value}' is not an RFC 3339 timestamp; this gate \
                 never falls back to comparing timestamp strings"
            ),
            Self::ReceiptExpired {
                checked_at,
                now,
                age_secs,
                max_age_secs,
            } => write!(
                f,
                "apply refused: approval receipt checked at {checked_at} is {age_secs}s old at \
                 {now}, past the {max_age_secs}s policy limit"
            ),
            Self::ReceiptFromFuture {
                checked_at,
                now,
                ahead_secs,
                tolerance_secs,
            } => write!(
                f,
                "apply refused: approval receipt claims checked_at {checked_at}, {ahead_secs}s \
                 ahead of {now}, beyond the {tolerance_secs}s skew tolerance"
            ),
            Self::TargetMismatch { detail } => write!(
                f,
                "apply refused: the approval receipt is bound to a different target/action \
                 ({detail})"
            ),
            Self::ReceiptTampered { detail } => write!(
                f,
                "apply refused: the approval receipt does not recompute to its recorded hash \
                 ({detail})"
            ),
            Self::BindingComputationFailed { detail } => {
                write!(
                    f,
                    "refused: could not compute an approval binding ({detail})"
                )
            }
        }
    }
}

impl std::error::Error for AuthorityDenialV1 {}

/// One concrete way a pinned revision stopped matching reality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RevisionDriftV1 {
    /// The proposal payload changed since approval.
    ProposalHashChanged { expected: String, actual: String },
    /// The immutable source bundle changed since approval.
    SourceBundleHashChanged { expected: String, actual: String },
    /// The pinned source snapshot set changed since approval.
    SourceSnapshotsChanged {
        expected: Vec<String>,
        actual: Vec<String>,
    },
    /// A pinned repository revision no longer resolves to the same commit.
    RepoRevisionChanged {
        repo: String,
        git_ref: String,
        expected_commit_sha: String,
        actual_commit_sha: Option<String>,
    },
    /// Nothing was ever pinned on the repository axis. Borrowed verbatim
    /// from `refinery::StalenessReasonV1::RepoRevisionUnavailable`: an empty
    /// axis means nobody verified it, not that it is unchanged, so it must
    /// not vacuously pass.
    RepoRevisionUnpinned { detail: String },
}

impl fmt::Display for RevisionDriftV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProposalHashChanged { expected, actual } => {
                write!(f, "proposal_hash {expected} -> {actual}")
            }
            Self::SourceBundleHashChanged { expected, actual } => {
                write!(f, "source_bundle_hash {expected} -> {actual}")
            }
            Self::SourceSnapshotsChanged { expected, actual } => {
                write!(f, "source snapshots {expected:?} -> {actual:?}")
            }
            Self::RepoRevisionChanged {
                repo,
                git_ref,
                expected_commit_sha,
                actual_commit_sha,
            } => write!(
                f,
                "{repo}@{git_ref} {expected_commit_sha} -> {}",
                actual_commit_sha.as_deref().unwrap_or("<unresolved>")
            ),
            Self::RepoRevisionUnpinned { detail } => write!(f, "{detail}"),
        }
    }
}

// ─── verified principal + credential context ────────────────────────────────

/// The non-secret description of *which credential* authenticated. Bound
/// into the receipt so a credential swap between issuance and apply is a
/// detectable change rather than an invisible one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialContextV1 {
    /// Where the credential came from, as a non-secret label
    /// (e.g. `"vault:GH_TOKEN"`, `"env:GH_TOKEN"`). Never the value.
    pub source: String,
    /// [`credential_fingerprint`] of the credential. One-way; used only for
    /// equality, never for reconstruction, never printed in a denial.
    pub credential_fingerprint: String,
}

/// A principal the GitHub API confirmed the active credential authenticates.
///
/// `login` is included because humans read it, but it is **not** the
/// identity: GitHub logins are renameable and re-registerable, so
/// `user_id`/`node_id` are the stable binding and all three are compared on
/// revalidation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedPrincipalV1 {
    pub login: String,
    pub user_id: u64,
    pub node_id: String,
    /// GitHub's account type string, verbatim. Only `"User"` is a human.
    pub account_type: String,
    pub credential_context: CredentialContextV1,
    /// When the `/user` probe answered, RFC 3339.
    pub verified_at: String,
}

impl VerifiedPrincipalV1 {
    /// GitHub's `type` for a human account. A GitHub App installation token
    /// or a bot account resolves to something else and is refused: this gate
    /// exists to prove a *human* approved.
    pub const HUMAN_ACCOUNT_TYPE: &'static str = "User";

    fn require_human(&self) -> Result<(), AuthorityDenialV1> {
        if self.account_type == Self::HUMAN_ACCOUNT_TYPE {
            Ok(())
        } else {
            Err(AuthorityDenialV1::PrincipalNotHuman {
                login: self.login.clone(),
                account_type: self.account_type.clone(),
            })
        }
    }

    /// Identity comparison for revalidation. Deliberately strict: any of
    /// login/user_id/node_id/account_type differing denies. A rename is
    /// cheap to re-approve through and must not silently carry an old
    /// receipt's human-readable authority record forward.
    fn same_identity_as(&self, other: &VerifiedPrincipalV1) -> Result<(), AuthorityDenialV1> {
        if self.user_id != other.user_id {
            return Err(AuthorityDenialV1::PrincipalChanged {
                detail: format!("user_id {} -> {}", other.user_id, self.user_id),
            });
        }
        if self.node_id != other.node_id {
            return Err(AuthorityDenialV1::PrincipalChanged {
                detail: format!("node_id {} -> {}", other.node_id, self.node_id),
            });
        }
        if self.login != other.login {
            return Err(AuthorityDenialV1::PrincipalChanged {
                detail: format!("login {} -> {}", other.login, self.login),
            });
        }
        if self.account_type != other.account_type {
            return Err(AuthorityDenialV1::PrincipalChanged {
                detail: format!(
                    "account_type {} -> {}",
                    other.account_type, self.account_type
                ),
            });
        }
        Ok(())
    }
}

// ─── live repository facts ──────────────────────────────────────────────────

/// The authenticated principal's live permission bits on a repository, as
/// GitHub reports them on `GET /repos/{owner}/{repo}`. This object is
/// already scoped to the authenticated user by GitHub, which is exactly the
/// "live repository permission" the ratified decision names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoPermissionV1 {
    pub admin: bool,
    pub maintain: bool,
    pub push: bool,
    pub triage: bool,
    pub pull: bool,
}

impl fmt::Display for RepoPermissionV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{admin={},maintain={},push={},triage={},pull={}}}",
            self.admin, self.maintain, self.push, self.triage, self.pull
        )
    }
}

/// The permission floor a policy may require for a governed mutation.
///
/// There is deliberately **no** `Pull` or `Triage` variant: read or triage
/// access must never be configurable as sufficient to approve a precedent
/// mutation, and making that unrepresentable is stronger than documenting
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoPermissionLevelV1 {
    Admin,
    Maintain,
    Push,
}

impl RepoPermissionLevelV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Maintain => "maintain",
            Self::Push => "push",
        }
    }
}

impl RepoPermissionV1 {
    /// Explicit satisfaction table. GitHub already sets the lower bits when
    /// a higher one is true, but the OR chain does not rely on that — it is
    /// correct either way, and there is no default-allow arm.
    pub fn satisfies(&self, required: RepoPermissionLevelV1) -> bool {
        match required {
            RepoPermissionLevelV1::Admin => self.admin,
            RepoPermissionLevelV1::Maintain => self.admin || self.maintain,
            RepoPermissionLevelV1::Push => self.admin || self.maintain || self.push,
        }
    }
}

/// Live repository facts for the authenticated principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoFactsV1 {
    /// GitHub's canonical `full_name`. Compared against the approval target
    /// so a rename/redirect cannot answer for a different repository.
    pub full_name: String,
    pub owner_login: String,
    pub owner_id: u64,
    /// `"User"` or `"Organization"`, verbatim from GitHub.
    pub owner_type: String,
    pub permissions: RepoPermissionV1,
    pub observed_at: String,
}

impl RepoFactsV1 {
    pub const USER_OWNER_TYPE: &'static str = "User";
}

/// Result of a team-membership probe. `NotMember` is a *decided negative*,
/// distinct from [`AuthorityDenialV1::AuthorityUnavailable`]. Both refuse;
/// only the reported reason differs — which is why the live probe's
/// 404-vs-transport-error classification can never turn into a false allow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "membership", rename_all = "snake_case")]
pub enum TeamMembershipProbeV1 {
    Member(TeamMembershipV1),
    NotMember {
        org: String,
        team_slug: String,
        login: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMembershipV1 {
    pub org: String,
    pub team_slug: String,
    pub login: String,
    /// GitHub's membership `state`: `"active"` or `"pending"`.
    pub state: String,
    /// GitHub's membership `role`: `"member"` or `"maintainer"`.
    pub role: String,
    pub observed_at: String,
}

impl TeamMembershipV1 {
    pub const ACTIVE_STATE: &'static str = "active";
    pub const ROLE_MEMBER: &'static str = "member";
    pub const ROLE_MAINTAINER: &'static str = "maintainer";
}

/// Minimum team role a policy entry demands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRoleV1 {
    Member,
    Maintainer,
}

impl TeamRoleV1 {
    /// Explicit table over GitHub's closed role vocabulary. An unrecognized
    /// role string — a future GitHub value, a typo, or a forged fixture —
    /// matches neither recognized role and therefore satisfies nothing.
    /// There is no arm that returns `true` for an unknown input.
    fn satisfied_by(self, observed_role: &str) -> bool {
        let is_member = observed_role == TeamMembershipV1::ROLE_MEMBER;
        let is_maintainer = observed_role == TeamMembershipV1::ROLE_MAINTAINER;
        match self {
            Self::Member => is_member || is_maintainer,
            Self::Maintainer => is_maintainer,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Maintainer => "maintainer",
        }
    }
}

// ─── owner-authorized policy ────────────────────────────────────────────────

/// One owner-authorized GitHub organization team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedTeamV1 {
    pub org: String,
    pub team_slug: String,
    pub required_role: TeamRoleV1,
}

/// The owner-controlled authorization rules. This value must come from
/// server-side configuration the caller cannot reach; nothing in this crate
/// constructs it from request input.
///
/// [`ApproverAuthorizationPolicyV1::authorization_revision`] is the
/// "authorization revision" the ratified decision requires the receipt to
/// bind: it is the canonical-JSON SHA-256 of the whole policy, so adding a
/// team, lowering the permission floor, or extending the receipt lifetime
/// all invalidate every receipt issued under the old rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproverAuthorizationPolicyV1 {
    /// Owner-set label, carried into the revision hash so two policies with
    /// coincidentally identical rules still have distinct revisions when the
    /// owner means them to be distinct.
    pub policy_id: String,
    /// Whether verified repository ownership by itself confers authority.
    pub allow_repository_owner: bool,
    pub authorized_teams: Vec<AuthorizedTeamV1>,
    pub required_permission: RepoPermissionLevelV1,
    pub max_receipt_age_secs: i64,
    pub future_skew_tolerance_secs: i64,
}

impl ApproverAuthorizationPolicyV1 {
    /// The only policy this crate will construct without explicit owner
    /// configuration: repository ownership only, admin permission floor,
    /// five-minute receipts. It grants nothing by delegation — an empty
    /// team list is the safe default, not an inconvenient one.
    pub fn repository_owner_only(policy_id: impl Into<String>) -> Self {
        Self {
            policy_id: policy_id.into(),
            allow_repository_owner: true,
            authorized_teams: Vec::new(),
            required_permission: RepoPermissionLevelV1::Admin,
            max_receipt_age_secs: 300,
            future_skew_tolerance_secs: 60,
        }
    }

    /// Reject a policy that cannot authorize anyone, or that tries to buy
    /// more receipt lifetime / skew tolerance than the hard ceilings allow.
    /// A policy that authorizes nobody is *safe* but is almost always a
    /// misconfiguration, and a silently-always-denying gate is the "quiet
    /// degradation" failure this contract forbids — so it is loud too.
    pub fn validate(&self) -> Result<(), AuthorityDenialV1> {
        if self.policy_id.trim().is_empty() {
            return Err(AuthorityDenialV1::PolicyUnusable {
                detail: "policy_id is empty".to_string(),
            });
        }
        if !self.allow_repository_owner && self.authorized_teams.is_empty() {
            return Err(AuthorityDenialV1::PolicyUnusable {
                detail: "policy authorizes nobody: repository-owner authority is disabled and no \
                         organization team is authorized"
                    .to_string(),
            });
        }
        for team in &self.authorized_teams {
            if team.org.trim().is_empty() || team.team_slug.trim().is_empty() {
                return Err(AuthorityDenialV1::PolicyUnusable {
                    detail: format!(
                        "authorized team entry has an empty org or team_slug (org={:?}, \
                         team_slug={:?})",
                        team.org, team.team_slug
                    ),
                });
            }
        }
        if self.max_receipt_age_secs <= 0 {
            return Err(AuthorityDenialV1::PolicyUnusable {
                detail: format!(
                    "max_receipt_age_secs must be positive, got {}",
                    self.max_receipt_age_secs
                ),
            });
        }
        if self.max_receipt_age_secs > MAX_RECEIPT_AGE_CEILING_SECS {
            return Err(AuthorityDenialV1::PolicyUnusable {
                detail: format!(
                    "max_receipt_age_secs {} exceeds the hard ceiling \
                     {MAX_RECEIPT_AGE_CEILING_SECS}",
                    self.max_receipt_age_secs
                ),
            });
        }
        if self.future_skew_tolerance_secs < 0 {
            return Err(AuthorityDenialV1::PolicyUnusable {
                detail: format!(
                    "future_skew_tolerance_secs must not be negative, got {}",
                    self.future_skew_tolerance_secs
                ),
            });
        }
        if self.future_skew_tolerance_secs > MAX_FUTURE_SKEW_CEILING_SECS {
            return Err(AuthorityDenialV1::PolicyUnusable {
                detail: format!(
                    "future_skew_tolerance_secs {} exceeds the hard ceiling \
                     {MAX_FUTURE_SKEW_CEILING_SECS}",
                    self.future_skew_tolerance_secs
                ),
            });
        }
        Ok(())
    }

    /// Canonical-JSON SHA-256 of the whole policy. Validated first so an
    /// unusable policy cannot mint a revision that a receipt then binds.
    pub fn authorization_revision(&self) -> Result<String, AuthorityDenialV1> {
        self.validate()?;
        canonical_json_sha256(self).map_err(|detail| AuthorityDenialV1::BindingComputationFailed {
            detail: format!("authorization_revision: {detail}"),
        })
    }
}

// ─── the approval target ────────────────────────────────────────────────────

/// The governed mutations this gate covers. Closed set — this leaf gates
/// precedent establishment and overturn (#1077) and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernedActionV1 {
    EstablishPrecedent,
    OverturnPrecedent,
}

impl GovernedActionV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EstablishPrecedent => "establish_precedent",
            Self::OverturnPrecedent => "overturn_precedent",
        }
    }
}

impl fmt::Display for GovernedActionV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A caller's claim that a repository ref sat at a commit. Carries no
/// `verified_at`: the *observation* time belongs to the server-side
/// evidence on the receipt ([`ApprovalReceiptV1::verified_repo_revisions`]),
/// not to the caller's claim, so target equality stays a pure value
/// comparison with no timestamp to drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRevisionPinV1 {
    pub repo: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub commit_sha: String,
}

/// Everything a caller may assert about itself. **Nothing in this struct is
/// read by any authorization branch.** It exists so the forgeable fields
/// have one obvious, inert home and so a reviewer can grep for it and
/// confirm the evaluation functions never take it as a parameter.
///
/// It is carried on the receipt and hashed into it — not because it grants
/// anything, but so that a stored receipt's description cannot be rewritten
/// after issuance to claim a different actor approved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerAssertedContextV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjudicator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_claim: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_receipt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_capability: Option<String>,
}

/// The exact thing being approved. Every field here is part of the receipt's
/// identity: an approval for one proposal, one source bundle, one repository
/// revision set, and one action can never be replayed onto another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalTargetV1 {
    /// `owner/repo` the governed mutation belongs to.
    pub repo: String,
    pub action: GovernedActionV1,
    /// Identifier of the precedent/packet being established or overturned.
    pub target_ref: String,
    /// The #1077 packet id this approval belongs to.
    pub packet_id: String,
    /// Hash of the immutable proposal payload.
    pub proposal_hash: String,
    /// Hash of the immutable source bundle the proposal was derived from.
    pub source_bundle_hash: String,
    /// Individual source snapshot hashes (issue/PR/doc), sorted by the
    /// caller; compared as an exact sequence.
    pub source_snapshot_hashes: Vec<String>,
    /// Repository revisions the proposal is pinned to. Must be non-empty:
    /// an unpinned repository axis is unverified, not unchanged.
    pub repo_revision_pins: Vec<RepoRevisionPinV1>,
}

// ─── the receipt ────────────────────────────────────────────────────────────

/// One constructible variant, on purpose. A receipt can only ever say
/// "approved"; refusals are [`AuthorityDenialV1`] values, so no caller can
/// hold something receipt-shaped that secretly means "denied" and be
/// tempted to read only the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionV1 {
    Approved,
}

/// How authority was established for this receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub enum AuthorityBasisV1 {
    /// The verified principal *is* the repository's owning user account,
    /// matched on the stable numeric account id, not the login string.
    RepositoryOwner {
        owner_login: String,
        owner_id: u64,
        owner_type: String,
    },
    /// The verified principal is an active member of an owner-authorized
    /// organization team with a sufficient role.
    AuthorizedOrgTeam {
        org: String,
        team_slug: String,
        required_role: TeamRoleV1,
        membership_state: String,
        membership_role: String,
    },
}

/// The immutable approval receipt.
///
/// Field-by-field, every entry answers "what would make this approval a lie
/// if it changed" — and each one is re-checked by
/// [`revalidate_approval`] at the mutation choke point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalReceiptV1 {
    /// Wire-format discriminator, so a stored receipt of a future shape can
    /// never be silently read as this one.
    pub receipt_version: String,
    /// One variant: `Approved`.
    pub decision: ApprovalDecisionV1,
    /// Who GitHub says the active credential authenticates, including the
    /// non-secret credential fingerprint.
    pub principal: VerifiedPrincipalV1,
    /// Which repository the authority was evaluated against. Redundant with
    /// `target.repo` on purpose: they are compared, so a receipt whose
    /// authority was proven on repo A can never be presented for a target
    /// in repo B.
    pub repo: String,
    /// Ownership or team membership — the evidence class that granted
    /// authority.
    pub authority: AuthorityBasisV1,
    /// The live permission bits observed at `checked_at`. Revocation shows
    /// up here on revalidation.
    pub observed_permission: RepoPermissionV1,
    /// The permission floor the policy demanded at issuance.
    pub required_permission: RepoPermissionLevelV1,
    /// Canonical hash of the owner-authorized policy in force at issuance.
    pub authorization_revision: String,
    /// When the server completed the authority probes, RFC 3339. Parsed —
    /// never string-compared.
    pub checked_at: String,
    /// The exact target/action approved.
    pub target: ApprovalTargetV1,
    /// Server-observed repository revisions at `checked_at`, matching
    /// `target.repo_revision_pins` one-for-one. These are the server's own
    /// evidence, not the caller's claim.
    pub verified_repo_revisions: Vec<RepoRevisionV1>,
    /// Descriptive only. Never consulted by any authorization branch;
    /// hashed so it cannot be rewritten after issuance.
    pub caller_asserted: CallerAssertedContextV1,
    /// Canonical-JSON SHA-256 over every field above.
    pub receipt_hash: String,
}

impl ApprovalReceiptV1 {
    pub const VERSION: &'static str = "approval_receipt_v1";

    fn hash_basis(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(obj) = value.as_object_mut() {
            obj.remove("receipt_hash");
        }
        value
    }

    fn compute_hash(&self) -> Result<String, AuthorityDenialV1> {
        let basis = self.hash_basis();
        if basis.is_null() {
            return Err(AuthorityDenialV1::BindingComputationFailed {
                detail: "approval receipt failed to serialize for hashing".to_string(),
            });
        }
        canonical_json_sha256(&basis).map_err(|detail| {
            AuthorityDenialV1::BindingComputationFailed {
                detail: format!("receipt_hash: {detail}"),
            }
        })
    }

    /// Recompute the receipt hash and refuse if the stored receipt was
    /// edited after issuance.
    pub fn verify_integrity(&self) -> Result<(), AuthorityDenialV1> {
        if self.receipt_version != Self::VERSION {
            return Err(AuthorityDenialV1::ReceiptTampered {
                detail: format!(
                    "receipt_version '{}' is not '{}'",
                    self.receipt_version,
                    Self::VERSION
                ),
            });
        }
        let recomputed = self.compute_hash()?;
        if recomputed != self.receipt_hash {
            return Err(AuthorityDenialV1::ReceiptTampered {
                detail: format!("recomputed {recomputed}, recorded {}", self.receipt_hash),
            });
        }
        Ok(())
    }
}

// ─── time handling ──────────────────────────────────────────────────────────

/// Parse an RFC 3339 timestamp into a UTC instant, or deny.
///
/// This is the *only* way a timestamp becomes comparable anywhere in this
/// module. There is no fallback to `String` ordering: a value that will not
/// parse produces [`AuthorityDenialV1::TimestampUnparseable`], because
/// lexicographic order over RFC 3339 strings is wrong across offsets
/// (`2026-07-25T00:00:00+08:00` sorts after `2026-07-24T17:30:00Z` as text
/// while being the *earlier* instant) and this repository has already been
/// bitten by exactly that (freeze-comparison-semantics: a frozen SQL shape
/// with an unfrozen time comparison deleted future rows).
///
/// - format: RFC 3339 with explicit offset
/// - semantic layer: absolute instant, normalized to UTC
/// - failure direction: deny
pub fn parse_rfc3339_utc(field: &str, value: &str) -> Result<DateTime<Utc>, AuthorityDenialV1> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| AuthorityDenialV1::TimestampUnparseable {
            field: field.to_string(),
            value: value.to_string(),
        })
}

/// Freshness check over parsed instants, both directions pinned:
/// too old → [`AuthorityDenialV1::ReceiptExpired`]; too far in the future
/// (a skewed or forged clock) → [`AuthorityDenialV1::ReceiptFromFuture`].
fn check_receipt_freshness(
    receipt: &ApprovalReceiptV1,
    policy: &ApproverAuthorizationPolicyV1,
    now: DateTime<Utc>,
) -> Result<(), AuthorityDenialV1> {
    let checked_at = parse_rfc3339_utc("receipt.checked_at", &receipt.checked_at)?;
    let age = now.signed_duration_since(checked_at);
    let age_secs = age.num_seconds();
    if age_secs > policy.max_receipt_age_secs {
        return Err(AuthorityDenialV1::ReceiptExpired {
            checked_at: receipt.checked_at.clone(),
            now: now.to_rfc3339(),
            age_secs,
            max_age_secs: policy.max_receipt_age_secs,
        });
    }
    if age_secs < -policy.future_skew_tolerance_secs {
        return Err(AuthorityDenialV1::ReceiptFromFuture {
            checked_at: receipt.checked_at.clone(),
            now: now.to_rfc3339(),
            ahead_secs: -age_secs,
            tolerance_secs: policy.future_skew_tolerance_secs,
        });
    }
    Ok(())
}

// ─── the probe seam ─────────────────────────────────────────────────────────

/// The only way live GitHub facts enter this module.
///
/// Every method returns `Result<_, AuthorityDenialV1>`, so an implementation
/// has no vocabulary for "unknown" other than a denial: unreachable GitHub,
/// a timeout, a rate limit, or an unparseable body must surface as
/// [`AuthorityDenialV1::AuthorityUnavailable`], and the pure resolver
/// propagates it unchanged. There is no `Option`, no `bool`, and no default
/// value anywhere on this trait through which an absent answer could become
/// a permissive one.
pub trait ApproverAuthorityProbe {
    /// `GET /user` — the principal the active credential authenticates,
    /// plus the non-secret credential context.
    fn authenticated_principal(&self) -> Result<VerifiedPrincipalV1, AuthorityDenialV1>;

    /// `GET /repos/{owner}/{repo}` — canonical name, owner identity, and the
    /// authenticated principal's live permission bits.
    fn repo_facts(&self, repo: &str) -> Result<RepoFactsV1, AuthorityDenialV1>;

    /// `GET /orgs/{org}/teams/{team_slug}/memberships/{login}` — a decided
    /// membership answer. HTTP 404 is `NotMember`; every other failure is a
    /// denial.
    fn team_membership(
        &self,
        org: &str,
        team_slug: &str,
        login: &str,
    ) -> Result<TeamMembershipProbeV1, AuthorityDenialV1>;

    /// `GET /repos/{owner}/{repo}/commits/{ref}` — the commit a ref resolves
    /// to right now.
    fn repo_revision(&self, repo: &str, git_ref: &str)
        -> Result<RepoRevisionV1, AuthorityDenialV1>;
}

// ─── evaluation (pure) ──────────────────────────────────────────────────────

/// Decide the authority basis from *already-probed* facts.
///
/// Note the parameter list: there is no caller-supplied actor, agent
/// identity, work claim, model, seat, execution receipt, or delegation
/// capability in it. That absence, not a comment, is what makes forged
/// caller fields unable to cross this gate.
///
/// Ordering: repository ownership first (cheapest, needs no extra probe),
/// then each authorized team in policy order. An unavailable team probe
/// aborts immediately with the unavailable denial rather than continuing —
/// a flaky probe must not be absorbed into an eventual "not a member", and
/// a later team must not mask that some evidence was never actually read.
fn evaluate_authority_basis<P: ApproverAuthorityProbe + ?Sized>(
    probe: &P,
    policy: &ApproverAuthorizationPolicyV1,
    principal: &VerifiedPrincipalV1,
    repo_facts: &RepoFactsV1,
) -> Result<AuthorityBasisV1, AuthorityDenialV1> {
    if policy.allow_repository_owner
        && repo_facts.owner_type == RepoFactsV1::USER_OWNER_TYPE
        && repo_facts.owner_id == principal.user_id
    {
        return Ok(AuthorityBasisV1::RepositoryOwner {
            owner_login: repo_facts.owner_login.clone(),
            owner_id: repo_facts.owner_id,
            owner_type: repo_facts.owner_type.clone(),
        });
    }

    let mut last_unusable: Option<AuthorityDenialV1> = None;
    for team in &policy.authorized_teams {
        // Propagates `AuthorityUnavailable` with `?` on purpose.
        let membership = probe.team_membership(&team.org, &team.team_slug, &principal.login)?;
        match membership {
            TeamMembershipProbeV1::NotMember { .. } => continue,
            TeamMembershipProbeV1::Member(m) => {
                if m.state != TeamMembershipV1::ACTIVE_STATE {
                    last_unusable = Some(AuthorityDenialV1::TeamMembershipNotUsable {
                        org: team.org.clone(),
                        team_slug: team.team_slug.clone(),
                        login: principal.login.clone(),
                        state: m.state.clone(),
                        role: m.role.clone(),
                        detail: format!(
                            "membership state must be '{}'",
                            TeamMembershipV1::ACTIVE_STATE
                        ),
                    });
                    continue;
                }
                if !team.required_role.satisfied_by(&m.role) {
                    last_unusable = Some(AuthorityDenialV1::TeamMembershipNotUsable {
                        org: team.org.clone(),
                        team_slug: team.team_slug.clone(),
                        login: principal.login.clone(),
                        state: m.state.clone(),
                        role: m.role.clone(),
                        detail: format!("policy requires role '{}'", team.required_role.as_str()),
                    });
                    continue;
                }
                return Ok(AuthorityBasisV1::AuthorizedOrgTeam {
                    org: team.org.clone(),
                    team_slug: team.team_slug.clone(),
                    required_role: team.required_role,
                    membership_state: m.state,
                    membership_role: m.role,
                });
            }
        }
    }

    // No arm above granted authority. This is the default branch, and it
    // denies (#919's `else { true }` is the anti-pattern it exists to
    // prevent). A near-miss membership is reported as such so the operator
    // sees why, but it is still a refusal.
    if let Some(denial) = last_unusable {
        return Err(denial);
    }
    Err(AuthorityDenialV1::NotAuthorized {
        login: principal.login.clone(),
        repo: repo_facts.full_name.clone(),
        detail: if policy.authorized_teams.is_empty() {
            "not the repository owner, and the policy authorizes no organization team".to_string()
        } else {
            "not the repository owner, and not an active member of any authorized team".to_string()
        },
    })
}

/// Re-check the same authority basis a receipt recorded, against freshly
/// probed facts. Revocation — removed from the team, permission downgraded,
/// repository transferred — surfaces here.
fn revalidate_authority_basis<P: ApproverAuthorityProbe + ?Sized>(
    probe: &P,
    principal: &VerifiedPrincipalV1,
    repo_facts: &RepoFactsV1,
    recorded: &AuthorityBasisV1,
) -> Result<(), AuthorityDenialV1> {
    match recorded {
        AuthorityBasisV1::RepositoryOwner {
            owner_login,
            owner_id,
            owner_type,
        } => {
            if repo_facts.owner_type != *owner_type
                || repo_facts.owner_id != *owner_id
                || repo_facts.owner_login != *owner_login
            {
                return Err(AuthorityDenialV1::AuthorityEvidenceChanged {
                    detail: format!(
                        "repository owner changed: receipt bound {owner_login}#{owner_id} \
                         ({owner_type}), live is {}#{} ({})",
                        repo_facts.owner_login, repo_facts.owner_id, repo_facts.owner_type
                    ),
                });
            }
            if repo_facts.owner_id != principal.user_id {
                return Err(AuthorityDenialV1::AuthorityEvidenceChanged {
                    detail: format!(
                        "verified principal #{} is no longer the repository owner #{}",
                        principal.user_id, repo_facts.owner_id
                    ),
                });
            }
            Ok(())
        }
        AuthorityBasisV1::AuthorizedOrgTeam {
            org,
            team_slug,
            required_role,
            membership_state,
            membership_role,
        } => {
            let membership = probe.team_membership(org, team_slug, &principal.login)?;
            match membership {
                TeamMembershipProbeV1::NotMember { .. } => {
                    Err(AuthorityDenialV1::AuthorityEvidenceChanged {
                        detail: format!(
                            "'{}' is no longer a member of {org}/{team_slug}",
                            principal.login
                        ),
                    })
                }
                TeamMembershipProbeV1::Member(m) => {
                    if m.state != TeamMembershipV1::ACTIVE_STATE
                        || !required_role.satisfied_by(&m.role)
                    {
                        return Err(AuthorityDenialV1::TeamMembershipNotUsable {
                            org: org.clone(),
                            team_slug: team_slug.clone(),
                            login: principal.login.clone(),
                            state: m.state,
                            role: m.role,
                            detail: format!(
                                "membership no longer satisfies required role '{}'",
                                required_role.as_str()
                            ),
                        });
                    }
                    if m.state != *membership_state || m.role != *membership_role {
                        return Err(AuthorityDenialV1::AuthorityEvidenceChanged {
                            detail: format!(
                                "team membership evidence changed: receipt bound \
                                 state={membership_state}/role={membership_role}, live is \
                                 state={}/role={}",
                                m.state, m.role
                            ),
                        });
                    }
                    Ok(())
                }
            }
        }
    }
}

/// Probe every pinned repository revision and confirm it still resolves to
/// the pinned commit. An empty pin list is drift, not agreement.
fn verify_repo_revision_pins<P: ApproverAuthorityProbe + ?Sized>(
    probe: &P,
    pins: &[RepoRevisionPinV1],
) -> Result<Vec<RepoRevisionV1>, AuthorityDenialV1> {
    if pins.is_empty() {
        return Err(AuthorityDenialV1::RevisionDrift {
            reasons: vec![RevisionDriftV1::RepoRevisionUnpinned {
                detail: "approval target pins no repository revision at all — the repository axis \
                         was never verified, not confirmed unchanged"
                    .to_string(),
            }],
        });
    }
    let mut observed = Vec::with_capacity(pins.len());
    let mut reasons = Vec::new();
    for pin in pins {
        let live = probe.repo_revision(&pin.repo, &pin.git_ref)?;
        if live.commit_sha != pin.commit_sha {
            reasons.push(RevisionDriftV1::RepoRevisionChanged {
                repo: pin.repo.clone(),
                git_ref: pin.git_ref.clone(),
                expected_commit_sha: pin.commit_sha.clone(),
                actual_commit_sha: Some(live.commit_sha.clone()),
            });
        }
        observed.push(live);
    }
    if reasons.is_empty() {
        Ok(observed)
    } else {
        Err(AuthorityDenialV1::RevisionDrift { reasons })
    }
}

// ─── the one resolver ───────────────────────────────────────────────────────

/// **The** server-side resolver. Returns a verified approver principal bound
/// into an immutable [`ApprovalReceiptV1`], or a typed loud
/// [`AuthorityDenialV1`]. There is no third outcome and no boolean.
///
/// `caller_asserted` is accepted only to be recorded; it is not forwarded to
/// any evaluation function.
pub fn resolve_verified_approver<P: ApproverAuthorityProbe + ?Sized>(
    probe: &P,
    policy: &ApproverAuthorizationPolicyV1,
    target: &ApprovalTargetV1,
    caller_asserted: &CallerAssertedContextV1,
    now: DateTime<Utc>,
) -> Result<ApprovalReceiptV1, AuthorityDenialV1> {
    let authorization_revision = policy.authorization_revision()?;

    let principal = probe.authenticated_principal()?;
    principal.require_human()?;
    // Parsed, not string-compared: an unparseable probe timestamp denies.
    parse_rfc3339_utc("principal.verified_at", &principal.verified_at)?;

    let repo_facts = probe.repo_facts(&target.repo)?;
    parse_rfc3339_utc("repo_facts.observed_at", &repo_facts.observed_at)?;
    if !repo_facts.full_name.eq_ignore_ascii_case(&target.repo) {
        return Err(AuthorityDenialV1::RepositoryMismatch {
            expected: target.repo.clone(),
            actual: repo_facts.full_name.clone(),
        });
    }

    if !repo_facts.permissions.satisfies(policy.required_permission) {
        return Err(AuthorityDenialV1::InsufficientRepoPermission {
            login: principal.login.clone(),
            repo: repo_facts.full_name.clone(),
            required: policy.required_permission,
            observed: repo_facts.permissions,
        });
    }

    let authority = evaluate_authority_basis(probe, policy, &principal, &repo_facts)?;
    let verified_repo_revisions = verify_repo_revision_pins(probe, &target.repo_revision_pins)?;

    let mut receipt = ApprovalReceiptV1 {
        receipt_version: ApprovalReceiptV1::VERSION.to_string(),
        decision: ApprovalDecisionV1::Approved,
        principal,
        repo: repo_facts.full_name.clone(),
        authority,
        observed_permission: repo_facts.permissions,
        required_permission: policy.required_permission,
        authorization_revision,
        checked_at: now.to_rfc3339(),
        target: target.clone(),
        verified_repo_revisions,
        caller_asserted: caller_asserted.clone(),
        receipt_hash: String::new(),
    };
    let receipt_hash = receipt.compute_hash()?;
    receipt.receipt_hash = receipt_hash;
    Ok(receipt)
}

/// What the governed path recomputed at the mutation choke point, so
/// revalidation compares against freshly derived facts rather than against
/// whatever the caller says the proposal still is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentApprovalContextV1 {
    /// The target as recomputed at apply time.
    pub target: ApprovalTargetV1,
}

/// Revalidate an approval at the mutation choke point. `Ok(())` is the only
/// thing that may be followed by a mutation; every other outcome is a
/// refusal that mutates nothing.
///
/// Order is deliberate — cheapest and most certain refusals first, so an
/// obviously stale receipt never causes GitHub traffic:
///
/// 1. receipt integrity (recompute hash);
/// 2. policy still valid and `authorization_revision` unchanged;
/// 3. receipt freshness, on parsed instants;
/// 4. target/action/proposal/source binding;
/// 5. live principal + credential context;
/// 6. live repository identity and permission floor;
/// 7. live ownership/team evidence (this is where revocation lands);
/// 8. live repository revisions.
pub fn revalidate_approval<P: ApproverAuthorityProbe + ?Sized>(
    probe: &P,
    policy: &ApproverAuthorizationPolicyV1,
    receipt: &ApprovalReceiptV1,
    current: &CurrentApprovalContextV1,
    now: DateTime<Utc>,
) -> Result<(), AuthorityDenialV1> {
    receipt.verify_integrity()?;

    let authorization_revision = policy.authorization_revision()?;
    if authorization_revision != receipt.authorization_revision {
        return Err(AuthorityDenialV1::AuthorizationRevisionChanged {
            expected: receipt.authorization_revision.clone(),
            actual: authorization_revision,
        });
    }

    check_receipt_freshness(receipt, policy, now)?;

    check_target_binding(receipt, &current.target)?;

    let live_principal = probe.authenticated_principal()?;
    live_principal.require_human()?;
    live_principal.same_identity_as(&receipt.principal)?;
    if live_principal.credential_context.credential_fingerprint
        != receipt.principal.credential_context.credential_fingerprint
        || live_principal.credential_context.source != receipt.principal.credential_context.source
    {
        // Deliberately carries no detail: neither the fingerprint nor the
        // source is echoed, so a denial message can never become a credential
        // oracle.
        return Err(AuthorityDenialV1::CredentialContextChanged);
    }

    let repo_facts = probe.repo_facts(&receipt.target.repo)?;
    if !repo_facts.full_name.eq_ignore_ascii_case(&receipt.repo) {
        return Err(AuthorityDenialV1::RepositoryMismatch {
            expected: receipt.repo.clone(),
            actual: repo_facts.full_name,
        });
    }
    if !repo_facts
        .permissions
        .satisfies(receipt.required_permission)
    {
        return Err(AuthorityDenialV1::InsufficientRepoPermission {
            login: live_principal.login.clone(),
            repo: repo_facts.full_name.clone(),
            required: receipt.required_permission,
            observed: repo_facts.permissions,
        });
    }
    if repo_facts.permissions != receipt.observed_permission {
        return Err(AuthorityDenialV1::AuthorityEvidenceChanged {
            detail: format!(
                "live repository permission {} differs from the receipt's {}",
                repo_facts.permissions, receipt.observed_permission
            ),
        });
    }

    revalidate_authority_basis(probe, &live_principal, &repo_facts, &receipt.authority)?;

    let live_revisions = verify_repo_revision_pins(probe, &receipt.target.repo_revision_pins)?;
    let mut reasons = Vec::new();
    for (bound, live) in receipt.verified_repo_revisions.iter().zip(&live_revisions) {
        if bound.repo != live.repo || bound.git_ref != live.git_ref {
            reasons.push(RevisionDriftV1::RepoRevisionChanged {
                repo: bound.repo.clone(),
                git_ref: bound.git_ref.clone(),
                expected_commit_sha: bound.commit_sha.clone(),
                actual_commit_sha: None,
            });
            continue;
        }
        if bound.commit_sha != live.commit_sha {
            reasons.push(RevisionDriftV1::RepoRevisionChanged {
                repo: bound.repo.clone(),
                git_ref: bound.git_ref.clone(),
                expected_commit_sha: bound.commit_sha.clone(),
                actual_commit_sha: Some(live.commit_sha.clone()),
            });
        }
    }
    if receipt.verified_repo_revisions.len() != live_revisions.len() {
        reasons.push(RevisionDriftV1::RepoRevisionUnpinned {
            detail: format!(
                "receipt bound {} verified repository revisions, {} resolve now",
                receipt.verified_repo_revisions.len(),
                live_revisions.len()
            ),
        });
    }
    if !reasons.is_empty() {
        return Err(AuthorityDenialV1::RevisionDrift { reasons });
    }

    Ok(())
}

/// Compare the receipt's bound target against the one recomputed at apply
/// time. Every mismatch class gets its own message so an operator can tell a
/// replay attempt from ordinary drift.
fn check_target_binding(
    receipt: &ApprovalReceiptV1,
    current: &ApprovalTargetV1,
) -> Result<(), AuthorityDenialV1> {
    let bound = &receipt.target;
    if bound.repo != current.repo || receipt.repo != current.repo {
        return Err(AuthorityDenialV1::TargetMismatch {
            detail: format!(
                "repository: receipt bound '{}' (authority proven on '{}'), apply targets '{}'",
                bound.repo, receipt.repo, current.repo
            ),
        });
    }
    if bound.action != current.action {
        return Err(AuthorityDenialV1::TargetMismatch {
            detail: format!(
                "action: receipt approved '{}', apply attempts '{}'",
                bound.action, current.action
            ),
        });
    }
    if bound.target_ref != current.target_ref {
        return Err(AuthorityDenialV1::TargetMismatch {
            detail: format!(
                "target_ref: receipt approved '{}', apply attempts '{}'",
                bound.target_ref, current.target_ref
            ),
        });
    }
    if bound.packet_id != current.packet_id {
        return Err(AuthorityDenialV1::TargetMismatch {
            detail: format!(
                "packet_id: receipt approved '{}', apply attempts '{}'",
                bound.packet_id, current.packet_id
            ),
        });
    }

    let mut reasons = Vec::new();
    if bound.proposal_hash != current.proposal_hash {
        reasons.push(RevisionDriftV1::ProposalHashChanged {
            expected: bound.proposal_hash.clone(),
            actual: current.proposal_hash.clone(),
        });
    }
    if bound.source_bundle_hash != current.source_bundle_hash {
        reasons.push(RevisionDriftV1::SourceBundleHashChanged {
            expected: bound.source_bundle_hash.clone(),
            actual: current.source_bundle_hash.clone(),
        });
    }
    if bound.source_snapshot_hashes != current.source_snapshot_hashes {
        reasons.push(RevisionDriftV1::SourceSnapshotsChanged {
            expected: bound.source_snapshot_hashes.clone(),
            actual: current.source_snapshot_hashes.clone(),
        });
    }
    if bound.repo_revision_pins != current.repo_revision_pins {
        for pin in &bound.repo_revision_pins {
            let live = current
                .repo_revision_pins
                .iter()
                .find(|p| p.repo == pin.repo && p.git_ref == pin.git_ref);
            let live_sha = live.map(|p| p.commit_sha.clone());
            if live_sha.as_deref() != Some(pin.commit_sha.as_str()) {
                reasons.push(RevisionDriftV1::RepoRevisionChanged {
                    repo: pin.repo.clone(),
                    git_ref: pin.git_ref.clone(),
                    expected_commit_sha: pin.commit_sha.clone(),
                    actual_commit_sha: live_sha,
                });
            }
        }
        if reasons.is_empty() {
            // The pin lists differ but every bound pin still matches — the
            // apply side added pins the approval never covered. Refuse: an
            // approval covers exactly the revision set it was issued for.
            reasons.push(RevisionDriftV1::RepoRevisionUnpinned {
                detail: format!(
                    "apply pins {} repository revisions, the receipt approved {}",
                    current.repo_revision_pins.len(),
                    bound.repo_revision_pins.len()
                ),
            });
        }
    }
    if !reasons.is_empty() {
        return Err(AuthorityDenialV1::RevisionDrift { reasons });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
