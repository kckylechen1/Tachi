use crate::server_state::{DbScope, MemoryServer};
use std::path::PathBuf;

/// #1114 (codex round-2 item 2 fix): `project.is_some()` alone is NOT proof
/// of a caller decision — `session_identity::enforce_session_project`
/// injects the session's bound project onto `capture_session`/
/// `compact_session_memory` args whenever the caller omitted `project=`
/// (same `__tachi_project_explicit`-marker shape as every other
/// project-defaulting write tool, see `write_affinity`'s module doc).
/// Before this fix, `resolve_capture_target` treated ANY `Some(project)` —
/// including that transport default — as an explicit override and returned
/// immediately, skipping the manifest agent-DB pin check entirely: an
/// agent with a manifest pin, captured through a bound session that omitted
/// `project=`, silently bypassed its own pin. `project_explicit` (threaded
/// from the caller's own params, same field/marker `TachiEventParams`/
/// `TachiDomainAdapterParams` already carry) now gates the immediate
/// -return branch — a genuine caller `project=` still wins outright (it's a
/// deliberate placement decision, same posture as the write-affinity gate's
/// own `explicit_project_override`); a transport default falls through to
/// the manifest-pin check first, and only becomes the target if there is no
/// pin.
pub(crate) fn resolve_capture_target(
    server: &MemoryServer,
    requested_scope: &str,
    project: Option<&str>,
    project_explicit: bool,
    agent_id: &str,
) -> (DbScope, Option<String>, Option<PathBuf>, Option<String>) {
    if project_explicit {
        if let Some(project) = project {
            return (DbScope::Project, Some(project.to_string()), None, None);
        }
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

    // No manifest pin — a transport-injected (non-explicit) project default
    // is still the best signal we have for WHICH store, so honor it here,
    // below the pin check.
    if let Some(project) = project {
        return (DbScope::Project, Some(project.to_string()), None, None);
    }

    let (target_db, warning) = server.resolve_write_scope(requested_scope);
    (target_db, None, None, warning)
}
