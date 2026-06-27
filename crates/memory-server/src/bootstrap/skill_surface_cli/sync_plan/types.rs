use serde::Serialize;

pub(in crate::bootstrap::skill_surface_cli) const SYNC_PLAN_BOUNDARY: &str =
    "read_only_plan_no_vendored_skill_mutation_no_github_write";

#[derive(Debug, Clone, Serialize, Default)]
pub(in crate::bootstrap::skill_surface_cli) struct SkillSourceSyncSummary {
    pub(in crate::bootstrap::skill_surface_cli) corpora: usize,
    pub(in crate::bootstrap::skill_surface_cli) up_to_date: usize,
    pub(in crate::bootstrap::skill_surface_cli) behind: usize,
    pub(in crate::bootstrap::skill_surface_cli) unavailable: usize,
    pub(in crate::bootstrap::skill_surface_cli) changed_files: usize,
    pub(in crate::bootstrap::skill_surface_cli) changed_skills: usize,
    pub(in crate::bootstrap::skill_surface_cli) review_required: usize,
    pub(in crate::bootstrap::skill_surface_cli) high_risk: usize,
    pub(in crate::bootstrap::skill_surface_cli) medium_risk: usize,
    pub(in crate::bootstrap::skill_surface_cli) local_overlay_reviews: usize,
    pub(in crate::bootstrap::skill_surface_cli) cards_to_review: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::bootstrap::skill_surface_cli) struct SkillSourceSyncPlan {
    pub(in crate::bootstrap::skill_surface_cli) schema_version: String,
    pub(in crate::bootstrap::skill_surface_cli) generated_at: String,
    pub(in crate::bootstrap::skill_surface_cli) mode: String,
    pub(in crate::bootstrap::skill_surface_cli) boundary: String,
    pub(in crate::bootstrap::skill_surface_cli) summary: SkillSourceSyncSummary,
    pub(in crate::bootstrap::skill_surface_cli) corpora: Vec<SkillSourceCorpusSyncPlan>,
    pub(in crate::bootstrap::skill_surface_cli) review_batches: Vec<SkillSourceReviewBatch>,
    pub(in crate::bootstrap::skill_surface_cli) next_actions: Vec<String>,
    pub(in crate::bootstrap::skill_surface_cli) review_workflow: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::bootstrap::skill_surface_cli) struct SkillSourceCorpusSyncPlan {
    pub(in crate::bootstrap::skill_surface_cli) corpus: String,
    pub(in crate::bootstrap::skill_surface_cli) repo: Option<String>,
    pub(in crate::bootstrap::skill_surface_cli) manifest_path: String,
    pub(in crate::bootstrap::skill_surface_cli) pinned_ref: Option<String>,
    pub(in crate::bootstrap::skill_surface_cli) pinned_sha: Option<String>,
    pub(in crate::bootstrap::skill_surface_cli) latest_ref: Option<String>,
    pub(in crate::bootstrap::skill_surface_cli) latest_sha: Option<String>,
    pub(in crate::bootstrap::skill_surface_cli) status: String,
    pub(in crate::bootstrap::skill_surface_cli) error: Option<String>,
    pub(in crate::bootstrap::skill_surface_cli) changed_files: Vec<GitChangedFile>,
    pub(in crate::bootstrap::skill_surface_cli) changed_skills: Vec<SkillSourceChangedSkill>,
    pub(in crate::bootstrap::skill_surface_cli) review_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::bootstrap::skill_surface_cli) struct SkillSourceChangedSkill {
    pub(in crate::bootstrap::skill_surface_cli) id: String,
    pub(in crate::bootstrap::skill_surface_cli) name: String,
    pub(in crate::bootstrap::skill_surface_cli) local_path: String,
    pub(in crate::bootstrap::skill_surface_cli) upstream_path: String,
    pub(in crate::bootstrap::skill_surface_cli) source_kind: Option<String>,
    pub(in crate::bootstrap::skill_surface_cli) local_overlay: Option<String>,
    pub(in crate::bootstrap::skill_surface_cli) metadata_status: String,
    pub(in crate::bootstrap::skill_surface_cli) file_status: String,
    pub(in crate::bootstrap::skill_surface_cli) change_classes: Vec<String>,
    pub(in crate::bootstrap::skill_surface_cli) risk_level: String,
    pub(in crate::bootstrap::skill_surface_cli) risk_reasons: Vec<String>,
    pub(in crate::bootstrap::skill_surface_cli) local_overlay_review_required: bool,
    pub(in crate::bootstrap::skill_surface_cli) affected_cards: Vec<SkillSourceAffectedCard>,
    pub(in crate::bootstrap::skill_surface_cli) recommended_action: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(in crate::bootstrap::skill_surface_cli) struct SkillSourceAffectedCard {
    pub(in crate::bootstrap::skill_surface_cli) profile: String,
    pub(in crate::bootstrap::skill_surface_cli) display_name: String,
    pub(in crate::bootstrap::skill_surface_cli) role: String,
    pub(in crate::bootstrap::skill_surface_cli) stage: Option<String>,
    pub(in crate::bootstrap::skill_surface_cli) archetype: String,
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::bootstrap::skill_surface_cli) struct SkillSourceReviewBatch {
    pub(in crate::bootstrap::skill_surface_cli) name: String,
    pub(in crate::bootstrap::skill_surface_cli) reason: String,
    pub(in crate::bootstrap::skill_surface_cli) risk_level: String,
    pub(in crate::bootstrap::skill_surface_cli) skills: Vec<SkillSourceReviewBatchSkill>,
    pub(in crate::bootstrap::skill_surface_cli) affected_cards: Vec<SkillSourceAffectedCard>,
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::bootstrap::skill_surface_cli) struct SkillSourceReviewBatchSkill {
    pub(in crate::bootstrap::skill_surface_cli) corpus: String,
    pub(in crate::bootstrap::skill_surface_cli) id: String,
    pub(in crate::bootstrap::skill_surface_cli) name: String,
    pub(in crate::bootstrap::skill_surface_cli) local_path: String,
    pub(in crate::bootstrap::skill_surface_cli) upstream_path: String,
    pub(in crate::bootstrap::skill_surface_cli) risk_level: String,
    pub(in crate::bootstrap::skill_surface_cli) change_classes: Vec<String>,
    pub(in crate::bootstrap::skill_surface_cli) local_overlay_review_required: bool,
    pub(in crate::bootstrap::skill_surface_cli) recommended_action: String,
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::bootstrap::skill_surface_cli) struct GitChangedFile {
    pub(in crate::bootstrap::skill_surface_cli) path: String,
    pub(in crate::bootstrap::skill_surface_cli) status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::bootstrap::skill_surface_cli) struct SkillChangeRisk {
    pub(in crate::bootstrap::skill_surface_cli) change_classes: Vec<String>,
    pub(in crate::bootstrap::skill_surface_cli) risk_level: String,
    pub(in crate::bootstrap::skill_surface_cli) risk_reasons: Vec<String>,
}
