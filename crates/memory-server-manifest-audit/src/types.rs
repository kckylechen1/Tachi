use serde::Serialize;
use std::path::PathBuf;

/// How a centralized `~/.tachi/projects/<name>/memory.db` is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectDbClass {
    /// A symlink whose target exists. This is the normal, healthy case: the
    /// real DB lives in the repo and the central path is just an alias.
    SymlinkAlias,
    /// A symlink whose target is missing/broken. The central path points
    /// nowhere — candidate for GC of the dangling link (never the target).
    SymlinkBroken,
    /// A real file (not a symlink) for which we found an owning git repo. The
    /// data could be relocated into `<repo>/.tachi/memory.db` and re-linked.
    RealFileWithOwningRepo,
    /// A real file with no discoverable owning repo. Keep it as a home-resident
    /// named project; do NOT move it anywhere.
    RealFileHomeResident,
    /// The `<name>` directory looks like a throwaway UUID/smoke-test workspace.
    /// Candidate for GC of the whole directory.
    UuidSmokeTestGarbage,
}

impl ProjectDbClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectDbClass::SymlinkAlias => "symlink-alias",
            ProjectDbClass::SymlinkBroken => "symlink-broken",
            ProjectDbClass::RealFileWithOwningRepo => "real-file-with-owning-repo",
            ProjectDbClass::RealFileHomeResident => "real-file-home-resident",
            ProjectDbClass::UuidSmokeTestGarbage => "uuid-smoke-test-garbage",
        }
    }
}

/// The proposed action for a single classified entry. PLAN-ONLY: producing a
/// `RelocationItem` never moves or deletes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlannedAction {
    /// Nothing to do — healthy symlink alias.
    KeepAlias,
    /// Move the real file into its owning repo's `.tachi/` and (conceptually)
    /// replace the central path with a symlink. Gated behind `--apply`.
    RelocateToRepo,
    /// Keep as a home-resident named project (no move).
    KeepHomeResident,
    /// Garbage-collect: a broken symlink, or a UUID smoke-test directory.
    GarbageCollect,
}

impl PlannedAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlannedAction::KeepAlias => "keep-alias",
            PlannedAction::RelocateToRepo => "relocate-to-repo",
            PlannedAction::KeepHomeResident => "keep-home-resident",
            PlannedAction::GarbageCollect => "garbage-collect",
        }
    }
}

/// Pre-gathered facts about a single `~/.tachi/projects/<name>/memory.db`.
///
/// Keeping I/O out of the classifier means tests can synthesize any combination
/// (symlink target present/absent, owning repo found/not, uuid-shaped name).
#[derive(Debug, Clone)]
pub struct ProjectDbInput {
    /// The project directory name (the `<name>` segment).
    pub project_name: String,
    /// Absolute path to `~/.tachi/projects/<name>/memory.db`.
    pub db_path: PathBuf,
    /// `true` if `db_path` itself is a symlink.
    pub is_symlink: bool,
    /// If a symlink, the resolved target (whether or not it exists).
    pub symlink_target: Option<PathBuf>,
    /// If a symlink, whether the target path currently exists on disk.
    pub symlink_target_exists: bool,
    /// For a real (non-symlink) file: the discovered owning repo root, if any.
    /// Determined by the caller (manifest match / git-root scan).
    pub owning_repo: Option<PathBuf>,
}

/// A single line of the relocation plan.
#[derive(Debug, Clone, Serialize)]
pub struct RelocationItem {
    pub project_name: String,
    pub db_path: String,
    pub class: ProjectDbClass,
    pub action: PlannedAction,
    /// For `RelocateToRepo`: the proposed destination
    /// `<owning_repo>/.tachi/memory.db`. None otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relocate_to: Option<String>,
    /// For symlink aliases: the resolved target (informational).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    /// Human-readable rationale.
    pub note: String,
}

/// The full plan.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RelocationPlan {
    pub items: Vec<RelocationItem>,
    /// Count of each class, for the summary line.
    pub n_symlink_alias: usize,
    pub n_symlink_broken: usize,
    pub n_relocatable: usize,
    pub n_home_resident: usize,
    pub n_garbage: usize,
}

impl RelocationPlan {
    pub(super) fn tally(&mut self, item: &RelocationItem) {
        match item.class {
            ProjectDbClass::SymlinkAlias => self.n_symlink_alias += 1,
            ProjectDbClass::SymlinkBroken => self.n_symlink_broken += 1,
            ProjectDbClass::RealFileWithOwningRepo => self.n_relocatable += 1,
            ProjectDbClass::RealFileHomeResident => self.n_home_resident += 1,
            ProjectDbClass::UuidSmokeTestGarbage => self.n_garbage += 1,
        }
    }
}
