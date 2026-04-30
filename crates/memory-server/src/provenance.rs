use super::*;

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_agent_profile(server: &MemoryServer) -> Option<serde_json::Value> {
    let guard = server
        .agent_profile
        .read()
        .unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|profile| {
        json!({
            "agent_id": profile.agent_id,
            "display_name": profile.display_name,
            "capabilities": profile.capabilities,
            "registered_at": profile.registered_at,
        })
    })
}

fn current_db_path(server: &MemoryServer, target_db: DbScope) -> Option<String> {
    match target_db {
        DbScope::Global => Some(server.global_db_path.display().to_string()),
        DbScope::Project => {
            if let Some(path) = server.project_db_path.as_ref() {
                return Some(path.display().to_string());
            }
            let guard = server
                .hot_project_db
                .read()
                .unwrap_or_else(|e| e.into_inner());
            guard
                .as_ref()
                .map(|state| state.db_path.display().to_string())
        }
    }
}

pub(super) fn inject_provenance(
    server: &MemoryServer,
    metadata: serde_json::Value,
    tool_name: &str,
    source_kind: &str,
    requested_scope: Option<&str>,
    target_db: DbScope,
    extra_context: serde_json::Value,
) -> serde_json::Value {
    let mut metadata_obj = match metadata {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("legacy_metadata".into(), other);
            map
        }
    };

    let mut provenance = serde_json::Map::new();
    provenance.insert("captured_at".into(), json!(Utc::now().to_rfc3339()));
    provenance.insert("tool_name".into(), json!(tool_name));
    provenance.insert("source_kind".into(), json!(source_kind));
    provenance.insert("db_scope".into(), json!(target_db.as_str()));

    if let Some(scope) = requested_scope
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
    {
        provenance.insert("requested_scope".into(), json!(scope));
    }

    if let Some(db_path) = current_db_path(server, target_db) {
        provenance.insert("db_path".into(), json!(db_path));
    }

    if let Some(agent) = current_agent_profile(server) {
        provenance.insert("agent".into(), agent);
    }

    for (field, env_key) in [
        ("profile", "TACHI_PROFILE"),
        ("domain", "TACHI_DOMAIN"),
        ("workspace_id", "TACHI_WORKSPACE_ID"),
        ("project_id", "TACHI_PROJECT_ID"),
        ("session_id", "TACHI_SESSION_ID"),
    ] {
        if let Some(value) = non_empty_env(env_key) {
            provenance.insert(field.into(), json!(value));
        }
    }

    if let serde_json::Value::Object(context) = extra_context {
        if !context.is_empty() {
            provenance.insert("context".into(), serde_json::Value::Object(context));
        }
    }

    metadata_obj.insert("provenance".into(), serde_json::Value::Object(provenance));
    serde_json::Value::Object(metadata_obj)
}

/// Restamp the `provenance.db_path` and `provenance.db_scope` fields on a
/// memory entry's metadata to reflect a new destination DB. Used when
/// copying/distilling rows from one DB to another so the destination row
/// correctly reports its location (audit fixes B6 / B11).
///
/// Also records the original source under `provenance.copied_from` so the
/// lineage is preserved.
pub(super) fn restamp_provenance_for_destination(
    metadata: serde_json::Value,
    destination_db_path: &std::path::Path,
    destination_scope: DbScope,
) -> serde_json::Value {
    let mut metadata_obj = match metadata {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("legacy_metadata".into(), other);
            map
        }
    };

    // Pull or create the provenance object.
    let mut provenance = match metadata_obj.remove("provenance") {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };

    // Stash the previous origin (only if not already stashed).
    if !provenance.contains_key("copied_from") {
        let mut copied_from = serde_json::Map::new();
        if let Some(prev) = provenance.get("db_path").cloned() {
            copied_from.insert("db_path".into(), prev);
        }
        if let Some(prev) = provenance.get("db_scope").cloned() {
            copied_from.insert("db_scope".into(), prev);
        }
        copied_from.insert("restamped_at".into(), json!(Utc::now().to_rfc3339()));
        if !copied_from.is_empty() {
            provenance.insert("copied_from".into(), serde_json::Value::Object(copied_from));
        }
    }

    provenance.insert(
        "db_path".into(),
        json!(destination_db_path.display().to_string()),
    );
    provenance.insert("db_scope".into(), json!(destination_scope.as_str()));

    metadata_obj.insert("provenance".into(), serde_json::Value::Object(provenance));
    serde_json::Value::Object(metadata_obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn restamp_replaces_db_path_and_records_origin() {
        let original = json!({
            "provenance": {
                "tool_name": "save_memory",
                "db_path": "/old/global.db",
                "db_scope": "global",
                "captured_at": "2026-01-01T00:00:00Z",
            },
            "user_field": "preserved",
        });
        let dest = PathBuf::from("/new/project.db");
        let restamped = restamp_provenance_for_destination(original, &dest, DbScope::Project);

        let prov = restamped.get("provenance").unwrap();
        assert_eq!(prov.get("db_path").unwrap(), "/new/project.db");
        assert_eq!(prov.get("db_scope").unwrap(), "project");
        let copied = prov.get("copied_from").unwrap();
        assert_eq!(copied.get("db_path").unwrap(), "/old/global.db");
        assert_eq!(copied.get("db_scope").unwrap(), "global");
        // Non-provenance fields preserved.
        assert_eq!(restamped.get("user_field").unwrap(), "preserved");
        // Captured_at preserved on provenance.
        assert_eq!(prov.get("captured_at").unwrap(), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn restamp_idempotent_keeps_original_copied_from() {
        let original = json!({
            "provenance": {
                "db_path": "/origin.db",
                "db_scope": "global",
            }
        });
        let first = restamp_provenance_for_destination(
            original,
            std::path::Path::new("/intermediate.db"),
            DbScope::Project,
        );
        let second = restamp_provenance_for_destination(
            first,
            std::path::Path::new("/final.db"),
            DbScope::Project,
        );
        let prov = second.get("provenance").unwrap();
        assert_eq!(prov.get("db_path").unwrap(), "/final.db");
        // copied_from still points at the *original* origin, not the intermediate.
        assert_eq!(
            prov.get("copied_from").unwrap().get("db_path").unwrap(),
            "/origin.db"
        );
    }

    #[test]
    fn restamp_handles_missing_provenance() {
        let original = json!({ "other": 1 });
        let r = restamp_provenance_for_destination(
            original,
            std::path::Path::new("/x.db"),
            DbScope::Global,
        );
        let prov = r.get("provenance").unwrap();
        assert_eq!(prov.get("db_path").unwrap(), "/x.db");
        assert_eq!(prov.get("db_scope").unwrap(), "global");
    }
}
