use super::*;

pub(super) fn print_skill_surface_report(report: &SkillSurfaceReport) {
    println!("Tachi skill surface status");
    println!("  home: {}", report.home);
    println!("  hosts: {}", report.hosts.join(", "));
    println!(
        "  stores: {}  entries: {}  symlinks: {}  broken_symlinks: {}",
        report.summary.stores,
        report.summary.entries,
        report.summary.symlinks,
        report.summary.broken_symlinks
    );
    println!(
        "  cc-switch skills: {}  projection issues: {}  drift groups: {}",
        report.summary.cc_switch_skills,
        report.summary.cc_switch_projection_issues,
        report.summary.drift_groups
    );
    println!();

    println!("Stores:");
    for store in &report.stores {
        println!(
            "  {:<10} {:<6} {:<10} exists={} entries={} content={} symlinks={} broken={} missing_files={}  {}",
            store.id,
            store.role,
            store.format,
            store.exists,
            store.entries,
            store.skills_with_content,
            store.symlinks,
            store.broken_symlinks,
            store.missing_skill_files,
            store.path
        );
    }

    let problem_entries: Vec<&SkillEntryStatus> = report
        .entries
        .iter()
        .filter(|entry| !entry.issues.is_empty())
        .collect();
    if !problem_entries.is_empty() {
        println!();
        println!("Entry issues:");
        for entry in problem_entries {
            println!(
                "  {:<10} {:<24} {} ({})",
                entry.store,
                entry.name,
                entry.issues.join(", "),
                entry.path
            );
        }
    }

    if !report.drift_groups.is_empty() {
        println!();
        println!("Same-name hash drift:");
        for group in &report.drift_groups {
            println!("  {}", group.name);
            for hash in &group.hashes {
                println!("    {} -> {}", hash.hash, hash.stores.join(", "));
            }
        }
    }

    let projection_issues: Vec<&CcSwitchProjectionStatus> = report
        .cc_switch_projection_status
        .iter()
        .filter(|status| !status.issues.is_empty())
        .collect();
    if !projection_issues.is_empty() {
        println!();
        println!("CC Switch projection issues:");
        for status in projection_issues {
            println!("  {:<24} {}", status.name, status.issues.join(", "));
        }
    }
}

pub(super) fn print_skill_source_report(report: &SkillSourceReport) {
    println!("Tachi skill source status");
    println!(
        "  corpora: {}  skills: {}  upstream-managed: {}  native-contracts: {}",
        report.summary.corpora,
        report.summary.skills,
        report.summary.upstream_managed,
        report.summary.native_contracts
    );
    println!(
        "  local-review: {}  missing-metadata: {}",
        report.summary.local_review, report.summary.missing_metadata
    );
    println!("  update check: {SOURCE_STATUS_NOT_CHECKED}");
    println!();

    println!("Corpora:");
    for corpus in &report.corpora {
        println!(
            "  {:<12} {:<24} ref={} sha={} policy={} skills={}",
            corpus.corpus,
            corpus.upstream.repo.as_deref().unwrap_or("local"),
            corpus.upstream.pinned_ref.as_deref().unwrap_or("local"),
            short_sha(corpus.upstream.pinned_sha.as_deref()),
            corpus
                .upstream
                .update_policy
                .as_deref()
                .unwrap_or("unknown"),
            corpus.summary.skills
        );
    }

    println!();
    println!("Skills:");
    for corpus in &report.corpora {
        for skill in &corpus.skills {
            let source = &skill.source;
            println!(
                "  {:<12} {:<34} {:<22} repo={} ref={} sha={} overlay={}",
                corpus.corpus,
                skill.name,
                source.kind.as_deref().unwrap_or("unknown"),
                source.repo.as_deref().unwrap_or("local"),
                source.pinned_ref.as_deref().unwrap_or("local"),
                short_sha(source.pinned_sha.as_deref()),
                source.local_overlay.as_deref().unwrap_or("none")
            );
        }
    }
}

fn short_sha(value: Option<&str>) -> String {
    value
        .map(|sha| sha.chars().take(7).collect())
        .unwrap_or_else(|| "local".to_string())
}
