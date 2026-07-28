use serde::{Deserialize, Serialize};

// ─── Tachi Event Ledger ABI ─────────────────────────────────────────────────

/// How much authority a captured event has when a downstream projector or
/// adapter decides whether it can affect behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityLevel {
    #[default]
    CollectOnly,
    ReviewSignalOnly,
    InteractionRoutingOnly,
    ToneAndReminderOnly,
    Advisory,
    RawFact,
    DerivedEvidence,
    Blocker,
    ExecutionGate,
}

impl AuthorityLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CollectOnly => "collect_only",
            Self::ReviewSignalOnly => "review_signal_only",
            Self::InteractionRoutingOnly => "interaction_routing_only",
            Self::ToneAndReminderOnly => "tone_and_reminder_only",
            Self::Advisory => "advisory",
            Self::RawFact => "raw_fact",
            Self::DerivedEvidence => "derived_evidence",
            Self::Blocker => "blocker",
            Self::ExecutionGate => "execution_gate",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "review_signal_only" => Self::ReviewSignalOnly,
            "interaction_routing_only" => Self::InteractionRoutingOnly,
            "tone_and_reminder_only" => Self::ToneAndReminderOnly,
            "advisory" => Self::Advisory,
            "raw_fact" => Self::RawFact,
            "derived_evidence" => Self::DerivedEvidence,
            "blocker" => Self::Blocker,
            "execution_gate" => Self::ExecutionGate,
            _ => Self::CollectOnly,
        }
    }

    /// Conservative default: only explicit evidence/gate levels should be
    /// considered eligible for automated decisions by downstream projectors.
    pub fn is_decision_eligible(&self) -> bool {
        matches!(
            self,
            Self::RawFact | Self::DerivedEvidence | Self::Blocker | Self::ExecutionGate
        )
    }
}

impl std::fmt::Display for AuthorityLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Behavioral surface an event may influence after projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectScope {
    None,
    Recall,
    Prompt,
    Routing,
    Tone,
    Scoring,
    Execution,
    MemoryWrite,
    ProjectCycle,
    DomainState,
}

impl EffectScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Recall => "recall",
            Self::Prompt => "prompt",
            Self::Routing => "routing",
            Self::Tone => "tone",
            Self::Scoring => "scoring",
            Self::Execution => "execution",
            Self::MemoryWrite => "memory_write",
            Self::ProjectCycle => "project_cycle",
            Self::DomainState => "domain_state",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "recall" => Self::Recall,
            "prompt" => Self::Prompt,
            "routing" => Self::Routing,
            "tone" => Self::Tone,
            "scoring" | "score" => Self::Scoring,
            "execution" => Self::Execution,
            "memory_write" | "memory" => Self::MemoryWrite,
            "project_cycle" => Self::ProjectCycle,
            "domain_state" | "domain" => Self::DomainState,
            _ => Self::None,
        }
    }
}

impl std::fmt::Display for EffectScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Typed projection families that can consume append-only events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionKind {
    Pattern,
    Timeline,
    Outcome,
    Affect,
    Bonding,
    WorldBook,
    ProjectCycle,
    DomainProfile,
    EvidenceGate,
}

impl ProjectionKind {
    pub const ALL: [Self; 9] = [
        Self::Pattern,
        Self::Timeline,
        Self::Outcome,
        Self::Affect,
        Self::Bonding,
        Self::WorldBook,
        Self::ProjectCycle,
        Self::DomainProfile,
        Self::EvidenceGate,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
            Self::Timeline => "timeline",
            Self::Outcome => "outcome",
            Self::Affect => "affect",
            Self::Bonding => "bonding",
            Self::WorldBook => "world_book",
            Self::ProjectCycle => "project_cycle",
            Self::DomainProfile => "domain_profile",
            Self::EvidenceGate => "evidence_gate",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Option<Self> {
        match s.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "pattern" => Some(Self::Pattern),
            "timeline" => Some(Self::Timeline),
            "outcome" => Some(Self::Outcome),
            "affect" | "emotion" => Some(Self::Affect),
            "bonding" => Some(Self::Bonding),
            "world_book" | "worldbook" => Some(Self::WorldBook),
            "project_cycle" => Some(Self::ProjectCycle),
            "domain_profile" => Some(Self::DomainProfile),
            "evidence_gate" => Some(Self::EvidenceGate),
            _ => None,
        }
    }

    pub fn path_prefix(&self) -> &'static str {
        match self {
            Self::Pattern => "/user/patterns",
            Self::Timeline => "/timeline",
            Self::Outcome => "/outcomes",
            Self::Affect => "/user/affect",
            Self::Bonding => "/user/patterns/bonding",
            Self::WorldBook => "/lorebook",
            Self::ProjectCycle => "/project-cycle",
            Self::DomainProfile => "/domain-profile",
            Self::EvidenceGate => "/evidence-gates",
        }
    }
}

impl std::fmt::Display for ProjectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuityCandidate {
    pub projection: ProjectionKind,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContinuityCandidateBatch {
    #[serde(default)]
    pub candidates: Vec<ContinuityCandidate>,
    #[serde(default)]
    pub open_threads: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContinuityOutcomeLabel {
    #[serde(default)]
    pub outcome: SessionOutcomeKind,
    #[serde(default)]
    pub evidence_basis: OutcomeEvidenceBasis,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub claims: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
}

/// Session-end outcome labels consumed by read-only continuity metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcomeKind {
    #[default]
    Unknown,
    UserCorrect,
    AiCorrected,
    AiError,
    UserError,
    PartialReframe,
    MutualCorrection,
    NoContest,
    Unresolved,
}

impl SessionOutcomeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::UserCorrect => "user_correct",
            Self::AiCorrected => "ai_corrected",
            Self::AiError => "ai_error",
            Self::UserError => "user_error",
            Self::PartialReframe => "partial_reframe",
            Self::MutualCorrection => "mutual_correction",
            Self::NoContest => "no_contest",
            Self::Unresolved => "unresolved",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        let normalized = s
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_");
        match normalized.as_str() {
            "user_correct" | "user_was_correct" | "ai_revised" | "assistant_revised" => {
                Self::UserCorrect
            }
            "ai_corrected"
            | "ai_correct"
            | "assistant_corrected_user"
            | "ai_challenged_user_accepted" => Self::AiCorrected,
            "ai_error" | "assistant_error" | "ai_wrong" | "assistant_wrong" => Self::AiError,
            "user_error" | "user_wrong" => Self::UserError,
            "partial_reframe" | "partial" | "reframe" | "mixed_reframe" => Self::PartialReframe,
            "mutual_correction" | "both_corrected" | "both_partial" | "mixed" => {
                Self::MutualCorrection
            }
            "no_contest" | "no_claim" | "not_applicable" => Self::NoContest,
            "unresolved" | "pending" => Self::Unresolved,
            _ => Self::Unknown,
        }
    }

    /// Denominator for the read-only challenge-rate signal. Unknown,
    /// unresolved, and no-contest labels are deliberately excluded.
    pub fn is_challenge_rate_eligible(&self) -> bool {
        !matches!(self, Self::Unknown | Self::Unresolved | Self::NoContest)
    }
}

impl std::fmt::Display for SessionOutcomeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Evidence axis for outcome labels. This keeps externally anchored revisions
/// separate from labels driven only by conversation pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeEvidenceBasis {
    ExternalEvidence,
    InterlocutorArgument,
    Testimonial,
    Mixed,
    #[default]
    Unverified,
}

impl OutcomeEvidenceBasis {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExternalEvidence => "external_evidence",
            Self::InterlocutorArgument => "interlocutor_argument",
            Self::Testimonial => "testimonial",
            Self::Mixed => "mixed",
            Self::Unverified => "unverified",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        let normalized = s
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_");
        match normalized.as_str() {
            "external_evidence" | "externally_anchored" | "verified" => Self::ExternalEvidence,
            "interlocutor_argument" | "argument" | "conversation_argument" => {
                Self::InterlocutorArgument
            }
            "testimonial" | "self_report" => Self::Testimonial,
            "mixed" => Self::Mixed,
            _ => Self::Unverified,
        }
    }
}

impl std::fmt::Display for OutcomeEvidenceBasis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricCount {
    pub label: String,
    pub count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionOutcomeMetrics {
    pub window_event_limit: usize,
    pub outcome_events: usize,
    pub eligible_outcomes: usize,
    pub ai_corrected: usize,
    pub challenge_rate: Option<f64>,
    pub labels: Vec<MetricCount>,
    pub evidence_basis: Vec<MetricCount>,
    pub note: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContinuityMetrics {
    pub session_outcomes: SessionOutcomeMetrics,
}

/// Append-only, domain-neutral continuity event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TachiEventRecord {
    pub id: String,
    pub source_repo: String,
    pub adapter: String,
    pub project: String,
    pub domain: String,
    pub session_id: String,
    pub actor: String,
    pub event_type: String,
    pub authority: AuthorityLevel,
    pub effects: Vec<EffectScope>,
    pub projection_hints: Vec<ProjectionKind>,
    pub payload: serde_json::Value,
    pub provenance: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TachiEventQuery {
    pub project: Option<String>,
    pub domain: Option<String>,
    pub event_type: Option<String>,
    pub session_id: Option<String>,
    pub source_repo: Option<String>,
    pub adapter: Option<String>,
    pub limit: usize,
}
