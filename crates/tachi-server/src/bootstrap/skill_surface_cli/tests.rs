use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::command::build_skill_surface_report;
use super::sources::{
    build_skill_source_report, parse_skill_source_manifest, skill_source_metadata_status,
};
use super::sync_plan::{affected_cards_for_skill, classify_skill_patch};
use super::SkillSourceMetadata;

fn temp_home(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    crate::utils::test_fixture_path(format!("tachi-{name}-{}-{nanos}", std::process::id()))
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
    // #909 hermeticity follow-up (tachi#911): `build_skill_source_report()`
    // resolves each `SKILL_SOURCE_MANIFESTS` entry through
    // `skill_source_resolver::resolve_vendored_skill_path("skill/<corpus>/manifest.yaml")`. That
    // resolver checks the repo-root-relative path *before* `$TACHI_SKILLS_ROOT`
    // (see the resolver's doc comment / resolution order), and in this
    // repo `skill/` is itself a git-tracked *absolute symlink* into a
    // host-local vendored-skills library (kckylechen1/tachi#895) — so on any
    // host where that symlink target exists, repo-root resolution wins and a
    // `$TACHI_SKILLS_ROOT` fixture can never be observed by this function; on
    // a host where the target is absent, every resolution step fails and the
    // call returns `Err`. Neither path lets this test mount a small fixture
    // without changing the resolver's production resolution order
    // (out of scope here), so instead of asserting exact skill counts/SHAs
    // tied to today's snapshot of the host library, this test:
    //   1. skips (does not fail the suite) when the library isn't mounted, and
    //   2. otherwise asserts only structural invariants that must hold for
    //      any valid manifest content — so it no longer breaks when the
    //      vendored corpora are updated upstream or the library is absent.
    // #909 residual (tachi#911 tail sweep): only the specific "optional
    // fixture not mounted" error is skipped gracefully. Any other error
    // (unreadable manifest, malformed YAML) is unexpected and must fail
    // the test loudly rather than being swallowed alongside the expected
    // case — see `is_missing_vendored_manifest_error_...` tests below for
    // the classifier that draws this line.
    let report = match build_skill_source_report() {
        Ok(report) => report,
        Err(e) if super::is_missing_vendored_manifest_error(&e) => {
            eprintln!(
                "skipping skill_source_report_reads_builtin_manifest_status: \
                 vendored-skills library not mounted on this host ({e})"
            );
            return;
        }
        Err(e) => panic!(
            "build_skill_source_report() failed with an unexpected (non-missing-library) \
             error, refusing to silently skip: {e}"
        ),
    };

    assert_eq!(report.schema_version, "tachi.skill_surface.sources.v1");
    assert_eq!(
        report.summary.corpora,
        super::SKILL_SOURCE_MANIFESTS.len(),
        "summary.corpora must track the number of configured manifest specs"
    );
    assert_eq!(
        report.corpora.len(),
        super::SKILL_SOURCE_MANIFESTS.len(),
        "one corpus status entry per configured manifest spec"
    );

    let corpus_names: Vec<&str> = report.corpora.iter().map(|c| c.corpus.as_str()).collect();
    assert!(
        corpus_names.contains(&"superpowers"),
        "expected a superpowers corpus entry, got {corpus_names:?}"
    );
    assert!(
        corpus_names.contains(&"waza"),
        "expected a waza corpus entry, got {corpus_names:?}"
    );

    const KNOWN_METADATA_STATUSES: &[&str] = &[
        "pinned_upstream",
        "native_contract",
        "local_or_external",
        "missing_metadata",
    ];
    let mut total_skills = 0usize;
    let mut total_upstream_managed = 0usize;
    let mut total_native_contracts = 0usize;
    let mut total_missing_metadata = 0usize;
    for corpus in &report.corpora {
        assert!(
            !corpus.skills.is_empty(),
            "{} corpus should list at least one skill",
            corpus.corpus
        );
        assert_eq!(
            corpus.summary.skills,
            corpus.skills.len(),
            "{} corpus summary.skills must match the skill list length",
            corpus.corpus
        );
        total_skills += corpus.skills.len();
        total_upstream_managed += corpus.summary.upstream_managed;
        total_native_contracts += corpus.summary.native_contracts;
        total_missing_metadata += corpus.summary.missing_metadata;
        for skill in &corpus.skills {
            assert!(
                KNOWN_METADATA_STATUSES.contains(&skill.metadata_status.as_str()),
                "unexpected metadata_status {:?} for skill {} in corpus {}",
                skill.metadata_status,
                skill.name,
                corpus.corpus
            );
            if skill.metadata_status == "missing_metadata" {
                assert!(
                    skill.source.update_policy.is_none()
                        || skill.source.kind.is_none()
                        || (skill.source.kind.as_deref() == Some("upstream_skill_repo")
                            && (skill.source.repo.is_none()
                                || skill.source.path.is_none()
                                || skill.source.pinned_ref.is_none()
                                || skill.source.pinned_sha.is_none())),
                    "missing_metadata skill {} in corpus {} should be missing a required field",
                    skill.name,
                    corpus.corpus
                );
            }
        }
    }
    assert_eq!(
        report.summary.skills, total_skills,
        "top-level summary.skills must equal the sum across all corpora"
    );
    assert_eq!(report.summary.upstream_managed, total_upstream_managed);
    assert_eq!(report.summary.native_contracts, total_native_contracts);
    assert_eq!(report.summary.missing_metadata, total_missing_metadata);
    assert!(
        report.summary.upstream_managed + report.summary.native_contracts <= report.summary.skills,
        "upstream_managed + native_contracts must not exceed total skills"
    );
}

// #909 residual (tachi#911 tail sweep): the classifier must distinguish the
// one *expected* error class (optional vendored-skills library not mounted)
// from everything else, so a real parser/read regression doesn't get
// silently swallowed as "library not mounted."

#[test]
fn missing_vendored_manifest_error_is_recognized_as_expected() {
    let err = "manifest skill/superpowers/manifest.yaml: not found in repo root, cwd, cargo \
               manifest dir, or the central vendored-skills library (set $TACHI_SKILLS_ROOT, \
               or mount ~/.agents/vendored-skills)";
    assert!(super::is_missing_vendored_manifest_error(err));
}

#[test]
fn unrelated_read_and_parse_errors_are_not_treated_as_missing_manifest() {
    let read_err = "read /home/.agents/vendored-skills/skill/waza/manifest.yaml: permission denied";
    let parse_err = "parse skill/waza/manifest.yaml: unexpected token at line 4";
    assert!(!super::is_missing_vendored_manifest_error(read_err));
    assert!(!super::is_missing_vendored_manifest_error(parse_err));
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

#[test]
fn skill_source_sync_plan_classifies_permission_and_evidence_changes_as_high_risk() {
    let source = SkillSourceMetadata {
        kind: Some("upstream_skill_repo".to_string()),
        repo: Some("tw93/Waza".to_string()),
        path: Some("skills/check/SKILL.md".to_string()),
        pinned_ref: Some("main".to_string()),
        pinned_sha: Some("abc".to_string()),
        update_policy: Some("reviewed_sync".to_string()),
        local_overlay: Some("tachi-routing-only".to_string()),
    };
    let risk = classify_skill_patch(
        &source,
        "skills/check/SKILL.md",
        r#"
+allowed_tools: Bash, Read
+Run verification and include evidence before approval.
"#,
    );

    assert_eq!(risk.risk_level, "high");
    assert!(risk
        .change_classes
        .contains(&"tool_permissions".to_string()));
    assert!(risk
        .change_classes
        .contains(&"evidence_contract".to_string()));
    assert!(risk.change_classes.contains(&"local_overlay".to_string()));
}

#[test]
fn skill_source_sync_plan_maps_changed_skill_to_cards() {
    let cards = affected_cards_for_skill("skill:waza-check");

    assert!(cards
        .iter()
        .any(|card| card.profile == "codex_55_review" && card.archetype == "raven"));
}
