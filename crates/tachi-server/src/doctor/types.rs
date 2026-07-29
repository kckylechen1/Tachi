use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DbClassification {
    Healthy,
    VecExtensionMissing,
    WalOrphan,
    Corrupt,
    LegacySchema,
    Placeholder,
    Backup,
}

impl DbClassification {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::VecExtensionMissing => "vec_extension_missing",
            Self::WalOrphan => "wal_orphan",
            Self::Corrupt => "corrupt",
            Self::LegacySchema => "legacy_schema",
            Self::Placeholder => "placeholder",
            Self::Backup => "backup",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Healthy => "✅",
            Self::VecExtensionMissing => "🟡",
            Self::WalOrphan => "🟠",
            Self::Corrupt => "🔴",
            Self::LegacySchema => "🟣",
            Self::Placeholder => "⚪",
            Self::Backup => "📦",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobBreakdown {
    pub total: usize,
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub pending: usize,
    pub other: usize,
}

impl Default for JobBreakdown {
    fn default() -> Self {
        Self {
            total: 0,
            completed: 0,
            skipped: 0,
            failed: 0,
            pending: 0,
            other: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorFinding {
    pub path: String,
    pub classification: DbClassification,
    pub file_size: u64,
    pub has_wal: bool,
    pub mem_count: Option<usize>,
    pub vec_rowid_count: Option<usize>,
    pub none_domain_count: Option<usize>,
    /// #1041 S4/F7: count of rows matching the OPPOSITE-of-registered
    /// -domain keyword vocabulary (a trading-registered store is scanned
    /// for engineering vocabulary and vice versa; see `doctor::cross_domain`)
    /// — an informational cross-domain-suspect tripwire, never a gate.
    /// `None` when the probe wasn't run: non-tachi schema,
    /// corrupt/backup/placeholder classification, the store has NO
    /// registered domain to compare against (there is no defined "foreign"
    /// side to scan for), or the probe itself errored (never folded into
    /// `Some(0)` — see `probe_keyword_suspects`'s doc for why that used to
    /// be a false-clean diagnostic).
    pub cross_domain_suspect_count: Option<usize>,
    /// Up to a handful of matching ids so an operator can spot-check hits.
    /// Empty whenever `cross_domain_suspect_count` is `None` or `0`.
    pub cross_domain_suspect_sample: Vec<String>,
    pub jobs: JobBreakdown,
    pub schema_kind: String, // "tachi" | "openclaw_legacy" | "unknown" | "empty"
    pub error: Option<String>,
    pub scope_hint: String, // global / project:<name> / openclaw-agent:<name> / antigravity / backup / unknown
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoFixAction {
    pub path: String,
    pub action: String,  // quarantine_placeholder | checkpoint_wal_copy
    pub outcome: String, // ok | error | skipped
    /// Human-readable receipt. For `checkpoint_wal_copy` the note includes an
    /// explicit SHM token: `shm=ok`, `shm=absent`,
    /// `shm=copy_failed_proceeded_without` (partial dest removed),
    /// `shm=copy_failed_partial_renamed_incomplete`, or
    /// `shm=copy_failed_partial_remains`. SHM is not durability-equivalent to
    /// WAL; a failed SHM copy does not block the checkpoint.
    ///
    /// `checkpoint_wal_copy` also has two `outcome: "skipped"` shapes, both
    /// fail-closed on a live/undeterminable daemon and both leave
    /// `destination: None`: `"live daemon holds this DB; refuse to make a
    /// torn copy"` (ownership confirmed held) and `"daemon ownership
    /// undetermined (<reason>); refusing to risk a torn copy"` (ownership
    /// could not be determined — lsof missing/erroring, canonicalize
    /// failure, or unsupported platform). The second shape must never be
    /// conflated with "not owned": an undeterminable daemon is treated the
    /// same as a confirmed-live one, not the same as a confirmed-absent one.
    pub note: String,
    /// Usable checkpoint path on success only. Must be `None` on any failure
    /// after a copy was attempted (incomplete destination discarded or
    /// quarantined as `.incomplete`), including when a present source WAL
    /// could not be copied.
    pub destination: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorWarning {
    pub code: String,
    pub path: String,
    pub message: String,
    pub remediation: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub scanned_roots: Vec<String>,
    pub findings: Vec<DoctorFinding>,
    pub physical_stores: Vec<crate::physical_db_identity::PhysicalDbStore>,
    pub summary: SummaryByClass,
    pub warnings: Vec<DoctorWarning>,
    pub auto_fix_actions: Vec<AutoFixAction>,
    pub quarantine_dir: Option<String>,
    pub generated_at: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SummaryByClass {
    pub healthy: usize,
    pub vec_extension_missing: usize,
    pub wal_orphan: usize,
    pub corrupt: usize,
    pub legacy_schema: usize,
    pub placeholder: usize,
    pub backup: usize,
    pub total_databases: usize,
    /// Resolved discovered aliases belonging to a physical store. Unresolved
    /// paths are deliberately excluded. Retained as a compatibility field;
    /// new consumers should prefer `resolved_aliases`.
    pub total_aliases: usize,
    pub resolved_aliases: usize,
    pub unresolved_paths: usize,
    /// All discovered path appearances: resolved aliases plus unresolved
    /// paths.
    pub path_appearances: usize,
    pub total_memories: usize,
    pub total_jobs: usize,
}
