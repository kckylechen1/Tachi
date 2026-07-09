use super::types::SkillSourceSyncPlan;

pub(in crate::bootstrap::skill_surface_cli) fn print_skill_source_sync_plan(
    report: &SkillSourceSyncPlan,
) {
    println!("Tachi skill source sync plan");
    println!("  mode: {}  boundary: {}", report.mode, report.boundary);
    println!(
        "  corpora: {}  behind: {}  up-to-date: {}  unavailable: {}",
        report.summary.corpora,
        report.summary.behind,
        report.summary.up_to_date,
        report.summary.unavailable
    );
    println!(
        "  changed files: {}  changed skills: {}  review-required corpora: {}",
        report.summary.changed_files, report.summary.changed_skills, report.summary.review_required
    );
    println!(
        "  high-risk: {}  medium-risk: {}  local-overlay reviews: {}  cards-to-review: {}",
        report.summary.high_risk,
        report.summary.medium_risk,
        report.summary.local_overlay_reviews,
        report.summary.cards_to_review
    );
    println!();

    println!("Corpora:");
    for corpus in &report.corpora {
        println!(
            "  {:<12} {:<24} pinned={} latest={} status={} changed_skills={}",
            corpus.corpus,
            corpus.repo.as_deref().unwrap_or("local"),
            short_sha(corpus.pinned_sha.as_deref()),
            short_sha(corpus.latest_sha.as_deref()),
            corpus.status,
            corpus.changed_skills.len()
        );
        if let Some(error) = &corpus.error {
            println!("    error: {error}");
        }
    }

    let changed = report
        .corpora
        .iter()
        .flat_map(|corpus| {
            corpus
                .changed_skills
                .iter()
                .map(move |skill| (corpus, skill))
        })
        .collect::<Vec<_>>();
    if !changed.is_empty() {
        println!();
        println!("Changed skills:");
        for (corpus, skill) in changed {
            let cards = skill
                .affected_cards
                .iter()
                .map(|card| card.profile.as_str())
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "  {:<12} {:<34} risk={:<6} classes={} cards={}",
                corpus.corpus,
                skill.name,
                skill.risk_level,
                skill.change_classes.join(","),
                if cards.is_empty() { "none" } else { &cards }
            );
            println!("    {}", skill.upstream_path);
        }
    }

    if !report.review_batches.is_empty() {
        println!();
        println!("Review batches:");
        for batch in &report.review_batches {
            let cards = batch
                .affected_cards
                .iter()
                .map(|card| card.profile.as_str())
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "  {:<22} risk={:<6} skills={} cards={}",
                batch.name,
                batch.risk_level,
                batch.skills.len(),
                if cards.is_empty() { "none" } else { &cards }
            );
            println!("    {}", batch.reason);
        }
    }

    if !report.next_actions.is_empty() {
        println!();
        println!("Next actions:");
        for (index, action) in report.next_actions.iter().enumerate() {
            println!("  {}. {}", index + 1, action);
        }
    }

    println!();
    println!("Reviewed sync workflow:");
    for (index, step) in report.review_workflow.iter().enumerate() {
        println!("  {}. {}", index + 1, step);
    }
}

fn short_sha(value: Option<&str>) -> String {
    value
        .map(|sha| sha.chars().take(7).collect())
        .unwrap_or_else(|| "unknown".to_string())
}
