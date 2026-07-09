use std::collections::{BTreeMap, BTreeSet};

use super::super::sources::{parse_skill_source_manifest, skill_source_metadata_status};
use super::super::*;
use super::git::{git_changed_files_and_patches, git_latest_sha, upstream_repo_url, GitDiffPlan};
use super::risk::{affected_cards_for_skill, classify_skill_patch};
use super::types::*;

pub(in crate::bootstrap::skill_surface_cli) fn build_skill_source_sync_plan(
) -> Result<SkillSourceSyncPlan, String> {
    let mut summary = SkillSourceSyncSummary {
        corpora: SKILL_SOURCE_MANIFESTS.len(),
        ..SkillSourceSyncSummary::default()
    };
    let mut corpora = Vec::new();
    let mut affected_cards = BTreeSet::new();

    for spec in SKILL_SOURCE_MANIFESTS {
        let content = read_skill_source_manifest_content(spec)?;
        let parsed = parse_skill_source_manifest(&content)
            .map_err(|e| format!("parse {}: {e}", spec.path))?;
        let corpus = inspect_corpus_sync(spec, parsed);
        accumulate_sync_summary(&mut summary, &corpus, &mut affected_cards);
        corpora.push(corpus);
    }
    summary.cards_to_review = affected_cards.len();
    let review_batches = build_review_batches(&corpora);
    let next_actions = build_next_actions(&summary, &corpora, &review_batches);

    Ok(SkillSourceSyncPlan {
        schema_version: "tachi.skill_surface.sync_plan.v1".to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        mode: "read_only".to_string(),
        boundary: SYNC_PLAN_BOUNDARY.to_string(),
        summary,
        corpora,
        review_batches,
        next_actions,
        review_workflow: vec![
            "Inspect upstream diff for changed skill files.".to_string(),
            "Classify changes by lifecycle guidance, evidence contract, shell/script, permissions, and examples.".to_string(),
            "Run static/SkillSpector-style scan for material external skill changes.".to_string(),
            "Check affected Card loadouts before accepting the sync.".to_string(),
            "Land vendored snapshot and manifest SHA updates only through a reviewed PR.".to_string(),
        ],
    })
}

pub(super) fn build_review_batches(
    corpora: &[SkillSourceCorpusSyncPlan],
) -> Vec<SkillSourceReviewBatch> {
    let mut batches = Vec::new();
    if let Some(batch) = review_batch_for(
        corpora,
        "local_overlay_review",
        "Changed skill has a Tachi local overlay; compare the overlay with upstream before syncing.",
        "high",
        |skill| skill.local_overlay_review_required,
    ) {
        batches.push(batch);
    }
    if let Some(batch) = review_batch_for(
        corpora,
        "high_risk_upstream",
        "Changed upstream skill may alter permissions, shell/script guidance, or destructive-safety boundaries.",
        "high",
        |skill| skill.risk_level == "high" && !skill.local_overlay_review_required,
    ) {
        batches.push(batch);
    }
    if let Some(batch) = review_batch_for(
        corpora,
        "medium_risk_upstream",
        "Changed upstream skill may alter evidence, lifecycle, routing, or native-contract behavior.",
        "medium",
        |skill| skill.risk_level == "medium" && !skill.local_overlay_review_required,
    ) {
        batches.push(batch);
    }
    if let Some(batch) = review_batch_for(
        corpora,
        "low_risk_upstream",
        "Changed upstream skill is currently classified as documentation-only or low-risk guidance.",
        "low",
        |skill| skill.risk_level == "low" && !skill.local_overlay_review_required,
    ) {
        batches.push(batch);
    }
    batches
}

fn review_batch_for(
    corpora: &[SkillSourceCorpusSyncPlan],
    name: &str,
    reason: &str,
    risk_level: &str,
    predicate: impl Fn(&SkillSourceChangedSkill) -> bool,
) -> Option<SkillSourceReviewBatch> {
    let mut skills = Vec::new();
    let mut cards = BTreeMap::<String, SkillSourceAffectedCard>::new();

    for corpus in corpora {
        for skill in &corpus.changed_skills {
            if !predicate(skill) {
                continue;
            }
            for card in &skill.affected_cards {
                cards
                    .entry(card.profile.clone())
                    .or_insert_with(|| card.clone());
            }
            skills.push(SkillSourceReviewBatchSkill {
                corpus: corpus.corpus.clone(),
                id: skill.id.clone(),
                name: skill.name.clone(),
                local_path: skill.local_path.clone(),
                upstream_path: skill.upstream_path.clone(),
                risk_level: skill.risk_level.clone(),
                change_classes: skill.change_classes.clone(),
                local_overlay_review_required: skill.local_overlay_review_required,
                recommended_action: skill.recommended_action.clone(),
            });
        }
    }

    if skills.is_empty() {
        return None;
    }

    Some(SkillSourceReviewBatch {
        name: name.to_string(),
        reason: reason.to_string(),
        risk_level: risk_level.to_string(),
        skills,
        affected_cards: cards.into_values().collect(),
    })
}

fn build_next_actions(
    summary: &SkillSourceSyncSummary,
    corpora: &[SkillSourceCorpusSyncPlan],
    review_batches: &[SkillSourceReviewBatch],
) -> Vec<String> {
    let mut actions = Vec::new();

    if summary.unavailable > 0 {
        let unavailable = corpora
            .iter()
            .filter(|corpus| corpus.status == "unavailable")
            .map(|corpus| corpus.corpus.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        actions.push(format!(
            "Retry unavailable corpora before vendoring updates: {unavailable}."
        ));
    }
    if !review_batches.is_empty() {
        let batches = review_batches
            .iter()
            .map(|batch| batch.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        actions.push(format!(
            "Review batches in order before changing vendored skills: {batches}."
        ));
    }
    if summary.cards_to_review > 0 {
        actions.push(
            "Run affected Card loadout review and `tachi poke run --suite smoke` after any accepted skill sync."
                .to_string(),
        );
    }
    if summary.changed_skills > 0 {
        actions.push(
            "Land vendored skill text and manifest SHA updates only through a reviewed PR."
                .to_string(),
        );
    }
    if actions.is_empty() {
        actions
            .push("No upstream skill sync action is required from the current pins.".to_string());
    }
    actions.push(
        "Keep this command read-only; it must not mutate vendored skills or write GitHub state."
            .to_string(),
    );
    actions
}

fn inspect_corpus_sync(
    spec: &SkillSourceManifestSpec,
    parsed: ParsedSkillSourceManifest,
) -> SkillSourceCorpusSyncPlan {
    let repo = parsed.upstream.repo.clone();
    let pinned_ref = parsed.upstream.pinned_ref.clone();
    let pinned_sha = parsed.upstream.pinned_sha.clone();
    let latest_ref = pinned_ref.clone().or_else(|| Some("main".to_string()));
    let mut plan = SkillSourceCorpusSyncPlan {
        corpus: parsed.corpus.clone(),
        repo: repo.clone(),
        manifest_path: spec.path.to_string(),
        pinned_ref,
        pinned_sha: pinned_sha.clone(),
        latest_ref: latest_ref.clone(),
        latest_sha: None,
        status: "unavailable".to_string(),
        error: None,
        changed_files: Vec::new(),
        changed_skills: Vec::new(),
        review_required: false,
    };

    let Some(repo) = repo else {
        plan.error = Some("manifest upstream repo is missing".to_string());
        return plan;
    };
    let Some(pinned_sha) = pinned_sha else {
        plan.error = Some("manifest upstream pinned_sha is missing".to_string());
        return plan;
    };
    let latest_ref = latest_ref.unwrap_or_else(|| "main".to_string());
    let repo_url = upstream_repo_url(&repo);
    let latest_sha = match git_latest_sha(&repo_url, &latest_ref) {
        Ok(sha) => sha,
        Err(err) => {
            plan.error = Some(err);
            return plan;
        }
    };
    plan.latest_sha = Some(latest_sha.clone());

    if latest_sha == pinned_sha {
        plan.status = "up_to_date".to_string();
        return plan;
    }

    let tracked_paths = tracked_upstream_paths(&repo, &parsed.skills);
    if tracked_paths.is_empty() {
        plan.status = "behind_no_tracked_paths".to_string();
        return plan;
    }

    let diff =
        match git_changed_files_and_patches(&repo_url, &pinned_sha, &latest_sha, &tracked_paths) {
            Ok(diff) => diff,
            Err(err) => {
                plan.error = Some(err);
                return plan;
            }
        };

    plan.status = if diff.changed_files.is_empty() {
        "behind_no_tracked_changes".to_string()
    } else {
        "behind_with_tracked_changes".to_string()
    };
    plan.changed_files = diff.changed_files.clone();
    plan.changed_skills = changed_skills_for_diff(&parsed.skills, &diff);
    plan.review_required = !plan.changed_skills.is_empty();
    plan
}

fn tracked_upstream_paths(repo: &str, skills: &[ParsedSkillSourceEntry]) -> BTreeSet<String> {
    skills
        .iter()
        .filter(|entry| {
            entry.source.repo.as_deref() == Some(repo)
                && matches!(
                    entry.source.kind.as_deref(),
                    Some("upstream_skill_repo") | Some("tachi_native_contract")
                )
        })
        .filter_map(|entry| entry.source.path.clone())
        .collect()
}

fn changed_skills_for_diff(
    skills: &[ParsedSkillSourceEntry],
    diff: &GitDiffPlan,
) -> Vec<SkillSourceChangedSkill> {
    let changed_by_path = diff
        .changed_files
        .iter()
        .map(|file| (file.path.clone(), file.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut changed = Vec::new();

    for entry in skills {
        let Some(path) = entry.source.path.as_deref() else {
            continue;
        };
        let Some(file) = changed_by_path.get(path) else {
            continue;
        };
        let patch = diff
            .patches
            .get(path)
            .map(String::as_str)
            .unwrap_or_default();
        let risk = classify_skill_patch(&entry.source, path, patch);
        let affected_cards = affected_cards_for_skill(&entry.id);
        let local_overlay_review_required = entry.source.local_overlay.is_some();
        changed.push(SkillSourceChangedSkill {
            id: entry.id.clone(),
            name: entry.name.clone(),
            local_path: entry.local_path.clone(),
            upstream_path: path.to_string(),
            source_kind: entry.source.kind.clone(),
            local_overlay: entry.source.local_overlay.clone(),
            metadata_status: skill_source_metadata_status(&entry.source),
            file_status: file.status.clone(),
            change_classes: risk.change_classes,
            risk_level: risk.risk_level,
            risk_reasons: risk.risk_reasons,
            local_overlay_review_required,
            affected_cards,
            recommended_action: "reviewed_sync_pr".to_string(),
        });
    }

    changed
}

fn accumulate_sync_summary(
    summary: &mut SkillSourceSyncSummary,
    corpus: &SkillSourceCorpusSyncPlan,
    affected_cards: &mut BTreeSet<String>,
) {
    match corpus.status.as_str() {
        "up_to_date" => summary.up_to_date += 1,
        "unavailable" => summary.unavailable += 1,
        status if status.starts_with("behind") => summary.behind += 1,
        _ => {}
    }
    summary.changed_files += corpus.changed_files.len();
    summary.changed_skills += corpus.changed_skills.len();
    if corpus.review_required {
        summary.review_required += 1;
    }
    for skill in &corpus.changed_skills {
        match skill.risk_level.as_str() {
            "high" => summary.high_risk += 1,
            "medium" => summary.medium_risk += 1,
            _ => {}
        }
        if skill.local_overlay_review_required {
            summary.local_overlay_reviews += 1;
        }
        for card in &skill.affected_cards {
            affected_cards.insert(card.profile.clone());
        }
    }
}
