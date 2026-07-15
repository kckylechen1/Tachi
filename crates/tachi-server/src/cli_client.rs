//! CLI client glue: forward CLI subcommand invocations to a running Tachi
//! daemon when one is detected, otherwise fall back to a transient in-process
//! `MemoryServer` so we use the same code path as the MCP tool handlers.

use std::path::PathBuf;

use memcore::MigrationAuthority;

use crate::MemoryServer;

mod detect;
mod forward;
mod tool_map;
mod transport;

#[cfg(test)]
use detect::semver_triple;
pub(crate) use detect::{
    app_home_from_global_db, daemon_global_db_matches, daemon_is_older_than_current,
    daemon_matches_requested_dbs, daemon_version_matches, detect_daemon,
    detect_daemon_for_global_db, DaemonInfo,
};
#[cfg(test)]
pub(crate) use forward::maybe_forward_write;
pub(crate) use forward::{maybe_forward_server_read, maybe_forward_server_write};
pub(crate) use transport::{
    call_daemon_tool, call_daemon_tool_raw, list_daemon_tools, DaemonCallError,
};

/// True when this process is the long-lived HTTP daemon (not stdio MCP / CLI).
pub(crate) fn is_daemon_process() -> bool {
    std::env::var("TACHI_DAEMON")
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Build a transient in-process `MemoryServer` for one-shot CLI use.
/// Uses the same constructor as the daemon, so capture gate, enrichment,
/// auto-link, etc. all run identically.
pub(crate) fn build_in_process_server(
    global_db: &PathBuf,
    project_db: Option<&PathBuf>,
) -> Result<MemoryServer, Box<dyn std::error::Error>> {
    build_in_process_server_with_migration_authority(
        global_db,
        project_db,
        MigrationAuthority::Deny,
    )
}

/// Build a transient CLI server with an explicitly approved migration capability.
///
/// The caller owns the capability decision. The `remember` command is the only
/// pre-serve CLI route that currently forwards the top-level migration flag;
/// daemon forwarding returns before this constructor is reached.
pub(crate) fn build_in_process_server_with_migration_authority(
    global_db: &PathBuf,
    project_db: Option<&PathBuf>,
    schema_migration: MigrationAuthority,
) -> Result<MemoryServer, Box<dyn std::error::Error>> {
    let server = MemoryServer::new_with_migration_authority(
        global_db.clone(),
        project_db.cloned(),
        schema_migration,
    )?;
    crate::provider_config::bootstrap_provider_runtime(&server);
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn daemon(global: Option<&Path>, project: Option<&Path>) -> DaemonInfo {
        DaemonInfo {
            url: "http://127.0.0.1:6919/mcp".to_string(),
            global_db: global.map(|path| path.display().to_string()),
            project_db: project.map(|path| path.display().to_string()),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            pid: Some(std::process::id() as i64),
        }
    }

    #[test]
    fn daemon_scope_matches_same_global_and_project() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let project = Path::new("/tmp/tachi/project/memory.db");
        let info = daemon(Some(global), Some(project));

        assert!(daemon_matches_requested_dbs(&info, global, Some(project)));
    }

    #[test]
    fn daemon_scope_rejects_different_project_db() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let daemon_project = Path::new("/tmp/tachi/sigil/memory.db");
        let requested_project = Path::new("/tmp/tachi/quant/memory.db");
        let info = daemon(Some(global), Some(daemon_project));

        assert!(!daemon_matches_requested_dbs(
            &info,
            global,
            Some(requested_project)
        ));
    }

    #[test]
    fn daemon_scope_rejects_project_daemon_for_no_project_request() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let project = Path::new("/tmp/tachi/sigil/memory.db");
        let info = daemon(Some(global), Some(project));

        assert!(!daemon_matches_requested_dbs(&info, global, None));
    }

    #[test]
    fn daemon_scope_accepts_global_only_when_both_have_no_project() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let info = daemon(Some(global), None);

        assert!(daemon_matches_requested_dbs(&info, global, None));
    }

    #[test]
    fn daemon_scope_rejects_missing_daemon_project_for_project_request() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let requested_project = Path::new("/tmp/tachi/quant/memory.db");
        let info = daemon(Some(global), None);

        assert!(!daemon_matches_requested_dbs(
            &info,
            global,
            Some(requested_project)
        ));
    }

    #[test]
    fn daemon_call_error_fallback_is_only_safe_before_dispatch() {
        assert!(
            DaemonCallError::BeforeDispatch("handshake failed".to_string())
                .allows_in_process_fallback()
        );
        assert!(!DaemonCallError::AfterDispatch("timeout".to_string()).allows_in_process_fallback());
    }

    #[tokio::test]
    async fn daemon_forward_non_object_args_fall_back_in_process() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let result = maybe_forward_write(global, None, "remember", &vec!["not", "an", "object"])
            .await
            .expect("non-object args should not fail the write path");

        assert!(result.is_none());
    }

    fn daemon_versioned(version: Option<&str>) -> DaemonInfo {
        DaemonInfo {
            url: "http://127.0.0.1:6919/mcp".to_string(),
            global_db: None,
            project_db: None,
            version: version.map(str::to_string),
            pid: Some(1234),
        }
    }

    #[test]
    fn semver_triple_parses_core_and_ignores_suffix() {
        assert_eq!(semver_triple("1.5.6"), Some((1, 5, 6)));
        assert_eq!(semver_triple("1.5.6-rc.1"), Some((1, 5, 6)));
        assert_eq!(semver_triple("1.5.6+build.9"), Some((1, 5, 6)));
        assert_eq!(semver_triple("1.5"), None); // incomplete -> unknown
        assert_eq!(semver_triple("garbage"), None);
    }

    #[test]
    fn daemon_is_older_only_when_strictly_behind_current() {
        let current = env!("CARGO_PKG_VERSION");
        // The running daemon reporting our exact version is NOT older.
        assert!(!daemon_is_older_than_current(&daemon_versioned(Some(
            current
        ))));
        // A clearly ancient version IS older.
        assert!(daemon_is_older_than_current(&daemon_versioned(Some(
            "0.0.1"
        ))));
        // A clearly future version is NOT older (never replace ahead-of-us).
        assert!(!daemon_is_older_than_current(&daemon_versioned(Some(
            "999.0.0"
        ))));
        // Unknown / unparseable / missing -> never treated as older (safe).
        assert!(!daemon_is_older_than_current(&daemon_versioned(Some(
            "weird"
        ))));
        assert!(!daemon_is_older_than_current(&daemon_versioned(None)));
    }
}
