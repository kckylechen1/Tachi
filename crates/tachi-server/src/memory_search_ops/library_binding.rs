//! Library binding receipts for agent-facing memory surfaces (#898 / #896 Phase 1).
//!
//! Agents frequently conclude "there is no memory" when the process is
//! global-only (`--no-project-db`) while a workspace repo still has a real
//! `<repo>/.tachi/memory.db`. This module reports **which libraries a call
//! actually addresses** and emits loud, stable warnings when that posture is
//! unsafe for coding sessions.
//!
//! ## Scope (in / out)
//!
//! **In scope:** JSON `tachi_memory` search, briefing (JSON + markdown),
//! `runtime_info`, and standalone `tachi_search` markdown (so agents that only
//! call `tachi_search` still see single_db_mode warnings).

use crate::MemoryServer;
use serde_json::{json, Value};

/// Stable warning id when the process is single-DB while a workspace local DB exists.
pub(crate) const WARN_SINGLE_DB_WITH_WORKSPACE_DB: &str =
    "single_db_mode while workspace has .tachi/memory.db — project memories are invisible unless you pass project=… or restart without --no-project-db";

/// Stable warning when no project library is bound and no named project could be resolved.
pub(crate) const WARN_UNSCOPED_NO_WORKSPACE: &str =
    "unscoped memory session: no project DB bound and no resolvable workspace named project";

/// Stable, greppable prefix for the #925 scope-downgrade warning: a
/// save/checkpoint asked for one `scope` (e.g. `project`) but landed in a
/// different effective `db_scope` (e.g. `global`, because the daemon is
/// single-DB) with no signal in the response. The requested/effective
/// values are call-specific, so only the prefix is a fixed string — see
/// `scope_downgrade_warning` for the full message, mirroring how
/// `WARN_SINGLE_DB_WITH_WORKSPACE_DB` above is a fixed warning id.
pub(crate) const WARN_SCOPE_DOWNGRADED_PREFIX: &str =
    "requested scope was not honored on save/checkpoint";

/// Build the loud, stable warning for a save/checkpoint whose requested
/// `scope` differs from the `db_scope` it actually landed in.
///
/// `session_bound` mirrors the same signal `reject_unbound_cross_project_write`
/// (`session_identity.rs`) gates on: an unbound session has `project=<name>`
/// hard-rejected (-32602) on the very next call, so suggesting it here would
/// send the caller in a circle (#1176). Only a bound session — where the
/// `project=` escape hatch actually works — gets that advice; an unbound
/// session gets pointed at how to *become* bound instead.
pub(crate) fn scope_downgrade_warning(
    requested_scope: &str,
    effective_db_scope: &str,
    session_bound: bool,
) -> String {
    let guidance = if session_bound {
        "pass project=<name> or restart the daemon with a project DB bound to get the requested scope"
    } else {
        // #1176 codex 2.4: `--no-project-db` detaches the daemon's launch cwd
        // to `<app_home>/runtime` (bootstrap/serve.rs: `no_project_serve_detaches_launch_cwd`)
        // and forces `project_db_path = None` regardless of cwd
        // (bootstrap/serve.rs: the `cli.no_project_db` branch of the project-DB
        // resolution), so "run inside the project repo" is not sufficient — the
        // adapter never binds via cwd on that route. Name the flag explicitly
        // rather than imply cwd alone fixes it. Kept short (fix-round for the
        // Oz receipt-golden gate, #1178): the save/checkpoint receipt this
        // string lands in is byte-budgeted (<500B, `receipt_golden.rs`), so
        // this omits the elaboration above and states only the three load-
        // bearing facts — no `project=` advice, the two binding routes, and
        // the `--no-project-db` exclusion.
        "unbound session: writes land in global — connect a bound session via X-Tachi-Project (HTTP) or stdio inside the repo without --no-project-db"
    };
    format!(
        "{WARN_SCOPE_DOWNGRADED_PREFIX}: requested scope={requested_scope:?} but saved to db_scope={effective_db_scope:?} — {guidance}"
    )
}

/// Build the binding receipt for the current server + optional explicit `project=` arg.
pub(crate) fn library_binding_receipt(
    server: &MemoryServer,
    explicit_project: Option<&str>,
) -> Value {
    let global_path = server.global_db_path_buf();
    let project_path = server.project_db_path_buf();
    let single_db_mode = !server.has_project_db();
    let session_project = server.session_project();
    let explicit = explicit_project
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|s| s.to_string());
    let effective_named_project =
        crate::memory_search_ops::resolve_effective_named_project(server, explicit.as_deref());

    let workspace_git_root = crate::utils::find_project_git_root();
    let workspace_local_db = workspace_git_root
        .as_ref()
        .map(|root| root.join(".tachi").join(memcore::MEMORY_DB_FILENAME));
    let workspace_local_db_legacy = workspace_git_root
        .as_ref()
        .map(|root| root.join(".tachi").join(memcore::LEGACY_MEMORY_DB_FILENAME));
    let workspace_local_db_exists = workspace_local_db
        .as_ref()
        .is_some_and(|path| path.exists())
        || workspace_local_db_legacy
            .as_ref()
            .is_some_and(|path| path.exists());
    let workspace_plan_c_alias = workspace_git_root
        .as_ref()
        .and_then(|root| crate::path_utils::plan_c_dir_name_from_root(root));

    let mut warnings: Vec<String> = Vec::new();
    if single_db_mode && workspace_local_db_exists {
        warnings.push(WARN_SINGLE_DB_WITH_WORKSPACE_DB.to_string());
    }
    if single_db_mode
        && effective_named_project.is_none()
        && explicit.is_none()
        && session_project.is_none()
    {
        // Avoid double-warning when the stronger single_db+workspace warning already fired.
        if !workspace_local_db_exists {
            warnings.push(WARN_UNSCOPED_NO_WORKSPACE.to_string());
        }
    }

    let resolved_named_path = effective_named_project.as_ref().and_then(|name| {
        server
            .resolve_server_named_project_db_path(name)
            .ok()
            .map(|path| path.display().to_string())
    });

    json!({
        "global_path": global_path.display().to_string(),
        "project_path": project_path.as_ref().map(|p| p.display().to_string()),
        "single_db_mode": single_db_mode,
        "explicit_project": explicit,
        "session_project": session_project,
        "effective_named_project": effective_named_project,
        "resolved_named_path": resolved_named_path,
        "workspace_git_root": workspace_git_root.as_ref().map(|p| p.display().to_string()),
        "workspace_local_db": workspace_local_db.as_ref().map(|p| p.display().to_string()),
        "workspace_local_db_exists": workspace_local_db_exists,
        "workspace_plan_c_alias": workspace_plan_c_alias,
        "warnings": warnings,
    })
}

/// True when the binding receipt carries the single_db+workspace warning.
#[cfg(test)]
pub(crate) fn receipt_has_single_db_workspace_warning(receipt: &Value) -> bool {
    receipt
        .get("warnings")
        .and_then(Value::as_array)
        .is_some_and(|warnings| {
            warnings.iter().any(|w| {
                w.as_str()
                    .is_some_and(|s| s.contains("single_db_mode while workspace has"))
            })
        })
}

/// The one-line provenance summary of a binding receipt: which posture the
/// process is in and which libraries it addressed.
///
/// This is the line [`format_binding_markdown`] leads with, extracted so a
/// *result* surface that suppresses the full receipt (see
/// [`binding_receipt_is_notable`]) can still carry provenance in one line
/// instead of dropping it silently.
pub(crate) fn binding_summary_line(receipt: &Value) -> String {
    let single = receipt
        .get("single_db_mode")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let project_path = receipt
        .get("project_path")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let effective = receipt
        .get("effective_named_project")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let global = receipt
        .get("global_path")
        .and_then(Value::as_str)
        .unwrap_or("-");
    format!(
        "Library binding: single_db_mode={single} project_path=`{project_path}` effective_named=`{effective}` global=`{global}`"
    )
}

/// Does this receipt carry news — something the caller has to act on — as
/// opposed to routine provenance?
///
/// News is either of:
///   * a warned posture (`warnings` non-empty: the two loud #898 postures and
///     the #925 scope downgrade), or
///   * a global-only process that resolved **no** named library at all, so
///     every row came from global and an explicitly requested library (if any)
///     did not resolve.
///
/// Callers that own a *result* surface add their own outcome-shaped triggers
/// (empty result set, un-honored `scope`); see `facade_memory_ops`.
///
/// Deliberately NOT a trigger: `explicit_project != effective_named_project`
/// compared as raw strings. One physical DB is addressable by any of its four
/// historical alias generations (`path_utils::alias`), so that comparison
/// fires on *spelling*, not on scope — including on every daemon-forwarded CLI
/// call, which addresses the DB by whichever alias generation exists on disk.
/// A genuinely un-honored request shows up as `warnings` or as a null
/// `effective_named_project`, both of which are covered above.
pub(crate) fn binding_receipt_is_notable(receipt: &Value) -> bool {
    let has_warnings = receipt
        .get("warnings")
        .and_then(Value::as_array)
        .is_some_and(|warnings| !warnings.is_empty());
    if has_warnings {
        return true;
    }
    // Both defaults are the reporting side: a receipt whose shape drifted is
    // reported in full rather than silently suppressed.
    let single_db_mode = receipt
        .get("single_db_mode")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let no_named_library = receipt
        .get("effective_named_project")
        .map(Value::is_null)
        .unwrap_or(true);
    single_db_mode && no_named_library
}

/// Format a short markdown block for human briefing surfaces.
pub(crate) fn format_binding_markdown(receipt: &Value) -> String {
    let mut lines = vec![binding_summary_line(receipt)];
    if let Some(warnings) = receipt.get("warnings").and_then(Value::as_array) {
        for w in warnings {
            if let Some(text) = w.as_str() {
                lines.push(format!("[!] binding: {text}"));
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryServer;
    use memcore::MemoryStore;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn workspace_local_db_for_root(root: &Path) -> PathBuf {
        root.join(".tachi").join("memory.db")
    }

    use crate::test_support::EnvRestore;

    #[test]
    fn format_binding_markdown_includes_warnings() {
        let receipt = json!({
            "single_db_mode": true,
            "project_path": null,
            "effective_named_project": null,
            "global_path": "/tmp/global/memory.db",
            "warnings": [WARN_SINGLE_DB_WITH_WORKSPACE_DB],
        });
        let md = format_binding_markdown(&receipt);
        assert!(md.contains("single_db_mode=true"));
        assert!(md.contains("[!] binding:"));
        assert!(md.contains("single_db_mode while workspace has"));
    }

    #[test]
    fn workspace_local_db_path_is_stable() {
        let root = Path::new("/tmp/repo");
        assert_eq!(
            workspace_local_db_for_root(root),
            PathBuf::from("/tmp/repo/.tachi/memory.db")
        );
    }

    /// The summary line is the markdown block's first line — extracting it
    /// must not have changed either surface's bytes.
    #[test]
    fn binding_summary_line_is_the_markdown_first_line() {
        let receipt = json!({
            "single_db_mode": true,
            "project_path": null,
            "effective_named_project": "Quant",
            "global_path": "/tmp/global/memory.db",
            "warnings": [WARN_SINGLE_DB_WITH_WORKSPACE_DB],
        });
        let md = format_binding_markdown(&receipt);
        assert_eq!(
            md.lines().next(),
            Some(binding_summary_line(&receipt).as_str())
        );
        assert!(binding_summary_line(&receipt).contains("effective_named=`Quant`"));
    }

    /// Discrimination for the receipt half of the "no news, no receipt" rule:
    /// a routine bound posture is quiet; a warned posture and an unscoped
    /// global-only posture are both news.
    #[test]
    fn notable_receipt_discriminates_news_from_routine_provenance() {
        let routine = json!({
            "single_db_mode": false,
            "project_path": "/repo/.tachi/tachi-memory.db",
            "effective_named_project": null,
            "global_path": "/tmp/global/memory.db",
            "warnings": [],
        });
        assert!(
            !binding_receipt_is_notable(&routine),
            "a project-bound process with no warnings has nothing to say"
        );

        let named_global_only = json!({
            "single_db_mode": true,
            "project_path": null,
            "effective_named_project": "Sigil-82fc54652d7d59f09166f0e3",
            "global_path": "/tmp/global/memory.db",
            "warnings": [],
        });
        assert!(
            !binding_receipt_is_notable(&named_global_only),
            "a global-only process that DID resolve a named library is the \
             ordinary CLI forwarding path, not news"
        );

        let mut warned = routine.clone();
        warned["warnings"] = json!([WARN_SINGLE_DB_WITH_WORKSPACE_DB]);
        assert!(
            binding_receipt_is_notable(&warned),
            "a warned posture is always news"
        );

        let unscoped = json!({
            "single_db_mode": true,
            "project_path": null,
            "effective_named_project": null,
            "global_path": "/tmp/global/memory.db",
            "warnings": [],
        });
        assert!(
            binding_receipt_is_notable(&unscoped),
            "global-only with no named library resolved is news even when no \
             warning fired (e.g. an explicit project= that did not resolve)"
        );

        assert!(
            binding_receipt_is_notable(&json!({})),
            "a receipt whose shape drifted must report, not suppress"
        );
    }

    #[test]
    fn receipt_warning_detector_matches_stable_phrase() {
        let with = json!({"warnings": [WARN_SINGLE_DB_WITH_WORKSPACE_DB]});
        let without = json!({"warnings": [WARN_UNSCOPED_NO_WORKSPACE]});
        assert!(receipt_has_single_db_workspace_warning(&with));
        assert!(!receipt_has_single_db_workspace_warning(&without));
    }

    /// Discrimination (#898): single_db process + existing workspace local DB
    /// MUST warn. Project-bound process MUST NOT emit that warning.
    #[test]
    fn single_db_mode_warns_when_workspace_local_db_exists_project_bound_does_not() {
        // Must use the crate-wide lock — a private Mutex races other env-mutating tests.
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace-repo");
        std::fs::create_dir_all(workspace.join(".git")).expect("git dir");
        std::fs::create_dir_all(workspace.join(".tachi")).expect("tachi dir");
        let local_db = workspace.join(".tachi/memory.db");
        {
            // Touch a real sqlite file so `exists()` is true.
            let _store = MemoryStore::open(local_db.to_str().expect("utf8"))
                .expect("open workspace local db");
        }

        // Drop-safe restore of TACHI_PROJECT_ROOT even if asserts panic.
        let _project_root = EnvRestore::set_path("TACHI_PROJECT_ROOT", &workspace);

        let global_db = tmp.path().join("global-memory.db");
        {
            let _store =
                MemoryStore::open(global_db.to_str().expect("utf8")).expect("open global db");
        }

        // RED→GREEN for single_db posture.
        let single = MemoryServer::new(global_db.clone(), None).expect("single_db server");
        let single_receipt = library_binding_receipt(&single, None);
        assert_eq!(single_receipt["single_db_mode"], json!(true));
        assert_eq!(single_receipt["workspace_local_db_exists"], json!(true));
        assert!(
            receipt_has_single_db_workspace_warning(&single_receipt),
            "single_db + workspace db must warn; receipt={single_receipt}"
        );

        // Project-bound: same workspace, no single_db warning.
        let project_db = tmp.path().join("project-memory.db");
        {
            let _store =
                MemoryStore::open(project_db.to_str().expect("utf8")).expect("open project db");
        }
        let dual =
            MemoryServer::new(global_db, Some(project_db.clone())).expect("project-bound server");
        let dual_receipt = library_binding_receipt(&dual, None);
        assert_eq!(dual_receipt["single_db_mode"], json!(false));
        assert_eq!(
            dual_receipt["project_path"].as_str(),
            Some(project_db.to_str().expect("utf8"))
        );
        assert!(
            !receipt_has_single_db_workspace_warning(&dual_receipt),
            "project-bound must not emit single_db workspace warning; receipt={dual_receipt}"
        );
    }
}
