use super::plan::build_review_batches;
use super::types::*;

fn changed_skill(
    id: &str,
    risk_level: &str,
    local_overlay_review_required: bool,
    affected_cards: Vec<SkillSourceAffectedCard>,
) -> SkillSourceChangedSkill {
    SkillSourceChangedSkill {
        id: id.to_string(),
        name: id.trim_start_matches("skill:").to_string(),
        local_path: format!("skills/{id}/SKILL.md"),
        upstream_path: format!("skills/{id}/SKILL.md"),
        source_kind: Some("upstream_skill_repo".to_string()),
        local_overlay: local_overlay_review_required.then(|| "tachi-routing-only".to_string()),
        metadata_status: "pinned_upstream".to_string(),
        file_status: "M".to_string(),
        change_classes: vec!["tool_permissions".to_string()],
        risk_level: risk_level.to_string(),
        risk_reasons: vec!["permissions changed".to_string()],
        local_overlay_review_required,
        affected_cards,
        recommended_action: "reviewed_sync_pr".to_string(),
    }
}

fn corpus(changed_skills: Vec<SkillSourceChangedSkill>) -> SkillSourceCorpusSyncPlan {
    SkillSourceCorpusSyncPlan {
        corpus: "waza".to_string(),
        repo: Some("tw93/Waza".to_string()),
        manifest_path: "crates/tachi-server/builtin_skills/waza/manifest.yaml".to_string(),
        pinned_ref: Some("main".to_string()),
        pinned_sha: Some("abc".to_string()),
        latest_ref: Some("main".to_string()),
        latest_sha: Some("def".to_string()),
        status: "behind_with_tracked_changes".to_string(),
        error: None,
        changed_files: vec![GitChangedFile {
            path: "skills/check/SKILL.md".to_string(),
            status: "M".to_string(),
        }],
        changed_skills,
        review_required: true,
    }
}

#[test]
fn review_batches_prioritize_local_overlays_and_collect_cards() {
    let raven = SkillSourceAffectedCard {
        profile: "codex_55_review".to_string(),
        display_name: "Codex 5.5 Review".to_string(),
        role: "reviewer".to_string(),
        stage: Some("review".to_string()),
        archetype: "raven".to_string(),
    };
    let corpora = vec![corpus(vec![
        changed_skill("skill:waza-check", "high", true, vec![raven.clone()]),
        changed_skill("skill:waza-hunt", "high", false, Vec::new()),
    ])];

    let batches = build_review_batches(&corpora);

    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].name, "local_overlay_review");
    assert_eq!(batches[0].skills[0].id, "skill:waza-check");
    assert_eq!(batches[0].affected_cards, vec![raven]);
    assert_eq!(batches[1].name, "high_risk_upstream");
    assert_eq!(batches[1].skills[0].id, "skill:waza-hunt");
}
