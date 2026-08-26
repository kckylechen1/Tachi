//! Admission for `TaskIntentV1` submissions (tachi#1840, zeroclaw #205
//! TB-4/TB-5).
//!
//! Two laws, both fail-closed and both typed:
//!
//! 1. **Forbidden content (TB-4).** Every text-bearing value on the wire —
//!    `objective`, constraint descriptions, artifact descriptions, source
//!    locators, workspace selectors, the context bundle ref — is scanned
//!    per forbidden category: credential-shaped values, CLI/SSH/tmux/shell
//!    commands, worktree paths, Private-Dyad-labeled values, oversized
//!    transcripts (structurally impossible: [`super::wire::BoundedText`]
//!    caps construction), and caller-minted task/attempt ids. A hit is a
//!    typed [`AdmissionRejection`] naming the category and the wire field.
//! 2. **Context is not authority (TB-4 seam law).** Admission consumes only
//!    authority-bearing fields of the intent plus the requester's own
//!    admitted authority (via [`super::RequesterAuthorityPort`]). The
//!    context bundle is an opaque ref on this wire; its CONTENT is never an
//!    input to any admission decision, so an intent whose only difference
//!    is bundle/guidance content yields an identical admission decision.
//!
//! 3. **Requester-bounded capability (TB-5).** `capability_request` must be
//!    within the requester's own admitted capability set. The closed
//!    [`super::wire::Capability`] enum already excludes vendor/CLI tokens
//!    structurally (DECISION TB-5/A option (a)); the requester-bound check
//!    narrows further: an unadmitted capability is a typed rejection even
//!    when well-formed.

use super::refs::{AttemptRef, RequesterRef, TaskRef};
use super::wire::{BoundedText, TaskIntentV1};

/// Forbidden-content categories (TB-4). One typed variant per category so a
/// rejection is inspectable, never a bare string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ForbiddenCategory {
    /// Raw credential-shaped value (API keys, tokens, private keys).
    #[error("credential-shaped value")]
    Credential,
    /// Shell/SSH/tmux/container command text.
    #[error("cli/shell command text")]
    Command,
    /// Worktree/filesystem path used as execution authority.
    #[error("worktree-shaped path")]
    WorktreePath,
    /// Private-Dyad-labeled value.
    #[error("private-dyad-labeled value")]
    PrivateDyad,
    /// Caller-minted task/attempt id smuggled as content.
    #[error("caller-minted task/attempt id")]
    CallerMintedRef,
}

/// Typed admission rejection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionRejection {
    /// A text-bearing value matched a TB-4 forbidden category.
    #[error("admission rejected: {category} in field `{field}`")]
    ForbiddenContent {
        /// The matched category.
        category: ForbiddenCategory,
        /// The wire field carrying the offending value.
        field: &'static str,
    },
    /// The requested capability exceeds the requester's own admitted
    /// authority (TB-5 intersection law). Skill/Procedure requirements and
    /// guidance content can only narrow, never originate or widen.
    #[error("admission rejected: capability not admitted for requester")]
    CapabilityNotAdmitted,
    /// The requester identity is not admitted by the authority source.
    #[error("admission rejected: requester not admitted")]
    RequesterNotAdmitted,
    /// The wire schema tag is not `task-intent.v1` (golden pin).
    #[error("admission rejected: schema tag mismatch (expected {expected}, got {actual})")]
    SchemaTag {
        /// The expected tag.
        expected: &'static str,
        /// The submitted tag.
        actual: String,
    },
    /// `retry_of` names a task Tachi never admitted (TB-18: lineage must
    /// reference real prior tasks).
    #[error("admission rejected: retry_of references an unknown task")]
    UnknownRetryLineage,
    /// No execution lane admitted the intent (staffing-plane refusal).
    #[error("admission rejected: no admitted execution plan")]
    NoAdmittedExecutionPlan,
}

/// Substrings whose presence in any text-bearing value is
/// credential-shaped (TB-4 category 1). Matched case-insensitively.
const CREDENTIAL_MARKERS: &[&str] = &[
    "-----BEGIN OPENSSH PRIVATE KEY",
    "-----BEGIN RSA PRIVATE KEY",
    "-----BEGIN PRIVATE KEY",
    "-----BEGIN EC PRIVATE KEY",
    "sk-ant-",     // anthropic-style key prefix
    "sk-proj-",    // openai-style key prefix
    "ghp_",        // github PAT
    "github_pat_", // github fine-grained PAT
    "gho_",        // github oauth token
    "xoxb-",       // slack bot token
    "xoxp-",       // slack user token
    "AKIA",        // aws access key id prefix (AKIA + 16 upper alnum)
    "api_key=",    // inline assignment
    "apikey:",
    "password=",
    "bearer ",
];

/// Leading tokens that make a value a shell/SSH/tmux/container command
/// (TB-4 category 2). Matched on the first whitespace-separated token,
/// case-insensitively.
const COMMAND_LEAD_TOKENS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "exec", "eval", "source", "sudo", "su", "ssh", "scp",
    "sftp", "mosh", "telnet", "tmux", "screen", "docker", "podman", "kubectl", "nerdctl", "git",
    "cargo", "npm", "pnpm", "yarn", "python", "python3", "node", "ruby", "codex", "claude",
    "gemini", "opencode", "aider", "grok", "rm", "mv", "cp", "chmod", "chown", "curl", "wget",
    "nc",
];

/// Markers that make a value a worktree/filesystem path (TB-4 category 3).
const WORKTREE_MARKERS: &[&str] = &[
    "/worktrees/",
    "worktree_path",
    ".git/",
    "/Users/",
    "/home/",
    "/tmp/",
    "/var/folders/",
    "\\.git\\",
];

/// Markers for Private-Dyad-labeled content (TB-4 category 4).
const PRIVATE_DYAD_MARKERS: &[&str] = &["private dyad", "private_dyad", "private-dyad"];

/// Scan one text-bearing value against every forbidden category.
///
/// Returns the first match by category order. Oversized transcripts are
/// impossible by construction (`BoundedText`), so no category exists for
/// them here — construction already rejected the payload.
pub fn scan_text(field: &'static str, value: &BoundedText) -> Result<(), AdmissionRejection> {
    let text = value.as_str();
    let lower = text.to_ascii_lowercase();

    for marker in CREDENTIAL_MARKERS {
        if text.contains(marker) || lower.contains(&marker.to_ascii_lowercase()) {
            return Err(reject(ForbiddenCategory::Credential, field));
        }
    }
    let first_token = lower.split_whitespace().next().unwrap_or("");
    if COMMAND_LEAD_TOKENS.contains(&first_token) {
        return Err(reject(ForbiddenCategory::Command, field));
    }
    if lower.starts_with("./") || lower.starts_with("/") || lower.starts_with('~') {
        return Err(reject(ForbiddenCategory::WorktreePath, field));
    }
    for marker in WORKTREE_MARKERS {
        // The text is matched lowercased, so the marker must be too
        // (`/Users/` would never match otherwise).
        if lower.contains(&marker.to_ascii_lowercase()) {
            return Err(reject(ForbiddenCategory::WorktreePath, field));
        }
    }
    for marker in PRIVATE_DYAD_MARKERS {
        if lower.contains(marker) {
            return Err(reject(ForbiddenCategory::PrivateDyad, field));
        }
    }
    // Caller-minted ref namespaces: the value carries a `task:`/`attempt:`
    // wire-form id, which only Tachi mints (TB-6); a caller-asserted one is
    // forbidden content, not authority.
    if text.contains(TaskRef::WIRE_PREFIX) || text.contains(AttemptRef::WIRE_PREFIX) {
        return Err(reject(ForbiddenCategory::CallerMintedRef, field));
    }
    Ok(())
}

fn reject(category: ForbiddenCategory, field: &'static str) -> AdmissionRejection {
    AdmissionRejection::ForbiddenContent { category, field }
}

/// Scan one intervention text (TB-4 extends to every text-bearing value on
/// the bridge surface, including intervention notes/prompts/reasons).
pub fn scan_intervention_text(
    field: &'static str,
    value: &BoundedText,
) -> Result<(), AdmissionRejection> {
    scan_text(field, value)
}

/// Scan EVERY text-bearing value of an intent (TB-4 check: "over every
/// text-bearing value").
pub fn scan_intent(intent: &TaskIntentV1) -> Result<(), AdmissionRejection> {
    scan_text("objective", &intent.objective)?;
    scan_text("context_bundle_ref", &intent.context_bundle_ref)?;
    for (index, source) in intent.source_refs.iter().enumerate() {
        let _ = index;
        scan_text("source_refs.locator", &source.locator)?;
    }
    for constraint in &intent.constraints {
        scan_text("constraints.description", &constraint.description)?;
    }
    for artifact in &intent.expected_artifacts {
        scan_text("expected_artifacts.description", &artifact.description)?;
    }
    if let Some(workspace) = &intent.workspace_source {
        scan_text("workspace_source.repo", &workspace.repo)?;
        if let Some(git_ref) = &workspace.git_ref {
            scan_text("workspace_source.git_ref", git_ref)?;
        }
    }
    Ok(())
}

/// The outcome of resolving a requester's admitted authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedAuthority {
    /// Capability set the requester's own profile/policy already permits.
    /// Empty means the requester may not request any capability (deny by
    /// default).
    pub capabilities: std::collections::BTreeSet<super::wire::Capability>,
}

impl AdmittedAuthority {
    /// An authority set admitting nothing (deny-by-default).
    pub fn none() -> Self {
        Self {
            capabilities: std::collections::BTreeSet::new(),
        }
    }

    /// An authority set admitting exactly these capabilities.
    pub fn of(capabilities: impl IntoIterator<Item = super::wire::Capability>) -> Self {
        Self {
            capabilities: capabilities.into_iter().collect(),
        }
    }
}

/// Full admission: forbidden content scan, requester admission, and the
/// requester-bounded capability law (TB-4 + TB-5).
///
/// Note what this function does NOT read: the context bundle's content, any
/// guidance text, any Skill/Procedure requirement. Authority-bearing
/// decisions (`capability_request`, `workspace_source`,
/// `routing_preference`, `approval_requirement`) depend only on the intent's
/// own typed fields and the requester's admitted authority — that is the
/// TB-4 seam-law test: differing bundle content cannot change the decision.
pub fn admit(
    intent: &TaskIntentV1,
    authority: &AdmittedAuthority,
) -> Result<(), AdmissionRejection> {
    scan_intent(intent)?;
    if !authority
        .capabilities
        .contains(&intent.capability_request.capability)
    {
        return Err(AdmissionRejection::CapabilityNotAdmitted);
    }
    Ok(())
}

/// Convenience: the requester whose authority must be resolved before
/// calling [`admit`]; resolving it against an identity source is the port
/// caller's job before building [`AdmittedAuthority`].
pub fn requester_of(intent: &TaskIntentV1) -> &RequesterRef {
    &intent.requester
}

#[cfg(test)]
mod tests {
    use super::super::wire::tests::sample_intent;
    use super::super::wire::{Capability, CapabilityRequest};
    use super::*;

    fn authority() -> AdmittedAuthority {
        AdmittedAuthority::of([Capability::ReasoningReview])
    }

    #[test]
    fn clean_intent_admits() {
        let intent = sample_intent(BoundedText::new("review the vertical").expect("bounded"));
        assert_eq!(admit(&intent, &authority()), Ok(()));
    }

    #[test]
    fn per_category_negative_payloads_are_typed_rejections() {
        // TB-4 check: one crafted payload per forbidden category is rejected
        // over text-bearing fields.
        let cases: &[(ForbiddenCategory, &str, &'static str)] = &[
            (
                ForbiddenCategory::Credential,
                "use ghp_0123456789abcdef to push",
                "objective",
            ),
            (
                ForbiddenCategory::Command,
                "ssh host 'cargo test'",
                "objective",
            ),
            (
                ForbiddenCategory::WorktreePath,
                "run in /Users/k/newton/worktrees/x",
                "objective",
            ),
            (
                ForbiddenCategory::PrivateDyad,
                "see private dyad notes",
                "objective",
            ),
            (
                ForbiddenCategory::CallerMintedRef,
                "continue task:abc123 please",
                "objective",
            ),
        ];
        for (category, payload, field) in cases {
            let intent = sample_intent(BoundedText::new(*payload).expect("bounded"));
            assert_eq!(
                admit(&intent, &authority()),
                Err(AdmissionRejection::ForbiddenContent {
                    category: *category,
                    field: "objective",
                }),
                "field {field} category {category:?}"
            );
        }
    }

    #[test]
    fn forbidden_content_rejects_in_every_text_bearing_field() {
        let mut intent = sample_intent(BoundedText::new("clean").expect("bounded"));
        intent.constraints = vec![super::super::wire::TaskConstraint {
            description: BoundedText::new("tmux attach -t main").expect("bounded"),
        }];
        assert_eq!(
            admit(&intent, &authority()),
            Err(AdmissionRejection::ForbiddenContent {
                category: ForbiddenCategory::Command,
                field: "constraints.description",
            })
        );
        let mut intent = sample_intent(BoundedText::new("clean").expect("bounded"));
        intent.workspace_source = Some(super::super::wire::WorkspaceSourceRef {
            repo: BoundedText::new("~/Projects/zeroclaw").expect("bounded"),
            git_ref: None,
        });
        assert_eq!(
            admit(&intent, &authority()),
            Err(AdmissionRejection::ForbiddenContent {
                category: ForbiddenCategory::WorktreePath,
                field: "workspace_source.repo",
            })
        );
    }

    #[test]
    fn capability_exceeding_requester_authority_is_rejected() {
        // TB-5: an intent whose capability_request exceeds the requester's
        // own admitted set fails typed admission regardless of guidance
        // content (the seam is structural: admission never reads guidance).
        let mut intent = sample_intent(BoundedText::new("investigate").expect("bounded"));
        intent.capability_request = CapabilityRequest {
            capability: Capability::ReadOnlyInvestigation,
        };
        assert_eq!(
            admit(&intent, &authority()),
            Err(AdmissionRejection::CapabilityNotAdmitted)
        );
        // ...and admits once the requester's own authority covers it.
        let wider = AdmittedAuthority::of([
            Capability::ReasoningReview,
            Capability::ReadOnlyInvestigation,
        ]);
        assert_eq!(admit(&intent, &wider), Ok(()));
    }

    #[test]
    fn repository_implementation_follows_the_same_requester_bounded_law() {
        // Owner override (TB-5/A surfaced 2026-08-26): the ratified
        // V-program text adds `repository_implementation` as the watershed
        // acceptance capability. The carrier stays the closed enum; the
        // TB-5 intersection law binds the new variant exactly like the
        // other two — deny by default, admit only from the requester's own
        // authority set.
        let mut intent = sample_intent(BoundedText::new("implement the leaf").expect("bounded"));
        intent.capability_request = CapabilityRequest {
            capability: Capability::RepositoryImplementation,
        };
        // Not in the requester's admitted set → typed rejection.
        assert_eq!(
            admit(&intent, &authority()),
            Err(AdmissionRejection::CapabilityNotAdmitted)
        );
        // In the set → admits, and the pre-existing variants keep the
        // identical law against this single-capability authority (no
        // loosening leaked into their intersection check).
        let granted = AdmittedAuthority::of([Capability::RepositoryImplementation]);
        assert_eq!(admit(&intent, &granted), Ok(()));
        let mut read_only = sample_intent(BoundedText::new("investigate").expect("bounded"));
        read_only.capability_request = CapabilityRequest {
            capability: Capability::ReadOnlyInvestigation,
        };
        assert_eq!(
            admit(&read_only, &granted),
            Err(AdmissionRejection::CapabilityNotAdmitted),
            "the pre-existing variants keep the identical intersection law"
        );
        // Forbidden-content law is unchanged for the new capability's
        // intents (write-class capability does not smuggle write-class
        // CONTENT): a command-shaped objective still rejects.
        let mut malicious = intent.clone();
        malicious.objective = BoundedText::new("git push --force origin master").expect("bounded");
        assert_eq!(
            admit(&malicious, &granted),
            Err(AdmissionRejection::ForbiddenContent {
                category: ForbiddenCategory::Command,
                field: "objective",
            })
        );
    }

    #[test]
    fn differing_bundle_content_yields_identical_admission_decision() {
        // TB-4 seam law / DoD admission test: the ONLY difference is
        // bundle/guidance content ⇒ identical admission decision on every
        // authority-bearing field.
        let mut a = sample_intent(BoundedText::new("same objective").expect("bounded"));
        let mut b = sample_intent(BoundedText::new("same objective").expect("bounded"));
        a.context_bundle_ref = BoundedText::new("bundle-aaa").expect("bounded");
        b.context_bundle_ref = BoundedText::new("bundle-zzz").expect("bounded");
        assert_eq!(admit(&a, &authority()), admit(&b, &authority()));
        // And the authority-bearing fields are untouched by the difference:
        assert_eq!(a.capability_request, b.capability_request);
        assert_eq!(a.workspace_source, b.workspace_source);
        assert_eq!(a.routing_preference, b.routing_preference);
        assert_eq!(a.approval_requirement, b.approval_requirement);
    }
}
