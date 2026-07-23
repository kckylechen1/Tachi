//! Discrimination fixtures for #1307 first-slice injection-surface doctor.
//!
//! Each RED fixture asserts an exact `check_kind`. The credential fixture also
//! asserts the secret VALUE string never appears in the serialized report.
//! The boundary test proves the doctor leaves fixture bytes identical.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::report::build_report;

fn temp_root(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "tachi-injection-surface-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_mode(path: &Path, contents: &str, mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(mode);
    use std::io::Write;
    let mut file = opts.open(path).unwrap();
    file.write_all(contents.as_bytes()).unwrap();
}

fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let entries = fs::read_dir(dir).unwrap();
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    walk(root, root, &mut out);
    out
}

fn assert_has_kind(report: &super::InjectionSurfaceReport, kind: &str) {
    let kinds: Vec<_> = report
        .findings
        .iter()
        .map(|f| f.check_kind.as_str())
        .collect();
    assert!(
        report.findings.iter().any(|f| f.check_kind == kind),
        "expected check_kind={kind}, got {kinds:?}"
    );
}

fn plane(
    path: &str,
    scanned: bool,
) -> serde_json::Value {
    serde_json::json!({ "path": path, "scanned": scanned })
}

fn write_registry(home: &Path, harnesses: serde_json::Value) -> PathBuf {
    let path = home.join("fleet-registry.json");
    write(
        &path,
        &serde_json::to_string_pretty(&serde_json::json!({ "harnesses": harnesses })).unwrap(),
    );
    path
}

#[test]
fn injection_surface_red_plugin_corpse() {
    let home = temp_root("plugin-corpse");
    let cache = home.join("fixture-a/plugins/superpowers-cache");
    fs::create_dir_all(&cache).unwrap();
    write(
        &home.join("fixture-a/plugin.json"),
        &serde_json::json!({
            "entries": [{
                "name": "superpowers",
                "installed": true,
                "enabled": false,
                "cache_dir": "fixture-a/plugins/superpowers-cache"
            }],
            "skill_roster": []
        })
        .to_string(),
    );
    let registry = write_registry(
        &home,
        serde_json::json!([{
            "harness_id": "fixture-a",
            "audience": "in-house",
            "expected_tachi_profile": "fixture-a",
            "planes": {
                "plugin": plane("fixture-a/plugin.json", true),
                "mcp": { "scanned": false },
                "credential": { "scanned": false },
                "environment": { "scanned": false },
                "density": { "scanned": false }
            }
        }]),
    );

    let report = build_report(&registry, Some(&home)).unwrap();
    assert_has_kind(&report, "plugin_corpse");
    assert_eq!(
        report
            .findings
            .iter()
            .find(|f| f.check_kind == "plugin_corpse")
            .unwrap()
            .severity,
        "CONCERN"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn injection_surface_red_mcp_identity_split() {
    let home = temp_root("mcp-split");
    write(
        &home.join("fixture-a/mcp.json"),
        &serde_json::json!({
            "registrations": [
                {
                    "name": "longbridge",
                    "scope": "user",
                    "endpoint": "http://user.example/mcp"
                },
                {
                    "name": "longbridge",
                    "scope": "local",
                    "endpoint": "stdio://longbridge"
                }
            ]
        })
        .to_string(),
    );
    let registry = write_registry(
        &home,
        serde_json::json!([{
            "harness_id": "fixture-a",
            "expected_tachi_profile": "fixture-a",
            "planes": {
                "mcp": plane("fixture-a/mcp.json", true),
                "plugin": { "scanned": false },
                "credential": { "scanned": false },
                "environment": { "scanned": false },
                "density": { "scanned": false }
            }
        }]),
    );

    let report = build_report(&registry, Some(&home)).unwrap();
    assert_has_kind(&report, "mcp_identity_split");
    assert_eq!(
        report
            .findings
            .iter()
            .find(|f| f.check_kind == "mcp_identity_split")
            .unwrap()
            .severity,
        "BUG"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn injection_surface_red_tachi_profile_mismatch() {
    let home = temp_root("profile-mismatch");
    write(
        &home.join("fixture-zcode/env.json"),
        &serde_json::json!({
            "tachi_profile": "opencode",
            "injected_paths": []
        })
        .to_string(),
    );
    let registry = write_registry(
        &home,
        serde_json::json!([{
            "harness_id": "fixture-zcode",
            "expected_tachi_profile": "zcode",
            "planes": {
                "environment": plane("fixture-zcode/env.json", true),
                "mcp": { "scanned": false },
                "plugin": { "scanned": false },
                "credential": { "scanned": false },
                "density": { "scanned": false }
            }
        }]),
    );

    let report = build_report(&registry, Some(&home)).unwrap();
    assert_has_kind(&report, "tachi_profile_mismatch");
    assert_eq!(
        report
            .findings
            .iter()
            .find(|f| f.check_kind == "tachi_profile_mismatch")
            .unwrap()
            .severity,
        "BUG"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn injection_surface_red_credential_world_readable() {
    let home = temp_root("cred-world");
    let secret_value = "SUPER_SECRET_VALUE_DO_NOT_LEAK_9f3c2a";
    let cred_path = home.join("fixture-a/api_key.credentials.json");
    write_mode(
        &cred_path,
        &format!(r#"{{"apiKey":"{secret_value}"}}"#),
        0o644,
    );
    let registry = write_registry(
        &home,
        serde_json::json!([{
            "harness_id": "fixture-a",
            "expected_tachi_profile": "fixture-a",
            "planes": {
                "credential": plane("fixture-a/api_key.credentials.json", true),
                "mcp": { "scanned": false },
                "plugin": { "scanned": false },
                "environment": { "scanned": false },
                "density": { "scanned": false }
            }
        }]),
    );

    let report = build_report(&registry, Some(&home)).unwrap();
    assert_has_kind(&report, "credential_world_readable");
    let finding = report
        .findings
        .iter()
        .find(|f| f.check_kind == "credential_world_readable")
        .unwrap();
    assert_eq!(finding.severity, "BUG");
    assert!(finding.evidence_path.contains("mode=0o644") || finding.evidence_path.contains("mode=644"));
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(
        !serialized.contains(secret_value),
        "credential VALUE must not appear in report"
    );
    assert!(!finding.evidence_path.contains(secret_value));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn injection_surface_red_env_ghost() {
    let home = temp_root("env-ghost");
    write(
        &home.join("fixture-a/env.json"),
        &serde_json::json!({
            "tachi_profile": "fixture-a",
            "injected_contracts": [{ "path": "Desktop/retired-project" }]
        })
        .to_string(),
    );
    let registry = write_registry(
        &home,
        serde_json::json!([{
            "harness_id": "fixture-a",
            "expected_tachi_profile": "fixture-a",
            "retired_path_prefixes": ["Desktop/"],
            "planes": {
                "environment": plane("fixture-a/env.json", true),
                "mcp": { "scanned": false },
                "plugin": { "scanned": false },
                "credential": { "scanned": false },
                "density": { "scanned": false }
            }
        }]),
    );

    let report = build_report(&registry, Some(&home)).unwrap();
    assert_has_kind(&report, "env_ghost");
    assert_eq!(
        report
            .findings
            .iter()
            .find(|f| f.check_kind == "env_ghost")
            .unwrap()
            .severity,
        "CONCERN"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn injection_surface_red_skill_sweep_in() {
    let home = temp_root("skill-sweep");
    let foreign_cache = home.join("fixture-b/plugins/superpowers-cache");
    fs::create_dir_all(foreign_cache.join("skills/hijack")).unwrap();
    write(
        &home.join("fixture-a/plugin.json"),
        &serde_json::json!({
            "entries": [],
            "skill_roster": [
                "fixture-b/plugins/superpowers-cache/skills/hijack"
            ]
        })
        .to_string(),
    );
    write(
        &home.join("fixture-b/plugin.json"),
        &serde_json::json!({
            "entries": [{
                "name": "superpowers",
                "installed": true,
                "enabled": true,
                "cache_dir": "fixture-b/plugins/superpowers-cache"
            }],
            "skill_roster": []
        })
        .to_string(),
    );
    let registry = write_registry(
        &home,
        serde_json::json!([
            {
                "harness_id": "fixture-a",
                "expected_tachi_profile": "fixture-a",
                "planes": {
                    "plugin": plane("fixture-a/plugin.json", true),
                    "mcp": { "scanned": false },
                    "credential": { "scanned": false },
                    "environment": { "scanned": false },
                    "density": { "scanned": false }
                }
            },
            {
                "harness_id": "fixture-b",
                "expected_tachi_profile": "fixture-b",
                "planes": {
                    "plugin": plane("fixture-b/plugin.json", true),
                    "mcp": { "scanned": false },
                    "credential": { "scanned": false },
                    "environment": { "scanned": false },
                    "density": { "scanned": false }
                }
            }
        ]),
    );

    let report = build_report(&registry, Some(&home)).unwrap();
    assert_has_kind(&report, "skill_sweep_in");
    let finding = report
        .findings
        .iter()
        .find(|f| f.check_kind == "skill_sweep_in")
        .unwrap();
    assert_eq!(finding.harness_id, "fixture-a");
    assert_eq!(finding.severity, "BUG");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn injection_surface_green_clean_two_harness_with_unscanned() {
    let home = temp_root("green-clean");
    for id in ["fixture-a", "fixture-b"] {
        write(
            &home.join(format!("{id}/mcp.json")),
            r#"{"registrations":[{"name":"tachi","scope":"user","endpoint":"stdio://tachi"}]}"#,
        );
        write(
            &home.join(format!("{id}/plugin.json")),
            r#"{"entries":[{"name":"local","installed":true,"enabled":true}],"skill_roster":[]}"#,
        );
        write_mode(
            &home.join(format!("{id}/api_key.credentials.json")),
            r#"{"apiKey":"unused-because-metadata-only"}"#,
            0o600,
        );
        write(
            &home.join(format!("{id}/env.json")),
            &serde_json::json!({
                "tachi_profile": id,
                "injected_paths": ["Projects/active"]
            })
            .to_string(),
        );
        write(&home.join(format!("{id}/density.json")), r#"{"bytes":128}"#);
    }

    // fixture-a leaves density deliberately unscanned; fixture-b scans all five.
    let registry = write_registry(
        &home,
        serde_json::json!([
            {
                "harness_id": "fixture-a",
                "audience": "in-house",
                "expected_tachi_profile": "fixture-a",
                "retired_path_prefixes": ["Desktop/"],
                "planes": {
                    "mcp": plane("fixture-a/mcp.json", true),
                    "plugin": plane("fixture-a/plugin.json", true),
                    "credential": plane("fixture-a/api_key.credentials.json", true),
                    "environment": plane("fixture-a/env.json", true),
                    "density": { "path": "fixture-a/density.json", "scanned": false }
                }
            },
            {
                "harness_id": "fixture-b",
                "audience": "in-house",
                "expected_tachi_profile": "fixture-b",
                "retired_path_prefixes": ["Desktop/"],
                "planes": {
                    "mcp": plane("fixture-b/mcp.json", true),
                    "plugin": plane("fixture-b/plugin.json", true),
                    "credential": plane("fixture-b/api_key.credentials.json", true),
                    "environment": plane("fixture-b/env.json", true),
                    "density": {
                        "path": "fixture-b/density.json",
                        "scanned": true,
                        "budget_bytes": 999999
                    }
                }
            }
        ]),
    );

    let report = build_report(&registry, Some(&home)).unwrap();
    assert_eq!(report.summary.harnesses, 2);
    assert!(
        report.findings.is_empty(),
        "expected zero findings, got {:?}",
        report.findings
    );
    assert_eq!(report.summary.planes_scanned, 9);
    assert_eq!(report.summary.planes_unscanned, 1);
    let unscanned: Vec<_> = report
        .plane_accounts
        .iter()
        .filter(|a| a.status == "unscanned")
        .collect();
    assert_eq!(unscanned.len(), 1);
    assert_eq!(unscanned[0].harness_id, "fixture-a");
    assert_eq!(unscanned[0].plane, "density");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn injection_surface_doctor_boundary_leaves_fixture_byte_identical() {
    let home = temp_root("boundary");
    let secret_value = "BOUNDARY_SECRET_VALUE_must_not_be_read_aa11";
    write(
        &home.join("fixture-a/mcp.json"),
        r#"{"registrations":[{"name":"tachi","scope":"user","endpoint":"stdio://tachi"}]}"#,
    );
    write(
        &home.join("fixture-a/plugin.json"),
        r#"{"entries":[],"skill_roster":[]}"#,
    );
    write_mode(
        &home.join("fixture-a/api_key.credentials.json"),
        &format!(r#"{{"token":"{secret_value}"}}"#),
        0o644,
    );
    write(
        &home.join("fixture-a/env.json"),
        r#"{"tachi_profile":"fixture-a","injected_paths":[]}"#,
    );
    write(&home.join("fixture-a/density.json"), r#"{"bytes":1}"#);
    let registry = write_registry(
        &home,
        serde_json::json!([{
            "harness_id": "fixture-a",
            "expected_tachi_profile": "fixture-a",
            "planes": {
                "mcp": plane("fixture-a/mcp.json", true),
                "plugin": plane("fixture-a/plugin.json", true),
                "credential": plane("fixture-a/api_key.credentials.json", true),
                "environment": plane("fixture-a/env.json", true),
                "density": plane("fixture-a/density.json", true)
            }
        }]),
    );

    let before = snapshot_tree(&home);
    let report = build_report(&registry, Some(&home)).unwrap();
    let after = snapshot_tree(&home);

    assert_eq!(
        before, after,
        "doctor must leave fixture files byte-identical"
    );
    // Credential finding is expected (world-readable), but VALUE must stay out.
    assert_has_kind(&report, "credential_world_readable");
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains(secret_value));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn injection_surface_unscanned_is_not_clean() {
    let home = temp_root("unscanned-not-clean");
    let registry = write_registry(
        &home,
        serde_json::json!([{
            "harness_id": "fixture-a",
            "expected_tachi_profile": "fixture-a",
            "planes": {
                "mcp": { "scanned": false },
                "plugin": { "scanned": false },
                "credential": { "scanned": false },
                "environment": { "scanned": false },
                "density": { "scanned": false }
            }
        }]),
    );

    let report = build_report(&registry, Some(&home)).unwrap();
    assert_eq!(report.summary.findings, 0);
    assert_eq!(report.summary.planes_scanned, 0);
    assert_eq!(report.summary.planes_unscanned, 5);
    assert!(report
        .plane_accounts
        .iter()
        .all(|a| a.status == "unscanned"));
    // Zero findings + all unscanned must not be mistaken for a clean fleet.
    assert_ne!(report.summary.planes_unscanned, 0);
    let _ = fs::remove_dir_all(home);
}
