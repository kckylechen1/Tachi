//! # Security scope (read before trusting this as multi-tenant identity)
//!
//! C1 (`reject_unbound_cross_project_write`) closes ONE attack: an UNBOUND HTTP
//! direct-connect session (no `X-Tachi-Project` header) targeting
//! `project=victim` via a tool argument. It does NOT authenticate the
//! `X-Tachi-Project` header itself — a direct HTTP client that claims
//! `X-Tachi-Project: victim` at `initialize` still binds to victim's project,
//! because project binding only verifies the project DB file exists
//! (existence ≡ access). Full multi-tenant authorization (binding header claims
//! to an authenticated identity via mTLS / local-only / vault-ACL) is tracked in
//! #495 and is OUT OF SCOPE for the #809 fix. Until #495 lands, treat this
//! module as "unbound-write rejection", NOT a complete identity spine.

use rmcp::model::JsonObject;

pub(crate) const HEADER_PROFILE: &str = "x-tachi-profile";
pub(crate) const HEADER_CLIENT: &str = "x-tachi-client";
pub(crate) const HEADER_PROJECT: &str = "x-tachi-project";

pub(crate) const META_PROFILE: &str = "tachiProfile";
pub(crate) const META_CLIENT: &str = "tachiClient";
pub(crate) const META_PROJECT: &str = "tachiProject";

pub(crate) fn enforce_session_project(
    tool_name: &str,
    arguments: &mut Option<JsonObject>,
    project: &str,
    transport_label: &str,
) -> Result<(), rmcp::ErrorData> {
    let args = arguments.get_or_insert_with(serde_json::Map::new);
    if let Some(explicit_project) = args.get("project") {
        let Some(requested_alias) = explicit_project.as_str().map(str::to_string) else {
            let requested = explicit_project.to_string();
            return Err(project_binding_mismatch(
                transport_label,
                &requested,
                project,
                Some("project must be a string"),
            ));
        };
        let requested_identity =
            crate::MemoryServer::resolve_named_project_db_identity(&requested_alias);
        let bound_identity = crate::MemoryServer::resolve_named_project_db_identity(project);

        match (requested_identity, bound_identity) {
            (Ok(requested_db), Ok(bound_db)) if requested_db == bound_db => {
                // The immutable session identity wins even when the caller used
                // a legacy/simplified alias for the same physical DB. This also
                // prevents downstream code from opening the same DB under a
                // second label.
                args.insert("project".to_string(), serde_json::json!(project));
                return Ok(());
            }
            (Ok(_), Ok(_)) if explicit_project_can_cross_binding(tool_name, args) => {
                // #737 read/write asymmetry: a resolvable different project is
                // still allowed only for the established read-only actions.
                return Ok(());
            }
            (Ok(_), Ok(_)) => {
                return Err(project_binding_mismatch(
                    transport_label,
                    &requested_alias,
                    project,
                    Some("requested alias resolves to a different canonical database"),
                ));
            }
            (Err(err), _) => {
                return Err(project_binding_mismatch(
                    transport_label,
                    &requested_alias,
                    project,
                    Some(&format!("requested alias resolution failed closed: {err}")),
                ));
            }
            (_, Err(err)) => {
                return Err(project_binding_mismatch(
                    transport_label,
                    &requested_alias,
                    project,
                    Some(&format!("bound identity resolution failed closed: {err}")),
                ));
            }
        }
    }
    if !project_defaults_to_bound_project(tool_name, args) {
        return Ok(());
    }
    if tool_name == "tachi_memory"
        && args
            .get("action")
            .and_then(|value| value.as_str())
            .map(|action| !tachi_memory_action_defaults_to_project(action))
            .unwrap_or(false)
    {
        return Ok(());
    }
    if args
        .get("scope")
        .and_then(|value| value.as_str())
        .is_some_and(|scope| scope.eq_ignore_ascii_case("global"))
    {
        return Ok(());
    }
    args.insert("project".to_string(), serde_json::json!(project));
    Ok(())
}

fn project_binding_mismatch(
    transport_label: &str,
    requested_alias: &str,
    effective_bound_identity: &str,
    reason: Option<&str>,
) -> rmcp::ErrorData {
    let reason = reason
        .map(|reason| format!(" Reason: {reason}."))
        .unwrap_or_default();
    rmcp::ErrorData::invalid_params(
        format!(
            "{transport_label} project binding mismatch: requested alias '{requested_alias}' is not the effective bound identity '{effective_bound_identity}'.{reason} \
repair: omit the project parameter (session binding wins). Same-DB aliases are accepted and normalized to '{effective_bound_identity}'; other-project writes and destructive actions remain forbidden"
        ),
        None,
    )
}

/// Returns true when an explicit `project` param may differ from the session
/// binding. Protects write isolation only: daemon-side reads use read-only opens
/// and do not threaten single-writer discipline.
pub(crate) fn explicit_project_can_cross_binding(tool_name: &str, args: &JsonObject) -> bool {
    match tool_name {
        "search_memory"
        | "find_similar_memory"
        | "get_memory"
        | "list_memories"
        | "tachi_search" => true,
        "tachi_memory" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_memory_action_allows_cross_project_read),
        "tachi_wiki" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_wiki_action_allows_cross_project_read),
        "tachi_event" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_event_action_allows_cross_project_read),
        _ => false,
    }
}

pub(crate) fn project_defaults_to_bound_project(tool_name: &str, args: &JsonObject) -> bool {
    if tool_name == "tachi_memory" {
        return args
            .get("action")
            .and_then(|value| value.as_str())
            .is_none_or(tachi_memory_action_defaults_to_project);
    }
    matches!(
        tool_name,
        "search_memory"
            | "find_similar_memory"
            | "get_memory"
            | "list_memories"
            | "archive_memory"
            | "save_memory"
            | "remember"
            | "ingest_event"
            | "extract_facts"
            | "tachi_search"
            | "tachi_save"
            | "tachi_event"
            | "tachi_domain_adapter"
            | "tachi_task"
            | "tachi_verify"
            | "tachi_gh"
            | "tachi_wiki"
            | "wiki_write"
            | "tachi_wiki_write"
    ) || tool_name == "tachi_memory"
}

fn tachi_memory_action_defaults_to_project(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "alerts"
            | "ask"
            | "briefing"
            | "checkpoint"
            | "consolidate"
            | "delete"
            | "extract_facts"
            | "get"
            | "ingest"
            | "ingest_source"
            | "apply_recall_proposals"
            | "pattern_feedback"
            | "progress"
            | "readiness"
            | "recall_proposals"
            | "recall_simulate"
            | "review_recall_proposal"
            | "save"
            | "search"
    )
}

fn tachi_memory_action_allows_cross_project_read(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "alerts"
            | "ask"
            | "briefing"
            | "consolidate"
            | "get"
            | "readiness"
            | "recall_simulate"
            | "search"
    )
}

fn tachi_event_action_allows_cross_project_read(action: &str) -> bool {
    matches!(action.to_ascii_lowercase().as_str(), "metrics" | "query")
}

fn tachi_wiki_action_allows_cross_project_read(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "browse" | "read" | "search"
    )
}

/// Reject explicit cross-project targeting from an UNBOUND session when the
/// tool/action is not a read-only cross-project case. Bound sessions are
/// handled by `enforce_session_project`. An unbound HTTP direct-connect session
/// has no declared tenant, so an explicit `project=` on a mutating tool is a
/// potential cross-tenant write and must be rejected. (C1 fix.)
///
/// Invariant protected: single-writer project isolation — an unbound session
/// must not be able to route a write into an arbitrary project's DB. Read-only
/// cross-project cases (handled by `explicit_project_can_cross_binding`) do not
/// threaten single-writer discipline and remain allowed; this guard checks both
/// sides so it does not over-reach into legitimate reads.
pub(crate) fn reject_unbound_cross_project_write(
    tool_name: &str,
    arguments: &Option<JsonObject>,
    bound_project: Option<&str>,
    transport_label: &str,
) -> Result<(), rmcp::ErrorData> {
    if bound_project.is_some() {
        return Ok(());
    }
    let Some(args) = arguments.as_ref() else {
        return Ok(());
    };
    let Some(explicit) = args.get("project").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    if explicit_project_can_cross_binding(tool_name, args) {
        return Ok(());
    }
    Err(rmcp::ErrorData::invalid_params(
        format!(
            "{transport_label} session is not bound to a project; refusing explicit project='{explicit}' on tool '{tool_name}' (cross-project writes require a bound session — send X-Tachi-Project at initialize)"
        ),
        None,
    ))
}

pub(crate) fn normalize_identity_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn args(map: serde_json::Map<String, serde_json::Value>) -> Option<JsonObject> {
        Some(map)
    }

    fn map_from(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }

    fn with_test_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        std::fs::create_dir_all(&tachi_home).expect("tachi home");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");

        let result = f(&tachi_home);

        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
        result
    }

    fn repo_db(root: &Path) -> PathBuf {
        let db = root.join(".tachi/memory.db");
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("create db parent");
        std::fs::write(&db, b"identity-only fixture").expect("write db fixture");
        db
    }

    fn write_manifest(tachi_home: &Path, db_paths: &[&Path]) -> Vec<u8> {
        let entries = db_paths
            .iter()
            .map(|db_path| {
                json!({
                    "path": db_path.to_string_lossy(),
                    "role": "project",
                    "owner": "project:test",
                    "schema_kind": "tachi",
                    "vec_enabled": false,
                    "allow_write": true,
                    "last_doctor_at": "1970-01-01T00:00:00Z",
                    "last_classification": "healthy",
                    "scope_hint": "test"
                })
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "generated_at": "1970-01-01T00:00:00Z",
            "_comment": "identity test",
            "dbs": entries
        }))
        .expect("serialize manifest");
        std::fs::write(tachi_home.join("manifest.json"), &bytes).expect("write manifest");
        bytes
    }

    #[test]
    fn bound_session_rejects_cross_project_write() {
        let mut arguments = args(map_from(&[
            ("action", json!("save")),
            ("project", json!("other")),
            ("text", json!("nope")),
        ]));
        let err = enforce_session_project(
            "tachi_memory",
            &mut arguments,
            "sigil",
            "HTTP direct-connect",
        )
        .expect_err("cross-project write must fail");
        assert!(
            err.message.contains("project binding mismatch"),
            "got: {}",
            err.message
        );
        // #925: bare mismatch is agent-hostile — must name the repair.
        assert!(
            err.message.contains("repair:")
                && err.message.contains("omit the project parameter")
                && err.message.contains("effective bound identity 'sigil'"),
            "mismatch must include repair hint, got: {}",
            err.message
        );
    }

    #[test]
    fn bound_session_allows_cross_project_read_search() {
        with_test_home(|tachi_home| {
            let sigil = tachi_home.join("projects/sigil/memory.db");
            let other = tachi_home.join("projects/other/memory.db");
            std::fs::create_dir_all(sigil.parent().expect("sigil parent")).expect("sigil parent");
            std::fs::create_dir_all(other.parent().expect("other parent")).expect("other parent");
            std::fs::write(&sigil, b"sigil").expect("sigil db");
            std::fs::write(&other, b"other").expect("other db");

            let mut arguments = args(map_from(&[
                ("action", json!("search")),
                ("project", json!("other")),
                ("query", json!("hello")),
            ]));
            enforce_session_project(
                "tachi_memory",
                &mut arguments,
                "sigil",
                "HTTP direct-connect",
            )
            .expect("cross-project read must pass");
            assert_eq!(
                arguments
                    .as_ref()
                    .and_then(|a| a.get("project"))
                    .and_then(|v| v.as_str()),
                Some("other")
            );
        });
    }

    #[test]
    fn bound_session_normalizes_hashed_and_legacy_aliases_in_either_direction() {
        with_test_home(|tachi_home| {
            let repo = tachi_home.join("repos/Sigil");
            let db = repo_db(&repo);
            write_manifest(tachi_home, &[&db]);
            let hashed = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("hashed alias");
            let legacy =
                crate::path_utils::plan_c_legacy_dir_name_from_root(&repo).expect("legacy alias");

            for (bound, requested) in [(&hashed, &legacy), (&legacy, &hashed)] {
                let mut arguments = args(map_from(&[
                    ("action", json!("save")),
                    ("project", json!(requested)),
                    ("text", json!("same physical DB")),
                ]));
                enforce_session_project(
                    "tachi_memory",
                    &mut arguments,
                    bound,
                    "HTTP direct-connect",
                )
                .unwrap_or_else(|err| {
                    panic!("{requested} should normalize to bound {bound}: {err}")
                });
                assert_eq!(
                    arguments
                        .as_ref()
                        .and_then(|a| a.get("project"))
                        .and_then(|v| v.as_str()),
                    Some(bound.as_str()),
                    "effective identity must remain the immutable binding"
                );
            }
        });
    }

    #[test]
    fn bound_session_rejects_ambiguous_legacy_alias_for_equal_repo_basenames() {
        with_test_home(|tachi_home| {
            let repo_a = tachi_home.join("workspace-a/Sigil");
            let repo_b = tachi_home.join("workspace-b/Sigil");
            let db_a = repo_db(&repo_a);
            let db_b = repo_db(&repo_b);
            write_manifest(tachi_home, &[&db_a, &db_b]);
            let bound = crate::path_utils::plan_c_dir_name_from_root(&repo_a).expect("bound alias");
            let mut arguments = args(map_from(&[
                ("action", json!("save")),
                ("project", json!("Sigil")),
                ("text", json!("must not guess")),
            ]));

            let err =
                enforce_session_project("tachi_memory", &mut arguments, &bound, "stdio proxy")
                    .expect_err("duplicate legacy basename must be ambiguous");
            assert!(err.message.contains("requested alias 'Sigil'"));
            assert!(err
                .message
                .contains(&format!("effective bound identity '{bound}'")));
            assert!(err.message.contains("ambiguous"), "got: {}", err.message);

            let other_hashed =
                crate::path_utils::plan_c_dir_name_from_root(&repo_b).expect("other hashed alias");
            let mut arguments = args(map_from(&[
                ("action", json!("save")),
                ("project", json!(&other_hashed)),
                ("text", json!("same basename is not same DB")),
            ]));
            let err =
                enforce_session_project("tachi_memory", &mut arguments, &bound, "stdio proxy")
                    .expect_err(
                        "distinct hashed aliases with equal basenames must remain isolated",
                    );
            assert!(
                err.message.contains("different canonical database"),
                "got: {}",
                err.message
            );
        });
    }

    #[test]
    fn bound_session_rejects_genuinely_different_write_and_destructive_aliases() {
        with_test_home(|tachi_home| {
            let sigil_repo = tachi_home.join("repos/Sigil");
            let quant_repo = tachi_home.join("repos/Quant");
            let sigil_db = repo_db(&sigil_repo);
            let quant_db = repo_db(&quant_repo);
            write_manifest(tachi_home, &[&sigil_db, &quant_db]);
            let bound =
                crate::path_utils::plan_c_dir_name_from_root(&sigil_repo).expect("bound alias");
            let other =
                crate::path_utils::plan_c_dir_name_from_root(&quant_repo).expect("other alias");

            for action in ["save", "delete"] {
                let mut arguments = args(map_from(&[
                    ("action", json!(action)),
                    ("project", json!(&other)),
                    ("text", json!("must stay isolated")),
                ]));
                let err = enforce_session_project(
                    "tachi_memory",
                    &mut arguments,
                    &bound,
                    "HTTP direct-connect",
                )
                .expect_err("different-project mutation must fail");
                assert!(
                    err.message.contains("different canonical database"),
                    "got: {}",
                    err.message
                );
            }
        });
    }

    #[test]
    fn unresolvable_alias_rejection_has_zero_filesystem_side_effects() {
        with_test_home(|tachi_home| {
            let repo = tachi_home.join("repos/Sigil");
            let db = repo_db(&repo);
            let manifest_before = write_manifest(tachi_home, &[&db]);
            let bound = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("bound alias");
            let projects = tachi_home.join("projects");
            let broken = projects.join("broken/memory.db");
            std::fs::create_dir_all(broken.parent().expect("broken parent"))
                .expect("broken parent");
            #[cfg(unix)]
            std::os::unix::fs::symlink(tachi_home.join("missing/memory.db"), &broken)
                .expect("broken symlink");

            for alias in ["unknown", "broken"] {
                let mut arguments = args(map_from(&[
                    ("action", json!("save")),
                    ("project", json!(alias)),
                    ("text", json!("must not create")),
                ]));
                let err =
                    enforce_session_project("tachi_memory", &mut arguments, &bound, "stdio proxy")
                        .expect_err("unresolvable alias must fail closed");
                assert!(
                    err.message.contains("resolution failed closed"),
                    "got: {}",
                    err.message
                );
            }

            assert!(
                !projects.join("unknown").exists(),
                "unknown dir was created"
            );
            assert!(
                !tachi_home.join("missing").exists(),
                "broken target was repaired"
            );
            #[cfg(unix)]
            assert!(broken.is_symlink(), "broken alias must remain untouched");
            assert_eq!(
                std::fs::read(tachi_home.join("manifest.json")).expect("read manifest"),
                manifest_before,
                "validation must not alter the manifest"
            );
        });
    }

    #[test]
    fn manifest_resolution_failure_rejects_even_equal_alias_without_mutation() {
        with_test_home(|tachi_home| {
            let bound_db = tachi_home.join("projects/sigil/memory.db");
            std::fs::create_dir_all(bound_db.parent().expect("bound parent"))
                .expect("bound parent");
            std::fs::write(&bound_db, b"bound DB").expect("bound DB");
            let invalid_manifest = b"{ not valid manifest JSON";
            std::fs::write(tachi_home.join("manifest.json"), invalid_manifest)
                .expect("invalid manifest");
            let mut arguments = args(map_from(&[
                ("action", json!("save")),
                ("project", json!("sigil")),
                ("text", json!("must fail closed")),
            ]));

            let err = enforce_session_project(
                "tachi_memory",
                &mut arguments,
                "sigil",
                "HTTP direct-connect",
            )
            .expect_err("manifest parse failure must reject an otherwise equal alias");
            assert!(
                err.message.contains("manifest cannot be read")
                    && err.message.contains("failed closed"),
                "got: {}",
                err.message
            );
            assert_eq!(
                std::fs::read(tachi_home.join("manifest.json")).expect("manifest unchanged"),
                invalid_manifest
            );
            assert_eq!(std::fs::read(&bound_db).expect("DB unchanged"), b"bound DB");
        });
    }

    #[test]
    fn bound_session_injects_project_for_defaulting_write() {
        let mut arguments = args(map_from(&[
            ("action", json!("save")),
            ("text", json!("bound write")),
        ]));
        enforce_session_project("tachi_memory", &mut arguments, "sigil", "stdio proxy")
            .expect("inject project");
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get("project"))
                .and_then(|v| v.as_str()),
            Some("sigil")
        );
    }

    #[test]
    fn bound_session_does_not_force_project_on_global_scope() {
        let mut arguments = args(map_from(&[
            ("action", json!("search")),
            ("scope", json!("global")),
            ("query", json!("x")),
        ]));
        enforce_session_project("tachi_memory", &mut arguments, "sigil", "stdio proxy")
            .expect("global scope search");
        assert!(
            arguments.as_ref().and_then(|a| a.get("project")).is_none(),
            "global scope must not get bound project injected"
        );
    }

    #[test]
    fn unbound_session_rejects_explicit_project_write() {
        let arguments = args(map_from(&[
            ("action", json!("save")),
            ("project", json!("victim")),
            ("text", json!("pwn")),
        ]));
        let err = reject_unbound_cross_project_write(
            "tachi_memory",
            &arguments,
            None,
            "HTTP direct-connect",
        )
        .expect_err("C1 must reject unbound write");
        assert!(
            err.message.contains("not bound to a project")
                && err.message.contains("victim")
                && err.message.contains("X-Tachi-Project"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn unbound_session_allows_explicit_project_read() {
        let arguments = args(map_from(&[
            ("action", json!("search")),
            ("project", json!("wiki")),
            ("query", json!("x")),
        ]));
        reject_unbound_cross_project_write("tachi_memory", &arguments, None, "HTTP direct-connect")
            .expect("unbound cross-project read allowed");
    }

    #[test]
    fn bound_session_skips_c1_unbound_guard() {
        let arguments = args(map_from(&[
            ("action", json!("save")),
            ("project", json!("sigil")),
            ("text", json!("ok")),
        ]));
        // C1 only applies when unbound; bound path uses enforce_session_project.
        reject_unbound_cross_project_write(
            "tachi_memory",
            &arguments,
            Some("sigil"),
            "HTTP direct-connect",
        )
        .expect("bound session not subject to unbound C1");
    }

    #[test]
    fn legacy_search_memory_is_cross_project_readable() {
        let args = map_from(&[("project", json!("other")), ("query", json!("q"))]);
        assert!(explicit_project_can_cross_binding("search_memory", &args));
        assert!(!explicit_project_can_cross_binding(
            "save_memory",
            &map_from(&[("project", json!("other")), ("text", json!("t"))])
        ));
    }

    #[test]
    fn normalize_identity_trims_and_rejects_empty() {
        assert_eq!(
            normalize_identity_value("  sigil  ").as_deref(),
            Some("sigil")
        );
        assert_eq!(normalize_identity_value("   "), None);
        assert_eq!(normalize_identity_value(""), None);
    }
}
