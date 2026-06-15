use crate::cli::HarnessAction;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const SUPPORTED_HOSTS: &[&str] = &["codex", "claude", "gemini", "antigravity", "cursor"];
const MANAGED_MARKER: &str = "TACHI:HARNESS";

#[derive(Debug, Clone)]
struct HarnessTargetSpec {
    host: &'static str,
    kind: &'static str,
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct HarnessTargetStatus {
    host: String,
    kind: String,
    path: String,
    exists: bool,
    status: String,
    managed: bool,
    mentions_tachi: bool,
    hash: Option<String>,
    bytes: Option<u64>,
    lines: Option<usize>,
    issues: Vec<String>,
    recommended_action: String,
}

#[derive(Debug, Clone, Serialize)]
struct DuplicateGroup {
    hash: String,
    paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct HarnessSummary {
    targets: usize,
    missing: usize,
    managed: usize,
    legacy_tachi: usize,
    unmanaged: usize,
    duplicate_groups: usize,
    issue_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct HarnessStatusReport {
    schema_version: String,
    generated_at: String,
    home: String,
    hosts: Vec<String>,
    summary: HarnessSummary,
    targets: Vec<HarnessTargetStatus>,
    duplicate_groups: Vec<DuplicateGroup>,
}

pub(super) async fn run_harness_command(
    action: HarnessAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        HarnessAction::Status { hosts, home, json } => {
            let home = crate::utils::resolve_home_arg(home)?;
            let report = build_harness_report(&home, &hosts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_harness_report(&report);
            }
            Ok(())
        }
    }
}

fn build_harness_report(
    home: &Path,
    host_filters: &[String],
) -> Result<HarnessStatusReport, String> {
    let hosts =
        crate::utils::normalize_supported_values(host_filters, SUPPORTED_HOSTS, "harness host")?;
    let mut specs = Vec::new();
    for host in &hosts {
        specs.extend(target_specs_for_host(home, host));
    }

    let mut targets: Vec<HarnessTargetStatus> = specs.iter().map(inspect_harness_target).collect();
    let duplicate_groups = duplicate_groups(&targets);
    annotate_duplicate_targets(&mut targets, &duplicate_groups);

    let mut summary = HarnessSummary {
        targets: targets.len(),
        duplicate_groups: duplicate_groups.len(),
        ..HarnessSummary::default()
    };

    for target in &targets {
        match target.status.as_str() {
            "missing" => summary.missing += 1,
            "managed" => summary.managed += 1,
            "legacy_tachi" => summary.legacy_tachi += 1,
            "unmanaged" => summary.unmanaged += 1,
            _ => {}
        }
        summary.issue_count += target.issues.len();
    }

    Ok(HarnessStatusReport {
        schema_version: "tachi.harness.status.v1".to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        home: home.display().to_string(),
        hosts: hosts.iter().map(|h| h.to_string()).collect(),
        summary,
        targets,
        duplicate_groups,
    })
}

fn target_specs_for_host(home: &Path, host: &str) -> Vec<HarnessTargetSpec> {
    match host {
        "codex" => vec![HarnessTargetSpec {
            host: "codex",
            kind: "agent_md",
            path: home.join(".codex").join("AGENTS.md"),
        }],
        "claude" => vec![
            HarnessTargetSpec {
                host: "claude",
                kind: "claude_md",
                path: home.join(".claude").join("CLAUDE.md"),
            },
            HarnessTargetSpec {
                host: "claude",
                kind: "agent_md",
                path: home.join(".claude").join("AGENTS.md"),
            },
        ],
        "gemini" => vec![HarnessTargetSpec {
            host: "gemini",
            kind: "gemini_md",
            path: home.join(".gemini").join("GEMINI.md"),
        }],
        "cursor" => vec![HarnessTargetSpec {
            host: "cursor",
            kind: "cursor_rule",
            path: home.join(".cursor").join("rules").join("tachi-memory.mdc"),
        }],
        "antigravity" => antigravity_specs(home),
        _ => Vec::new(),
    }
}

fn antigravity_specs(home: &Path) -> Vec<HarnessTargetSpec> {
    let gemini_root = home.join(".gemini");
    let mut specs = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&gemini_root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !name.contains("antigravity") {
                continue;
            }
            let path = entry.path().join("global_workflows").join("multi-agent.md");
            specs.push(HarnessTargetSpec {
                host: "antigravity",
                kind: "global_workflow",
                path,
            });
        }
    }

    if specs.is_empty() {
        specs.push(HarnessTargetSpec {
            host: "antigravity",
            kind: "global_workflow",
            path: gemini_root
                .join("antigravity")
                .join("global_workflows")
                .join("multi-agent.md"),
        });
    }

    specs.sort_by(|a, b| a.path.cmp(&b.path));
    specs
}

fn inspect_harness_target(spec: &HarnessTargetSpec) -> HarnessTargetStatus {
    let path = spec.path.display().to_string();
    let Ok(metadata) = std::fs::metadata(&spec.path) else {
        return HarnessTargetStatus {
            host: spec.host.to_string(),
            kind: spec.kind.to_string(),
            path,
            exists: false,
            status: "missing".to_string(),
            managed: false,
            mentions_tachi: false,
            hash: None,
            bytes: None,
            lines: None,
            issues: vec!["missing_target".to_string()],
            recommended_action: "create_managed_projection".to_string(),
        };
    };

    let content = std::fs::read_to_string(&spec.path).unwrap_or_default();
    let lower = content.to_ascii_lowercase();
    let managed = content.contains(MANAGED_MARKER);
    let mentions_tachi = lower.contains("tachi");
    let mut issues = detect_harness_issues(&content);

    let status = if managed {
        "managed"
    } else if mentions_tachi {
        issues.push("legacy_tachi_block_without_managed_marker".to_string());
        "legacy_tachi"
    } else {
        "unmanaged"
    };

    let recommended_action = if managed && issues.is_empty() {
        "none".to_string()
    } else if managed {
        "review_managed_block".to_string()
    } else if mentions_tachi {
        "adopt_marker_bounded_tachi_block".to_string()
    } else {
        "add_marker_bounded_tachi_block".to_string()
    };

    HarnessTargetStatus {
        host: spec.host.to_string(),
        kind: spec.kind.to_string(),
        path,
        exists: true,
        status: status.to_string(),
        managed,
        mentions_tachi,
        hash: Some(crate::utils::stable_hash(&content)),
        bytes: Some(metadata.len()),
        lines: Some(content.lines().count()),
        issues,
        recommended_action,
    }
}

fn detect_harness_issues(content: &str) -> Vec<String> {
    let lower = content.to_ascii_lowercase();
    let mut issues = BTreeSet::new();

    for pattern in [
        "opus 4.6",
        "codex o3",
        "mcp_claude-code",
        "mcp_gemini-cli",
        "tachi_task(action=\"plan\")",
        "tachi_task(action='plan')",
    ] {
        if lower.contains(pattern) {
            issues.insert("stale_or_host_specific_hardcode".to_string());
        }
    }

    if lower.contains("do not directly use run_command")
        || lower.contains("must not directly use run_command")
    {
        issues.insert("host_specific_command_policy".to_string());
    }

    issues.into_iter().collect()
}

fn duplicate_groups(targets: &[HarnessTargetStatus]) -> Vec<DuplicateGroup> {
    let mut by_hash: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for target in targets {
        if let Some(hash) = target.hash.as_deref() {
            by_hash
                .entry(hash.to_string())
                .or_default()
                .push(target.path.clone());
        }
    }

    by_hash
        .into_iter()
        .filter_map(|(hash, paths)| {
            if paths.len() > 1 {
                Some(DuplicateGroup { hash, paths })
            } else {
                None
            }
        })
        .collect()
}

fn annotate_duplicate_targets(targets: &mut [HarnessTargetStatus], groups: &[DuplicateGroup]) {
    let duplicate_paths: BTreeSet<&str> = groups
        .iter()
        .flat_map(|group| group.paths.iter().map(String::as_str))
        .collect();
    for target in targets {
        if duplicate_paths.contains(target.path.as_str()) {
            target.issues.push("duplicate_content".to_string());
            if target.recommended_action == "none" {
                target.recommended_action = "deduplicate_or_confirm_shared_projection".to_string();
            }
        }
    }
}

fn print_harness_report(report: &HarnessStatusReport) {
    println!("Tachi harness status");
    println!("  home: {}", report.home);
    println!("  hosts: {}", report.hosts.join(", "));
    println!(
        "  targets: {} (managed {}, legacy_tachi {}, unmanaged {}, missing {})",
        report.summary.targets,
        report.summary.managed,
        report.summary.legacy_tachi,
        report.summary.unmanaged,
        report.summary.missing
    );
    println!(
        "  issues: {} across {} duplicate group(s)",
        report.summary.issue_count, report.summary.duplicate_groups
    );
    println!();

    for target in &report.targets {
        let hash = target.hash.as_deref().unwrap_or("-");
        let lines = target
            .lines
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<12} {:<16} {:<13} lines={:<4} hash={}  {}",
            target.host, target.kind, target.status, lines, hash, target.path
        );
        if !target.issues.is_empty() {
            println!("  issues: {}", target.issues.join(", "));
        }
        if target.recommended_action != "none" {
            println!("  action: {}", target.recommended_action);
        }
    }

    if !report.duplicate_groups.is_empty() {
        println!();
        println!("Duplicate content groups:");
        for group in &report.duplicate_groups {
            println!("  {}", group.hash);
            for path in &group.paths {
                println!("    - {path}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_home(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("tachi-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn harness_status_detects_legacy_and_duplicate_antigravity_workflows() {
        let home = temp_home("harness-status");
        let codex = home.join(".codex");
        let ag = home
            .join(".gemini")
            .join("antigravity")
            .join("global_workflows");
        let ag_backup = home
            .join(".gemini")
            .join("antigravity-backup")
            .join("global_workflows");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::create_dir_all(&ag).unwrap();
        std::fs::create_dir_all(&ag_backup).unwrap();
        std::fs::write(codex.join("AGENTS.md"), "<!-- TACHI:HARNESS:START -->\n").unwrap();
        let stale = "Tachi rules\nAntigravity Opus 4.6\nmcp_claude-code_run\n";
        std::fs::write(ag.join("multi-agent.md"), stale).unwrap();
        std::fs::write(ag_backup.join("multi-agent.md"), stale).unwrap();

        let report =
            build_harness_report(&home, &["codex".to_string(), "antigravity".to_string()]).unwrap();

        assert_eq!(report.summary.managed, 1);
        assert_eq!(report.summary.legacy_tachi, 2);
        assert_eq!(report.duplicate_groups.len(), 1);
        assert!(report.targets.iter().any(|target| target
            .issues
            .contains(&"stale_or_host_specific_hardcode".to_string())));

        let _ = std::fs::remove_dir_all(home);
    }
}
