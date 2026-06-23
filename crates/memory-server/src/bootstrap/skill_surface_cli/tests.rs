use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::command::build_skill_surface_report;
use super::sources::{
    build_skill_source_report, parse_skill_source_manifest, skill_source_metadata_status,
};

fn temp_home(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("tachi-{name}-{}-{nanos}", std::process::id()))
}

fn write_skill(root: &Path, name: &str, content: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), content).unwrap();
}

#[test]
fn skill_surface_reports_same_name_hash_drift() {
    let home = temp_home("skill-surface-drift");
    let cc = home.join(".cc-switch").join("skills");
    let codex = home.join(".codex").join("skills");
    std::fs::create_dir_all(&cc).unwrap();
    std::fs::create_dir_all(&codex).unwrap();
    write_skill(&cc, "check", "one");
    write_skill(&codex, "check", "two");

    let report = build_skill_surface_report(&home, &["codex".to_string()]).unwrap();

    assert!(report
        .drift_groups
        .iter()
        .any(|group| group.name == "check"));

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn skill_surface_reads_cc_switch_projection_matrix() {
    let home = temp_home("skill-surface-ccswitch");
    let cc_dir = home.join(".cc-switch").join("skills");
    let claude_dir = home.join(".claude").join("skills");
    std::fs::create_dir_all(&cc_dir).unwrap();
    std::fs::create_dir_all(&claude_dir).unwrap();
    write_skill(&cc_dir, "think", "think skill");

    let db_path = home.join(".cc-switch").join("cc-switch.db");
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE skills (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            directory TEXT NOT NULL,
            enabled_claude BOOLEAN NOT NULL DEFAULT 0,
            enabled_codex BOOLEAN NOT NULL DEFAULT 0,
            enabled_gemini BOOLEAN NOT NULL DEFAULT 0,
            enabled_opencode BOOLEAN NOT NULL DEFAULT 0,
            enabled_hermes BOOLEAN NOT NULL DEFAULT 0,
            content_hash TEXT
        );
        INSERT INTO skills
            (id, name, description, directory, enabled_claude, enabled_codex, enabled_gemini, enabled_opencode, enabled_hermes, content_hash)
        VALUES
            ('local:think', 'think', 'plan', 'think', 1, 0, 0, 0, 0, 'sha');",
    )
    .unwrap();

    let report = build_skill_surface_report(&home, &["claude".to_string()]).unwrap();

    let think = report
        .cc_switch_projection_status
        .iter()
        .find(|status| status.name == "think")
        .unwrap();
    assert_eq!(think.status, "drift");
    assert!(think
        .issues
        .contains(&"missing_projection:claude".to_string()));

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn skill_source_report_reads_builtin_manifest_status() {
    let report = build_skill_source_report().unwrap();

    assert_eq!(report.schema_version, "tachi.skill_surface.sources.v1");
    assert_eq!(report.summary.corpora, 2);
    assert_eq!(report.summary.skills, 16);
    assert_eq!(report.summary.upstream_managed, 13);
    assert_eq!(report.summary.native_contracts, 3);
    assert_eq!(report.summary.missing_metadata, 0);

    let superpowers = report
        .corpora
        .iter()
        .find(|corpus| corpus.corpus == "superpowers")
        .unwrap();
    assert_eq!(
        superpowers.upstream.repo.as_deref(),
        Some("obra/superpowers")
    );
    assert_eq!(
        superpowers.upstream.pinned_sha.as_deref(),
        Some("6fd4507659784c351abbd2bc264c7162cfd386dc")
    );
    assert!(superpowers
        .skills
        .iter()
        .any(|skill| skill.name == "verification-before-completion"
            && skill.metadata_status == "native_contract"));

    let waza = report
        .corpora
        .iter()
        .find(|corpus| corpus.corpus == "waza")
        .unwrap();
    assert!(waza.skills.iter().any(|skill| skill.name == "check"
        && skill.metadata_status == "pinned_upstream"
        && skill.source.local_overlay.as_deref() == Some("tachi-routing-only")));
    assert!(waza.skills.iter().any(|skill| skill.name == "tachi"
        && skill.metadata_status == "native_contract"
        && skill.source.update_policy.as_deref() == Some("local_review")));
}

#[test]
fn skill_source_manifest_parser_flags_missing_metadata() {
    let parsed = parse_skill_source_manifest(
        r#"
schema_version: 1
corpus: sample
upstream:
  repo: example/source
  pinned_ref: main
  pinned_sha: abc
  update_policy: reviewed_sync
skills:
  - id: skill:sample
name: sample
local_path: skill/sample/SKILL.md
source:
  kind: upstream_skill_repo
  repo: example/source
  path: skills/sample/SKILL.md
  pinned_ref: main
"#,
    )
    .unwrap();

    assert_eq!(parsed.skills.len(), 1);
    assert_eq!(
        skill_source_metadata_status(&parsed.skills[0].source),
        "missing_metadata"
    );
}
