use std::path::Path;

use serde_json::{json, Value};

use super::command::build_skill_surface_report;

fn write_skill(root: &Path, name: &str, content: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("SKILL.md"), content).unwrap();
}

fn report(home: &Path, hosts: &[&str]) -> Value {
    serde_json::to_value(
        build_skill_surface_report(
            home,
            &hosts
                .iter()
                .map(|host| host.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn entry<'a>(report: &'a Value, store: &str, name: &str) -> &'a Value {
    report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["store"] == store && entry["name"] == name)
        .unwrap_or_else(|| panic!("missing {store}/{name}: {report}"))
}

#[test]
fn status_reads_antigravity_config_skills_including_native_links() {
    let home = tempfile::tempdir().unwrap();
    let canonical = home.path().join(".agents/skills");
    let native = home.path().join(".gemini/config/skills");
    write_skill(&canonical, "read", "read the canonical Skill\n");
    std::fs::create_dir_all(&native).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(canonical.join("read"), native.join("read")).unwrap();
    #[cfg(not(unix))]
    write_skill(&native, "read", "read the canonical Skill\n");
    write_skill(
        &home.path().join(".gemini/antigravity/skills"),
        "retired-path",
        "not a current discovery root",
    );

    let status = report(home.path(), &["antigravity"]);
    let skill = entry(&status, "antigravity", "read");
    assert_eq!(skill["hash"], entry(&status, "agents", "read")["hash"]);
    #[cfg(unix)]
    assert_eq!(skill["is_symlink"], true);
    assert!(!status["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == "retired-path"));
    assert_eq!(status["summary"]["drift_groups"], 0);
}

#[test]
fn status_separates_factory_plugin_cache_and_user_enablement() {
    let home = tempfile::tempdir().unwrap();
    for (root, name) in [
        (".factory/skills", "local"),
        (".agents/skills", "shared"),
        (".agent/skills", "legacy-shared"),
    ] {
        write_skill(&home.path().join(root), name, "user skill\n");
    }
    let cache = home.path().join(".factory/plugins/cache");
    for plugin in ["enabled", "disabled", "unconfigured"] {
        write_skill(
            &cache.join(format!("example/{plugin}/v1/skills")),
            "shared",
            &format!("independently versioned {plugin} skill\n"),
        );
    }
    let settings_path = home.path().join(".factory/settings.json");
    let settings = json!({"enabledPlugins": {
        "enabled@example": true, "disabled@example": false,
    }})
    .to_string();
    std::fs::write(&settings_path, &settings).unwrap();

    let status = report(home.path(), &["factory"]);
    assert_eq!(status["hosts"], json!(["factory"]));
    for (store, name) in [
        ("factory", "local"),
        ("agents", "shared"),
        ("factory-agent", "legacy-shared"),
    ] {
        assert!(entry(&status, store, name)["hash"].is_string());
        let store = status["stores"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["id"] == store)
            .unwrap();
        assert_eq!(store["hosts"], json!(["factory"]));
    }
    let plugins: Vec<_> = status["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["store"] == "factory-plugins")
        .collect();
    assert_eq!(plugins.len(), 3);
    for (id, expected) in [
        ("enabled@example", json!(true)),
        ("disabled@example", json!(false)),
        ("unconfigured@example", Value::Null),
    ] {
        let plugin = plugins
            .iter()
            .find(|entry| entry["plugin"]["id"] == id)
            .unwrap();
        assert_eq!(plugin["role"], "plugin");
        assert_eq!(plugin["plugin"]["enabled"], expected);
        assert_eq!(plugin["plugin"]["enablement_scope"], "user_settings");
    }
    let plugin_store = status["stores"]
        .as_array()
        .unwrap()
        .iter()
        .find(|store| store["id"] == "factory-plugins")
        .unwrap();
    assert_eq!(plugin_store["read_only"], true);
    assert_eq!(plugin_store["entries"], 3);
    assert_eq!(status["summary"]["drift_groups"], 0);
    assert_eq!(std::fs::read_to_string(settings_path).unwrap(), settings);
}

#[test]
fn status_keeps_factory_enablement_unknown_for_unbound_paths_or_invalid_settings() {
    let home = tempfile::tempdir().unwrap();
    let cache = home.path().join(".factory/plugins/cache");
    write_skill(
        &cache.join("example/plugin/v1/skills"),
        "known-layout",
        "skill",
    );
    write_skill(
        &cache.join("unrecognized/skills"),
        "unknown-layout",
        "skill",
    );
    let settings_path = home.path().join(".factory/settings.json");
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":{"plugin@example":true}}"#,
    )
    .unwrap();
    let status = report(home.path(), &["factory"]);
    let unknown = entry(&status, "factory-plugins", "unknown-layout");
    assert!(unknown["plugin"]["id"].is_null());
    assert!(unknown["plugin"]["enabled"].is_null());

    std::fs::write(&settings_path, "invalid JSON").unwrap();
    let status = report(home.path(), &["factory"]);
    assert!(entry(&status, "factory-plugins", "known-layout")["plugin"]["enabled"].is_null());
    assert!(status["stores"].as_array().unwrap().iter().any(|store| {
        store["id"] == "factory-plugins" && !store["issues"].as_array().unwrap().is_empty()
    }));
    assert_eq!(
        std::fs::read_to_string(settings_path).unwrap(),
        "invalid JSON"
    );
}

const CANONICAL: &str = "---\nname: read\ndescription: |\n  Read evidence before acting.\n  Preserve exact quotations.\nallowed-tools: [Read, Search]\nmetadata:\n  owner: shared\n---\n\n# Read\nUse the current source.\n";
const PROJECTED: &str = "---\nname: read\ndescription: |\n  Read evidence before acting.\n  Preserve exact quotations.\n---\n\n# Read\nUse the current source.\n";

fn codex_fixture(home: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let canonical = home.join(".agents/skills/read");
    let projected = home.join(".codex/skills/read");
    write_skill(canonical.parent().unwrap(), "read", CANONICAL);
    write_skill(projected.parent().unwrap(), "read", PROJECTED);
    std::fs::write(
        projected.join(".agents-codex-projection.json"),
        json!({"source": canonical}).to_string(),
    )
    .unwrap();
    (canonical, projected)
}

#[test]
fn status_recognizes_only_source_bound_codex_frontmatter_projection() {
    let home = tempfile::tempdir().unwrap();
    let (_, projected) = codex_fixture(home.path());
    let status = report(home.path(), &["codex"]);
    let codex = entry(&status, "codex", "read");
    assert_eq!(codex["projection"]["status"], "projected");
    assert_ne!(codex["hash"], entry(&status, "agents", "read")["hash"]);
    assert_eq!(
        codex["comparison_hash"],
        entry(&status, "agents", "read")["hash"]
    );
    assert_eq!(status["summary"]["drift_groups"], 0);

    #[cfg(unix)]
    {
        // The canonical store can itself link to the producer's source tree.
        let canonical = home.path().join(".agents/skills/read");
        let library = home.path().join("central-library/read");
        std::fs::create_dir_all(library.parent().unwrap()).unwrap();
        std::fs::rename(&canonical, &library).unwrap();
        std::os::unix::fs::symlink(&library, &canonical).unwrap();
        std::fs::write(
            projected.join(".agents-codex-projection.json"),
            json!({"source": library}).to_string(),
        )
        .unwrap();
        let status = report(home.path(), &["codex"]);
        assert_eq!(
            entry(&status, "codex", "read")["projection"]["status"],
            "projected"
        );
        assert_eq!(status["summary"]["drift_groups"], 0);
    }

    // A marker pointing to a different skill cannot bless equal-looking bytes.
    let foreign = home.path().join("foreign/read");
    write_skill(foreign.parent().unwrap(), "read", CANONICAL);
    std::fs::write(
        projected.join(".agents-codex-projection.json"),
        json!({"source": foreign}).to_string(),
    )
    .unwrap();
    let status = report(home.path(), &["codex"]);
    assert_eq!(
        entry(&status, "codex", "read")["projection"]["status"],
        "unverified"
    );
    assert_eq!(status["summary"]["drift_groups"], 1);
}

#[test]
fn status_matches_verified_codex_projection_to_the_same_cc_switch_source() {
    let home = tempfile::tempdir().unwrap();
    codex_fixture(home.path());
    write_skill(&home.path().join(".cc-switch/skills"), "read", CANONICAL);
    let conn = rusqlite::Connection::open(home.path().join(".cc-switch/cc-switch.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE skills (name TEXT, directory TEXT, enabled_claude INTEGER,
         enabled_codex INTEGER, enabled_gemini INTEGER, enabled_opencode INTEGER,
         enabled_hermes INTEGER, content_hash TEXT);
         INSERT INTO skills VALUES ('read', 'read', 0, 1, 0, 0, 0, NULL);",
    )
    .unwrap();
    drop(conn);
    let status = report(home.path(), &["codex"]);
    assert_eq!(status["cc_switch_projection_status"][0]["status"], "synced");
    assert_eq!(
        status["cc_switch_projection_status"][0]["checks"][0]["content_matches_cc_switch"],
        true
    );
}

#[test]
fn status_keeps_real_codex_body_and_description_changes_as_drift() {
    let home = tempfile::tempdir().unwrap();
    let (_, projected) = codex_fixture(home.path());
    for changed in [
        PROJECTED.replace("Use the current source.", "Use an old source."),
        PROJECTED.replace("Preserve exact quotations.", "Ignore exact quotations."),
    ] {
        std::fs::write(projected.join("SKILL.md"), changed).unwrap();
        let status = report(home.path(), &["codex"]);
        assert_eq!(
            entry(&status, "codex", "read")["projection"]["status"],
            "drift"
        );
        assert_eq!(status["summary"]["drift_groups"], 1);
    }
}
