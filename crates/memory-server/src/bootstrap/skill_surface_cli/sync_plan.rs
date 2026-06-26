use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::sources::{parse_skill_source_manifest, skill_source_metadata_status};
use super::*;

const SYNC_PLAN_BOUNDARY: &str = "read_only_plan_no_vendored_skill_mutation_no_github_write";

#[derive(Debug, Clone, Serialize, Default)]
pub(super) struct SkillSourceSyncSummary {
    corpora: usize,
    up_to_date: usize,
    behind: usize,
    unavailable: usize,
    changed_files: usize,
    changed_skills: usize,
    review_required: usize,
    high_risk: usize,
    medium_risk: usize,
    local_overlay_reviews: usize,
    cards_to_review: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SkillSourceSyncPlan {
    schema_version: String,
    generated_at: String,
    mode: String,
    boundary: String,
    summary: SkillSourceSyncSummary,
    corpora: Vec<SkillSourceCorpusSyncPlan>,
    review_batches: Vec<SkillSourceReviewBatch>,
    next_actions: Vec<String>,
    review_workflow: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SkillSourceCorpusSyncPlan {
    corpus: String,
    repo: Option<String>,
    manifest_path: String,
    pinned_ref: Option<String>,
    pinned_sha: Option<String>,
    latest_ref: Option<String>,
    latest_sha: Option<String>,
    status: String,
    error: Option<String>,
    changed_files: Vec<GitChangedFile>,
    changed_skills: Vec<SkillSourceChangedSkill>,
    review_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SkillSourceChangedSkill {
    id: String,
    name: String,
    local_path: String,
    upstream_path: String,
    source_kind: Option<String>,
    local_overlay: Option<String>,
    metadata_status: String,
    file_status: String,
    change_classes: Vec<String>,
    risk_level: String,
    risk_reasons: Vec<String>,
    local_overlay_review_required: bool,
    affected_cards: Vec<SkillSourceAffectedCard>,
    recommended_action: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct SkillSourceAffectedCard {
    pub(super) profile: String,
    pub(super) display_name: String,
    pub(super) role: String,
    pub(super) stage: Option<String>,
    pub(super) archetype: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SkillSourceReviewBatch {
    name: String,
    reason: String,
    risk_level: String,
    skills: Vec<SkillSourceReviewBatchSkill>,
    affected_cards: Vec<SkillSourceAffectedCard>,
}

#[derive(Debug, Clone, Serialize)]
struct SkillSourceReviewBatchSkill {
    corpus: String,
    id: String,
    name: String,
    local_path: String,
    upstream_path: String,
    risk_level: String,
    change_classes: Vec<String>,
    local_overlay_review_required: bool,
    recommended_action: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct GitChangedFile {
    path: String,
    status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkillChangeRisk {
    pub(super) change_classes: Vec<String>,
    pub(super) risk_level: String,
    pub(super) risk_reasons: Vec<String>,
}

pub(super) fn build_skill_source_sync_plan() -> Result<SkillSourceSyncPlan, String> {
    let mut summary = SkillSourceSyncSummary {
        corpora: SKILL_SOURCE_MANIFESTS.len(),
        ..SkillSourceSyncSummary::default()
    };
    let mut corpora = Vec::new();
    let mut affected_cards = BTreeSet::new();

    for spec in SKILL_SOURCE_MANIFESTS {
        let parsed = parse_skill_source_manifest(spec.content)
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

pub(super) fn classify_skill_patch(
    source: &SkillSourceMetadata,
    path: &str,
    patch: &str,
) -> SkillChangeRisk {
    let mut classes = Vec::<String>::new();
    let mut reasons = Vec::<String>::new();
    let lower = patch.to_ascii_lowercase();
    let path_lower = path.to_ascii_lowercase();

    if path_lower.contains("/scripts/")
        || path_lower.ends_with(".sh")
        || lower.contains("```bash")
        || lower.contains("command")
        || lower.contains("subprocess")
    {
        push_unique(&mut classes, "shell_or_scripts");
        reasons.push("change may alter shell/script/tool execution guidance".to_string());
    }
    if contains_any(
        &lower,
        &[
            "allowed_tools",
            "permission",
            "permissions",
            "sandbox",
            "approval",
            "approve",
            "github_write",
            "write actions",
        ],
    ) {
        push_unique(&mut classes, "tool_permissions");
        reasons.push("change mentions permission or approval boundaries".to_string());
    }
    if contains_any(
        &lower,
        &[
            "rm -rf",
            "git clean",
            "delete",
            "destructive",
            "force",
            "cleanup",
            "clean up",
        ],
    ) {
        push_unique(&mut classes, "destructive_safety");
        reasons.push("change touches destructive or cleanup safety language".to_string());
    }
    if contains_any(
        &lower,
        &[
            "evidence",
            "verification",
            "verify",
            "review",
            "reviewer",
            "tests",
            "test ",
            "report",
            "done:",
        ],
    ) {
        push_unique(&mut classes, "evidence_contract");
        reasons.push("change may alter completion, review, or verification evidence".to_string());
    }
    if contains_any(
        &lower,
        &[
            "plan", "dispatch", "subagent", "task", "worker", "ledger", "todo", "workflow",
        ],
    ) {
        push_unique(&mut classes, "lifecycle_guidance");
        reasons.push("change may alter lifecycle or subagent orchestration guidance".to_string());
    }
    if contains_any(
        &lower,
        &["description:", "when_to_use:", "dispatch_intent:", "name:"],
    ) {
        push_unique(&mut classes, "routing_metadata");
        reasons.push("change may alter skill routing or discovery metadata".to_string());
    }
    if path_lower.contains("example") || lower.contains("example") {
        push_unique(&mut classes, "examples");
        reasons.push("change touches examples or sample workflow text".to_string());
    }
    if source.local_overlay.is_some() {
        push_unique(&mut classes, "local_overlay");
        reasons.push("local overlay must be checked against upstream text changes".to_string());
    }
    if source.kind.as_deref() == Some("tachi_native_contract") {
        push_unique(&mut classes, "native_contract");
        reasons.push(
            "Tachi native wrapper must be checked against upstream-equivalent changes".to_string(),
        );
    }
    if classes.is_empty() {
        classes.push("documentation_guidance".to_string());
        reasons.push("skill text changed without a more specific classifier hit".to_string());
    }

    let risk_level = if classes.iter().any(|class| {
        matches!(
            class.as_str(),
            "shell_or_scripts" | "tool_permissions" | "destructive_safety"
        )
    }) {
        "high"
    } else if classes.iter().any(|class| {
        matches!(
            class.as_str(),
            "evidence_contract"
                | "lifecycle_guidance"
                | "routing_metadata"
                | "local_overlay"
                | "native_contract"
        )
    }) {
        "medium"
    } else {
        "low"
    }
    .to_string();

    SkillChangeRisk {
        change_classes: classes,
        risk_level,
        risk_reasons: reasons,
    }
}

pub(super) fn affected_cards_for_skill(skill_id: &str) -> Vec<SkillSourceAffectedCard> {
    crate::dispatch_profile::DISPATCH_PROFILES
        .iter()
        .filter(|profile| {
            crate::dispatch_profile::profile_required_skill_ids(profile)
                .iter()
                .any(|required| required == skill_id)
        })
        .map(|profile| {
            let card = crate::dispatch_profile::profile_json(profile);
            SkillSourceAffectedCard {
                profile: profile.name.to_string(),
                display_name: profile.display_name.to_string(),
                role: profile.role.to_string(),
                stage: profile.stage.map(str::to_string),
                archetype: card
                    .get("card_archetype")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            }
        })
        .collect()
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

struct GitDiffPlan {
    changed_files: Vec<GitChangedFile>,
    patches: BTreeMap<String, String>,
}

fn git_latest_sha(repo_url: &str, ref_name: &str) -> Result<String, String> {
    let candidates = if ref_name.starts_with("refs/") {
        vec![ref_name.to_string()]
    } else {
        vec![
            format!("refs/heads/{ref_name}"),
            format!("refs/tags/{ref_name}"),
            ref_name.to_string(),
        ]
    };

    for candidate in candidates {
        let output = run_git(None, &["ls-remote", repo_url, &candidate])?;
        if let Some(sha) = output
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .find(|value| !value.is_empty())
        {
            return Ok(sha.to_string());
        }
    }

    Err(format!(
        "git ls-remote found no ref '{ref_name}' in {repo_url}"
    ))
}

fn git_changed_files_and_patches(
    repo_url: &str,
    pinned_sha: &str,
    latest_sha: &str,
    tracked_paths: &BTreeSet<String>,
) -> Result<GitDiffPlan, String> {
    let root = create_temp_root("tachi-skill-source-sync")?;
    let result = (|| {
        let repo_dir = root.join("repo");
        let repo_dir_arg = repo_dir.display().to_string();
        run_git(
            None,
            &[
                "clone",
                "--quiet",
                "--filter=blob:none",
                "--no-checkout",
                repo_url,
                &repo_dir_arg,
            ],
        )?;
        run_git(Some(&repo_dir), &["fetch", "--quiet", "origin", pinned_sha])?;
        run_git(Some(&repo_dir), &["fetch", "--quiet", "origin", latest_sha])?;

        let mut diff_args = vec![
            "diff".to_string(),
            "--name-status".to_string(),
            pinned_sha.to_string(),
            latest_sha.to_string(),
            "--".to_string(),
        ];
        diff_args.extend(tracked_paths.iter().cloned());
        let diff_arg_refs = diff_args.iter().map(String::as_str).collect::<Vec<_>>();
        let changed_files = parse_name_status(&run_git(Some(&repo_dir), &diff_arg_refs)?);

        let mut patches = BTreeMap::new();
        for file in &changed_files {
            let patch_args = [
                "diff",
                "--unified=0",
                pinned_sha,
                latest_sha,
                "--",
                file.path.as_str(),
            ];
            let patch = run_git(Some(&repo_dir), &patch_args)?;
            patches.insert(file.path.clone(), patch);
        }

        Ok(GitDiffPlan {
            changed_files,
            patches,
        })
    })();
    let _ = fs::remove_dir_all(&root);
    result
}

fn parse_name_status(output: &str) -> Vec<GitChangedFile> {
    output
        .lines()
        .filter_map(|line| {
            let parts = line.split('\t').collect::<Vec<_>>();
            let status = parts.first()?.trim();
            if status.is_empty() {
                return None;
            }
            let path = if status.starts_with('R') || status.starts_with('C') {
                parts.last()?
            } else {
                parts.get(1)?
            };
            Some(GitChangedFile {
                path: (*path).to_string(),
                status: status.to_string(),
            })
        })
        .collect()
}

fn run_git(cwd: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = ProcessCommand::new("git");
    if let Some(cwd) = cwd {
        command.arg("-C").arg(cwd);
    }
    command.args(args);
    let output = command
        .output()
        .map_err(|e| format!("run git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "git {} failed{}",
            args.join(" "),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn upstream_repo_url(repo: &str) -> String {
    if repo.contains("://") || repo.starts_with("git@") {
        repo.to_string()
    } else {
        format!("https://github.com/{repo}.git")
    }
}

fn create_temp_root(prefix: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system time before UNIX_EPOCH: {e}"))?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&root).map_err(|e| format!("create temp dir {}: {e}", root.display()))?;
    Ok(root)
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|item| item == value) {
        values.push(value.to_string());
    }
}

pub(super) fn print_skill_source_sync_plan(report: &SkillSourceSyncPlan) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
            manifest_path: "crates/memory-server/builtin_skills/waza/manifest.yaml".to_string(),
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
}
