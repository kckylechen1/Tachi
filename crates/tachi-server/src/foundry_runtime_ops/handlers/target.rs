use crate::server_state::{DbScope, MemoryServer};
use std::path::PathBuf;

pub(crate) fn resolve_capture_target(
    server: &MemoryServer,
    requested_scope: &str,
    explicit_project: Option<&str>,
    agent_id: &str,
) -> (DbScope, Option<String>, Option<PathBuf>, Option<String>) {
    if let Some(project) = explicit_project {
        return (DbScope::Project, Some(project.to_string()), None, None);
    }

    let manifest_path = server.tachi_home_dir().join("manifest.json");
    let manifest = crate::manifest::Manifest::load_or_empty(&manifest_path);
    if let Some(db_path) = manifest.resolve_agent_db_path(agent_id) {
        return (
            DbScope::Project,
            None,
            Some(db_path),
            Some(format!(
                "agent capture pinned to manifest DB for {agent_id}"
            )),
        );
    }

    let (target_db, warning) = server.resolve_write_scope(requested_scope);
    (target_db, None, None, warning)
}
