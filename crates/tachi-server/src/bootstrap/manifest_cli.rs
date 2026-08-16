use super::print_pretty_json;
use std::error::Error;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::ManifestAction;

fn manifest_path(app_home: &Path) -> PathBuf {
    app_home.join("manifest.json")
}

fn default_scan_roots(home: &Path, app_home: &Path, git_root: Option<&PathBuf>) -> Vec<PathBuf> {
    let mut roots = crate::doctor::default_scan_roots(home, git_root.map(|p| p.as_path()));
    if app_home.exists() && !roots.iter().any(|root| root == app_home) {
        roots.push(app_home.to_path_buf());
    }
    roots
}

fn model_lane_lines() -> [&'static str; 6] {
    [
        "embedding: voyage-4 (1024d), key=VOYAGE_API_KEY",
        "rerank: rerank-2.5, keys=VOYAGE_RERANK_API_KEY or VOYAGE_API_KEY",
        "extract/summary: OpenAI-compatible API lanes with configured provider fallback",
        "daily distill: OpenAI-compatible API only; FOUNDRY_DISTILL_BACKEND=claude_cli is a legacy selector, not a Claude subprocess",
        "reasoning/chat: Claude CLI first, then configured OpenAI-compatible API fallback",
        "security scan: historical ClaudeCli selector runs two provider-only votes fail-closed; raw_api uses one API vote; disabled skips LLM",
    ]
}

/// Read-only: opens the global (and project, if present) hub stores directly
/// via `MemoryStore::open_read_only` — deliberately NOT a full `MemoryServer`
/// (which spins up LLM clients / pools we don't need for a lint pass). Missing
/// or unopenable DBs are tolerated silently (mirrors `collect_job_histograms`
/// below): a doctor run must not fail just because a project DB doesn't exist
/// yet.
///
/// Deliberately does NOT filter by `cap_type` in the SQL query (`hub_list`'s
/// `type = ?` filter is case-sensitive, memcore/src/db/hub_db.rs:100-103, so a
/// `cap_type="MCP"` row would silently never reach a `Some("mcp")` filter here
/// — #995 finding 2). Collect every capability and let
/// `hub_capability_discovery_status_warnings` apply its own
/// `eq_ignore_ascii_case("mcp")` check, which is already case-insensitive.
fn collect_hub_caps_for_lint(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    immutable: bool,
) -> Vec<memcore::HubCapability> {
    let mut caps = Vec::new();
    for path in std::iter::once(Some(global_db_path)).chain(std::iter::once(project_db_path)) {
        let Some(path) = path else { continue };
        if !path.exists() {
            continue;
        }
        if immutable {
            let uri = crate::doctor::make_immutable_uri(path);
            let Ok(conn) = memcore::db::open_immutable_readonly(&uri) else {
                continue;
            };
            if let Ok(mut found) = memcore::db::hub_list(&conn, None, false) {
                caps.append(&mut found);
            }
        } else {
            let Some(path_str) = path.to_str() else {
                continue;
            };
            let Ok(store) = memcore::MemoryStore::open_read_only(path_str) else {
                continue;
            };
            if let Ok(mut found) = store.hub_list(None, false) {
                caps.append(&mut found);
            }
        }
    }
    caps
}

pub(super) async fn run_doctor_command(
    json_output: bool,
    fix: bool,
    roots_override: Vec<PathBuf>,
    jobs_report: bool,
    probe_keys: bool,
    run_daily: bool,
    home: &Path,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    git_root: Option<&PathBuf>,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<(), Box<dyn Error>> {
    let roots: Vec<PathBuf> = if !roots_override.is_empty() {
        roots_override
    } else {
        default_scan_roots(home, app_home, git_root)
    };
    let quarantine_dir = app_home.join("quarantine");
    let opts = crate::doctor::ScanOptions {
        auto_fix: fix,
        max_depth: 10,
    };
    let strict_read_only = !fix && !run_daily && !probe_keys;
    let mut report = if strict_read_only {
        crate::doctor::scan_strict_read_only(&roots, &quarantine_dir, opts)
    } else {
        crate::doctor::scan(&roots, &quarantine_dir, opts)
    };
    report
        .warnings
        .extend(crate::doctor::project_secret_file_warnings(
            git_root.map(|p| p.as_path()),
        ));
    report
        .warnings
        .extend(crate::doctor::hub_capability_discovery_status_warnings(
            &collect_hub_caps_for_lint(global_db_path, project_db_path, strict_read_only),
        ));
    // #1119: proactively surface any DB already skewed vs this binary's schema
    // version (the incident shape), computed from the just-scanned findings.
    let schema_skew = crate::doctor::schema_version_skew_warnings(&report.findings);
    report.warnings.extend(schema_skew);

    // tachi#1184 item 2: the build-resource patrol (private orphan
    // CARGO_TARGET_DIR-shaped dirs + managed-worktree inspection notes).
    // REPORT-ONLY — see `doctor::build_resources` module docs; never gated on
    // `fix`, since neither half of this patrol deletes or reconciles anything.
    report.warnings.extend(if strict_read_only {
        crate::doctor::scan_orphan_build_resources_strict(
            crate::doctor::DEFAULT_ORPHAN_MAX_AGE_DAYS,
            global_db_path,
        )
    } else {
        crate::doctor::scan_orphan_build_resources(
            crate::doctor::DEFAULT_ORPHAN_MAX_AGE_DAYS,
            global_db_path,
        )
    });
    report
        .warnings
        .extend(crate::doctor::worktree_inspection_report(
            crate::doctor::DEFAULT_WORKTREE_STALE_DAYS,
        ));

    // The default doctor is a read-only report. Keep the populated manifest in
    // memory for report sections, but persist it only when the operator chose
    // one of doctor's explicit mutation intents.
    let manifest_path = manifest_path(app_home);
    let mut m = crate::manifest::Manifest::load_or_empty(&manifest_path);
    m.populate_from_doctor(&report);
    if fix || run_daily {
        if let Err(e) = m.save(&manifest_path) {
            eprintln!(
                "[doctor] warning: failed to update manifest at {}: {e}",
                manifest_path.display()
            );
        }
    }

    // --run-daily: unified remediation verb for stale distill marker + probe cache.
    // Runs the provider probe refresh and the daily distill batch, then writes the
    // marker so the next `tachi status` sees a fresh (or failure-detailed) marker
    // instead of a bare stale warning.
    // Distill batch Err / errors>0 / typed distill-step failure exits non-zero after
    // the terminal receipt is printed (#1505). Persist-gate skips stay exit 0.
    let daily_remediation = if run_daily {
        Some(
            run_daily_pipeline_remediation(
                app_home,
                global_db_path,
                project_db_path,
                schema_migration,
            )
            .await,
        )
    } else {
        None
    };
    let daily_remediation_text = daily_remediation.as_ref().map(|outcome| match outcome {
        Ok(summary) | Err(summary) => summary.clone(),
    });
    let daily_remediation_failed = matches!(daily_remediation, Some(Err(_)));

    // Branch #5: optional foundry job-status histogram per manifest DB.
    let jobs_section = if jobs_report {
        Some(collect_job_histograms(&m, strict_read_only))
    } else {
        None
    };
    let provider_section = collect_provider_key_report(
        &app_home.join("global").join(memcore::MEMORY_DB_FILENAME),
        probe_keys,
        strict_read_only,
    )
    .await;

    if json_output {
        let mut full = serde_json::to_value(&report)?;
        if let Some(jobs) = &jobs_section {
            if let Some(obj) = full.as_object_mut() {
                obj.insert("foundry_jobs".into(), serde_json::to_value(jobs)?);
            }
        }
        if let Some(obj) = full.as_object_mut() {
            obj.insert(
                "provider_keys".into(),
                serde_json::to_value(&provider_section)?,
            );
            obj.insert(
                "models".into(),
                serde_json::to_value(crate::status_ops::status_health::model_lanes_json())?,
            );
            if let Some(remediation) = &daily_remediation_text {
                obj.insert(
                    "daily_remediation".into(),
                    serde_json::Value::String(remediation.clone()),
                );
            }
        }
        print_pretty_json(&full)?;
    } else {
        println!("{}", crate::doctor::render_report(&report));
        println!();
        println!(
            "manifest: {} ({} dbs recorded)",
            manifest_path.display(),
            m.dbs.len()
        );
        if let Some(jobs) = &jobs_section {
            println!("\n=== foundry jobs ===");
            for entry in jobs {
                let h = &entry.histogram;
                println!(
                    "  {} planned={} queued={} running={} completed={} failed={} skipped={} other={} gc_eligible(>=30d)={} total={}",
                    entry.path,
                    h.planned, h.queued, h.running, h.completed, h.failed, h.skipped,
                    h.other.iter().map(|(_, n)| n).sum::<usize>(),
                    h.gc_eligible, h.total
                );
                if !entry.error.is_empty() {
                    println!("    error: {}", entry.error);
                }
            }
        }
        println!("\n=== provider keys ===");
        for key in &provider_section.keys {
            println!(
                "  {} status={} source={} required={} deprecated={}",
                key.name, key.status, key.source, key.required, key.deprecated
            );
        }
        if probe_keys {
            for probe in &provider_section.probes {
                println!(
                    "  probe {}: {}{}",
                    probe.name,
                    probe.status,
                    probe
                        .message
                        .as_ref()
                        .map(|msg| format!(" ({msg})"))
                        .unwrap_or_default()
                );
            }
        } else {
            println!("  live probes skipped (pass --probe-keys to test providers)");
        }
        println!("\n=== model lanes ===");
        for line in model_lane_lines() {
            println!("  {line}");
        }
        if let Some(remediation) = &daily_remediation_text {
            println!("\n=== daily pipeline remediation ===");
            println!("{remediation}");
        }
    }

    if daily_remediation_failed {
        let summary = daily_remediation_text
            .unwrap_or_else(|| "daily pipeline remediation failed".to_string());
        return Err(format!("daily pipeline remediation failed:{summary}").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_regular_files(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(path).expect("read snapshot directory") {
                let entry = entry.expect("read snapshot entry");
                let file_type = entry.file_type().expect("read snapshot file type");
                if file_type.is_dir() {
                    visit(root, &entry.path(), out);
                } else if file_type.is_file() {
                    let relative = entry
                        .path()
                        .strip_prefix(root)
                        .expect("snapshot entry below root")
                        .to_path_buf();
                    out.insert(
                        relative,
                        std::fs::read(entry.path()).expect("read snapshot file"),
                    );
                }
            }
        }

        let mut out = std::collections::BTreeMap::new();
        visit(root, root, &mut out);
        out
    }

    #[test]
    fn doctor_default_preserves_every_database_and_config_byte() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().expect("tmp");
        let home = dir.path().join("home");
        let app_home = dir.path().join("app-home");
        let global_dir = app_home.join("global");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&global_dir).expect("create global dir");
        let global_db = global_dir.join(memcore::MEMORY_DB_FILENAME);
        drop(
            memcore::MemoryStore::open(global_db.to_str().expect("utf8 db path"))
                .expect("seed current database"),
        );
        let config = app_home.join("config.env");
        std::fs::write(&config, b"DOCTOR_READ_ONLY_SENTINEL=1\n").expect("seed config");
        let before = snapshot_regular_files(&app_home);

        tokio::runtime::Runtime::new()
            .expect("tokio runtime")
            .block_on(run_doctor_command(
                false,
                false,
                vec![app_home.clone()],
                false,
                false,
                false,
                &home,
                &app_home,
                &global_db,
                None,
                None,
                &memcore::MigrationAuthority::Deny,
            ))
            .expect("default doctor scan");

        let after = snapshot_regular_files(&app_home);
        for path in before.keys().chain(after.keys()) {
            if before.get(path) != after.get(path) {
                eprintln!(
                    "doctor default changed {} (before={} bytes, after={} bytes)",
                    path.display(),
                    before.get(path).map_or(0, Vec::len),
                    after.get(path).map_or(0, Vec::len)
                );
            }
        }
        assert_eq!(
            after,
            before,
            "default doctor must not create or alter a manifest, DB, WAL/SHM, marker, provider-health cache, or config file"
        );
    }

    #[test]
    fn manifest_path_lives_directly_under_app_home() {
        let app_home = std::path::Path::new("/tmp/tachi-app-home");
        assert_eq!(manifest_path(app_home), app_home.join("manifest.json"));
    }

    #[test]
    fn model_lane_lines_describe_current_routing() {
        assert_eq!(
            model_lane_lines(),
            [
                "embedding: voyage-4 (1024d), key=VOYAGE_API_KEY",
                "rerank: rerank-2.5, keys=VOYAGE_RERANK_API_KEY or VOYAGE_API_KEY",
                "extract/summary: OpenAI-compatible API lanes with configured provider fallback",
                "daily distill: OpenAI-compatible API only; FOUNDRY_DISTILL_BACKEND=claude_cli is a legacy selector, not a Claude subprocess",
                "reasoning/chat: Claude CLI first, then configured OpenAI-compatible API fallback",
                "security scan: historical ClaudeCli selector runs two provider-only votes fail-closed; raw_api uses one API vote; disabled skips LLM",
            ]
        );
    }

    fn test_hub_capability(id: &str, cap_type: &str, definition: &str) -> memcore::HubCapability {
        memcore::HubCapability {
            id: id.to_string(),
            name: id.to_string(),
            cap_type: cap_type.to_string(),
            version: 1,
            description: String::new(),
            definition: definition.to_string(),
            enabled: true,
            review_status: "approved".to_string(),
            health_status: "healthy".to_string(),
            last_error: None,
            last_success_at: None,
            last_failure_at: None,
            fail_streak: 0,
            active_version: None,
            exposure_mode: "direct".to_string(),
            uses: 0,
            successes: 0,
            failures: 0,
            avg_rating: 0.0,
            last_used: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// #995 finding 2 (codex review): `hub_list`'s SQL `type = ?` filter
    /// (memcore/src/db/hub_db.rs:100-103) is case-sensitive. A cap_type of
    /// "MCP" (uppercase) registered in the DB must still reach the lint —
    /// collection must not silently drop it via a case-sensitive
    /// `Some("mcp")` filter.
    #[test]
    fn collect_hub_caps_for_lint_finds_uppercase_mcp_cap_type() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("global.db");
        let store =
            memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store for write");
        let cap = test_hub_capability("mcp:UPPER", "MCP", r#"{"other_field":"value"}"#);
        store.hub_register(&cap).expect("register cap");
        drop(store);

        let caps = collect_hub_caps_for_lint(&db_path, None, false);
        assert!(
            caps.iter().any(|c| c.id == "mcp:UPPER"),
            "collector must return the uppercase cap_type=MCP row so the lint's \
             case-insensitive check can see it: {caps:?}"
        );

        let warnings = crate::doctor::hub_capability_discovery_status_warnings(&caps);
        assert!(
            warnings.iter().any(|w| w.path == "mcp:UPPER"),
            "lint must fire on the enabled+approved+healthy uppercase MCP cap missing \
             discovery_status: {warnings:?}"
        );
    }

    #[test]
    fn default_scan_roots_include_custom_app_home_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let app_home = temp.path().join("custom-tachi");
        std::fs::create_dir_all(&app_home).expect("create custom app home");

        let roots = default_scan_roots(&home, &app_home, None);
        assert!(
            roots.iter().any(|root| root == &app_home),
            "custom app home should be scanned: {roots:?}"
        );

        let default_app_home = app_home.join(".tachi");
        std::fs::create_dir_all(&default_app_home).expect("create default app home");
        let roots = default_scan_roots(&app_home, &default_app_home, None);
        assert_eq!(
            roots
                .iter()
                .filter(|root| *root == &default_app_home)
                .count(),
            1,
            "app home should not be duplicated: {roots:?}"
        );
    }

    // --- #1181 checkpoint 7 (codex review, 2026-07-17, MERGE-BLOCKING) +
    // leader adjudication "THREAD IT": `doctor --run-daily`'s distill step
    // opened its DBs via the fail-closed `MemoryServer::new` regardless of
    // the CLI's resolved `--allow-schema-migration` authority, silently
    // refusing to migrate a stamped-older DB even when the operator
    // explicitly authorized it. This mirrors the
    // `backfill_*_requires_flag_to_migrate_stamped_older_db_in_process`
    // pattern (backfill.rs) at the `run_daily_pipeline_remediation` entry
    // point that `run_doctor_command` calls under `--run-daily`.

    fn seed_and_stamp_older_schema_version(db_path: &std::path::Path) {
        memcore::MemoryStore::open(db_path.to_str().expect("utf8 db path"))
            .expect("seed current-schema db");
        let conn = rusqlite::Connection::open(db_path).expect("reopen to roll back stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp older schema version");
    }

    fn read_user_version(db_path: &std::path::Path) -> u32 {
        let conn = rusqlite::Connection::open(db_path).expect("open for version read");
        memcore::db::migrations::read_schema_version(&conn).expect("read schema version")
    }

    // Plain `#[test]` + `block_on` (not `#[tokio::test]`), matching the
    // `global_test_lock` convention used everywhere else in this crate: the
    // guard protects process-wide DB-path state against a parallel test
    // racing the same schema-migration setup, so it must stay held for the
    // entire two-call sequence including both internal awaits -- `block_on`
    // runs that async body to completion synchronously on this thread, so
    // there is no `.await` expression in scope for clippy's
    // `await_holding_lock` lint to flag, while the guard's actual coverage
    // is unchanged.
    #[test]
    fn doctor_run_daily_requires_flag_to_migrate_stamped_older_global_db() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().expect("tmp");
        let app_home = dir.path().join("app-home");
        std::fs::create_dir_all(&app_home).expect("create app home");
        let global_db = dir.path().join("global.db");
        let project_db = dir.path().join("project.db");
        seed_and_stamp_older_schema_version(&global_db);

        // Isolate named-project discovery so the allow path cannot scan the
        // host's real ~/.tachi projects and absorb unrelated migration denials
        // into report.errors (#1505 exit wiring surfaces those as Err).
        let prev_tachi_home = std::env::var_os("TACHI_HOME");
        std::env::set_var("TACHI_HOME", &app_home);
        struct RestoreTachiHome(Option<std::ffi::OsString>);
        impl Drop for RestoreTachiHome {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("TACHI_HOME", value),
                    None => std::env::remove_var("TACHI_HOME"),
                }
            }
        }
        let _restore_tachi_home = RestoreTachiHome(prev_tachi_home);

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

        let deny_summary = rt
            .block_on(run_daily_pipeline_remediation(
                &app_home,
                &global_db,
                Some(project_db.as_path()),
                &memcore::MigrationAuthority::Deny,
            ))
            .expect_err("schema-deny distill step must fail closed");
        assert!(
            deny_summary.contains("refusing to migrate db schema"),
            "doctor --run-daily without --allow-schema-migration must surface the \
             typed refusal, not silently skip the distill step: {deny_summary}"
        );
        assert_eq!(
            read_user_version(&global_db),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "deny must not mutate the old schema stamp"
        );

        let allow_summary = rt
            .block_on(run_daily_pipeline_remediation(
                &app_home,
                &global_db,
                Some(project_db.as_path()),
                &memcore::MigrationAuthority::Allow {
                    approved_by: "test:1181-doctor-run-daily".to_string(),
                },
            ))
            .unwrap_or_else(|summary| summary);
        assert!(
            !allow_summary.contains("refusing to migrate db schema"),
            "doctor --run-daily WITH --allow-schema-migration must not refuse the \
             authorized migration: {allow_summary}"
        );
        assert_eq!(
            read_user_version(&global_db),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
            "allow must re-stamp the global DB at the current schema version"
        );
    }

    #[test]
    fn doctor_run_daily_blocks_distill_owner_after_missing_timeout_or_failure() {
        for status in [None, Some("timeout"), Some("failed")] {
            let refresh = doctor_probe_refresh_fixture(status, Ok(()));
            if let Some(status) = status {
                assert!(provider_persistence_receipt(&refresh)
                    .is_some_and(|receipt| receipt.status == status));
            } else {
                assert!(provider_persistence_receipt(&refresh).is_none());
            }
            assert!(
                !provider_persistence_allows_distill(&refresh),
                "a {status:?} persistence phase must block the distill DB owner"
            );
        }
    }

    #[test]
    fn doctor_run_daily_preserves_timeout_when_probe_cache_write_fails() {
        let refresh = doctor_probe_refresh_fixture(
            Some("timeout"),
            Err("controlled probe-cache write failure".to_string()),
        );

        let summary = provider_probe_refresh_summary(&refresh);
        assert!(summary.contains("probe cache write failed: controlled probe-cache write failure"));
        assert!(summary.contains("provider_health_persist status=timeout"));
        assert!(summary.contains("cause=provider_health_persist_join_timeout"));
        assert!(
            !provider_persistence_allows_distill(&refresh),
            "cache-write failure must not discard the timeout receipt or permit distill"
        );
    }

    #[test]
    fn doctor_run_daily_fails_closed_when_client_construction_left_no_persistence_receipt() {
        let refresh = doctor_probe_refresh_fixture(None, Ok(()));
        let summary = provider_persistence_distill_skip_summary(&refresh);

        assert!(!provider_persistence_allows_distill(&refresh));
        assert!(summary.contains("status=missing"));
        assert!(summary.contains("cause=provider_health_persist_receipt_missing"));
    }

    #[test]
    fn doctor_run_daily_fails_closed_when_cache_write_and_persistence_receipt_are_both_missing() {
        let refresh = doctor_probe_refresh_fixture(
            None,
            Err("controlled probe-cache write failure".to_string()),
        );
        let summary = provider_probe_refresh_summary(&refresh);
        let distill_skip = provider_persistence_distill_skip_summary(&refresh);

        assert!(!provider_persistence_allows_distill(&refresh));
        assert!(summary.contains("probe cache write failed: controlled probe-cache write failure"));
        assert!(distill_skip.contains("status=missing"));
        assert!(distill_skip.contains("cause=provider_health_persist_receipt_missing"));
    }

    fn doctor_probe_refresh_fixture(
        persistence_status: Option<&str>,
        cache_write: Result<(), String>,
    ) -> crate::status_ops::status_health::DoctorProbeCacheRefresh {
        let probes = persistence_status
            .map(|persistence_status| {
                vec![crate::status_ops::status_health::ProviderProbeResult {
                    name: crate::status_ops::status_health::PROVIDER_HEALTH_PERSIST_PHASE
                        .to_string(),
                    status: persistence_status.to_string(),
                    message: Some(format!(
                        "phase=provider_health_persist cause=provider_health_persist_join_{persistence_status}"
                    )),
                }]
            })
            .unwrap_or_else(|| {
                vec![crate::status_ops::status_health::ProviderProbeResult {
                    name: "llm_client".to_string(),
                    status: "failed".to_string(),
                    message: Some("controlled LLM client construction failure".to_string()),
                }]
            });
        let report = crate::status_ops::status_health::ProviderProbeReport {
            probes: probes.clone(),
            rotation_groups: Vec::new(),
        };
        let cache_write =
            cache_write.map(|()| crate::status_ops::status_health::ProviderProbeCache {
                last_probe_at: chrono::Utc::now().to_rfc3339(),
                ttl_seconds: 60,
                probes,
                rotation_groups: Vec::new(),
            });
        crate::status_ops::status_health::DoctorProbeCacheRefresh {
            report,
            cache_write,
        }
    }

    /// #1605 part B. Live measurement: `tachi doctor --json --run-daily` on the
    /// owner machine exhausted every distill tier with "Missing API key" ~70ms
    /// after process start — zero network attempts — while the Vault was
    /// unlocked and held `SILICONFLOW_API_KEY`/`ZAI_API_KEY` verbatim.
    ///
    /// Cause: the run-daily distill step built its own server with a bare
    /// `MemoryServer::new_with_migration_authority`. That constructor's
    /// `LlmClient` loads key *health* from the DB and nothing else, so
    /// `provider_state.secrets` is empty; only
    /// `provider_config::bootstrap_provider_runtime` materializes Vault
    /// secrets into it. `select_secret` then found no pool entry and no env
    /// value (Vault-stored keys are deliberately absent from env), so
    /// `provider_secret_unavailable_error` reported `configured == 0` —
    /// "Missing API key" — which reads as "no key configured" for a machine
    /// whose Vault holds 34 of them.
    ///
    /// Deadlock shape, pinned by the first half of this test: the only route
    /// from the Vault into the pool is materialization, and nothing on the
    /// lane-call path performs it, so the process can never recover on its own.
    ///
    /// macOS-only: the Vault→pool route for a freshly built server goes through
    /// Keychain auto-unlock, which is macOS-only and here driven by the
    /// deterministic `TACHI_TEST_KEYCHAIN_PASSWORD` override (same pattern as
    /// `vault_ops::tests::bootstrap_auto_unlock_owns_one_provider_refresh`).
    #[cfg(target_os = "macos")]
    #[test]
    fn doctor_run_daily_distill_server_materializes_vault_provider_keys() {
        use crate::test_support::EnvRestore;

        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _allow_auto_unlock = EnvRestore::set("TACHI_TEST_ALLOW_KEYCHAIN_AUTO_UNLOCK", "1");
        let _keychain_password = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", "r1605-password");
        // The env fallback inside `select_secret` must not stand in for the
        // Vault route this test is about: a stray exported provider key on the
        // host would otherwise satisfy the chain without any materialization.
        let _env_guards: Vec<EnvRestore> = [
            "DISTILL_API_KEY",
            "DEEPSEEK_API_KEY",
            "REASONING_API_KEY",
            "ZAI_API_KEY",
            "BIGMODEL_API_KEY",
            "EXTRACT_API_KEY",
            "SILICONFLOW_API_KEY",
            "DISTILL_FALLBACK_API_KEY",
        ]
        .into_iter()
        .map(EnvRestore::remove)
        .collect();

        let dir = tempfile::tempdir().expect("tmp");
        let global_db = dir.path().join("global.db");

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let seed = crate::MemoryServer::new(global_db.clone(), None).expect("seed server");
            crate::vault_ops::handle_vault_init(
                &seed,
                crate::vault_ops::VaultInitParams {
                    password: "r1605-password".to_string(),
                },
            )
            .await
            .expect("vault init");
            crate::vault_ops::handle_vault_set(
                &seed,
                crate::vault_ops::VaultSetParams {
                    name: "SILICONFLOW_API_KEY".to_string(),
                    value: "r1605-distill-secret".to_string(),
                    agent_id: None,
                    secret_type: "api_key".to_string(),
                    description: "#1605 run-daily distill fixture".to_string(),
                    allowed_agents: None,
                    enable_rotation: false,
                    rotation_strategy: None,
                },
            )
            .await
            .expect("vault set");
            crate::vault_ops::handle_vault_lock(&seed)
                .await
                .expect("vault lock");
        });

        // Pre-fix construction: the exact call the run-daily step used to make.
        let bare = crate::MemoryServer::new_with_migration_authority(
            global_db.clone(),
            None,
            memcore::MigrationAuthority::Deny,
        )
        .expect("bare server");
        let chain = bare.llm.distill_api_key_envs_for_tests();
        assert!(
            chain.contains(&"SILICONFLOW_API_KEY"),
            "test precondition: the distill lane chain consults SILICONFLOW_API_KEY, got {chain:?}"
        );
        assert_eq!(
            bare.llm.provider_secret_count(),
            0,
            "a server built without bootstrap_provider_runtime has an empty provider pool"
        );
        assert!(
            bare.llm.provider_secret_for_tests(&chain).is_none(),
            "deadlock shape: the Vault holds the key, nothing on the lane path materializes it, \
             so the lane resolves nothing and reports 'Missing API key'"
        );
        drop(bare);

        // The seam the run-daily distill step builds through now.
        let server = crate::cli_client::build_in_process_server_with_migration_authority(
            &global_db,
            None,
            memcore::MigrationAuthority::Deny,
        )
        .expect("in-process server");
        assert_eq!(
            server
                .llm
                .provider_secret_for_tests(&server.llm.distill_api_key_envs_for_tests())
                .as_deref(),
            Some("r1605-distill-secret"),
            "the distill lane's own key chain must resolve the pooled Vault secret"
        );
    }

    #[test]
    fn distill_failure_marker_mirrors_absorbed_report_counters() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker_path = temp.path().join(".last_distill_run");
        write_distill_failure_marker(&marker_path, "api boom; other", 1, 2, 3, 4);
        let marker: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker_path).expect("marker written"))
                .expect("marker JSON");
        assert_eq!(marker["error"], "api boom; other");
        assert_eq!(marker["groups_distilled"], 1);
        assert_eq!(marker["groups_skipped"], 2);
        assert_eq!(marker["fallback_used"], 3);
        assert_eq!(marker["errors"], 4);
        assert!(
            marker.get("ts").and_then(|v| v.as_str()).is_some(),
            "failure marker must carry ts: {marker}"
        );
    }

    #[test]
    fn distill_failure_marker_zeros_when_hard_err_has_no_report() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker_path = temp.path().join("nested").join(".last_distill_run");
        write_distill_failure_marker(&marker_path, "batch exploded", 0, 0, 0, 0);
        let marker: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker_path).expect("marker written"))
                .expect("marker JSON");
        assert_eq!(marker["error"], "batch exploded");
        assert_eq!(marker["groups_distilled"], 0);
        assert_eq!(marker["groups_skipped"], 0);
        assert_eq!(marker["fallback_used"], 0);
        assert_eq!(marker["errors"], 0);
    }
}

#[derive(Debug, serde::Serialize)]
struct ProviderKeyReport {
    keys: Vec<ProviderKeyStatus>,
    probes: Vec<crate::status_ops::status_health::ProviderProbeResult>,
}

#[derive(Debug, serde::Serialize)]
struct ProviderKeyStatus {
    name: String,
    label: String,
    required: bool,
    deprecated: bool,
    canonical_name: String,
    alias_names: Vec<String>,
    cleanup_hint: Option<String>,
    status: String,
    source: String,
}

impl From<crate::status_ops::ApiKeyStatus> for ProviderKeyStatus {
    fn from(status: crate::status_ops::ApiKeyStatus) -> Self {
        Self {
            name: status.name,
            label: status.label,
            required: status.required,
            deprecated: status.deprecated,
            canonical_name: status.canonical_name,
            alias_names: status.alias_names,
            cleanup_hint: status.cleanup_hint,
            status: status.status,
            source: status.source,
        }
    }
}

/// Runs the daily pipeline remediation when `--run-daily` is passed:
/// 1. Refreshes the provider key probe cache.
/// 2. Runs the daily distill batch and writes the marker.
///
/// Returns `Ok(summary)` when distill completed cleanly (or was skipped by the
/// provider-persist gate). Returns `Err(summary)` when the distill step itself
/// failed — batch `Err`, absorbed `report.errors > 0`, or server-init failure
/// after persist authorized the distill owner — so the doctor CLI can exit
/// non-zero while still emitting the typed cause in the terminal receipt (#1505).
async fn run_daily_pipeline_remediation(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<String, String> {
    // Step 1: refresh provider probe cache.
    let probe_refresh = crate::status_ops::status_health::refresh_doctor_probe_cache(
        app_home,
        global_db_path,
        schema_migration,
    )
    .await;
    let probe_summary = provider_probe_refresh_summary(&probe_refresh);

    // Step 2: run the distill batch. Only an explicit successful receipt may
    // authorize the next write-capable owner. Missing, timeout, and failure all
    // fail closed; cache-write failure cannot erase the in-memory typed receipt
    // (#1505). Persist-gate skips remain Ok (doctor exit 0); distill-step
    // failures return Err so the packaged CLI exits non-zero.
    let distill_outcome = if !provider_persistence_allows_distill(&probe_refresh) {
        Ok(provider_persistence_distill_skip_summary(&probe_refresh))
    } else {
        let marker_path = app_home.join("foundry-runs").join(".last_distill_run");
        // Codex review (2026-07-17, checkpoint 7, MERGE-BLOCKING) + leader
        // adjudication: `doctor --allow-schema-migration --fix --run-daily`
        // must thread the resolved authority into this in-process open,
        // same as every other CLI in-process DB open (#1181's frozen
        // contract) — `MemoryServer::new` hardcodes Deny and would
        // silently refuse the distill step on a stamped-older DB even
        // when the operator explicitly authorized migration.
        //
        // #1605: built through the CLI in-process seam instead of
        // `MemoryServer::new_with_migration_authority` directly. That
        // constructor's `LlmClient` starts with an EMPTY provider-secret
        // pool — construction loads key *health* from the DB and nothing
        // else (`provider_health/config.rs`'s
        // `initial_key_health_from_db`); only
        // `provider_config::bootstrap_provider_runtime` (Keychain
        // auto-unlock + materialization) fills `provider_state.secrets`.
        // Without it every lane call died as "Missing API key" in ~70ms
        // with zero network attempts while the Vault held the keys, because
        // `select_secret` reads that empty map and the env fallback finds
        // nothing (vault-stored keys are deliberately not in env). Every
        // other in-process server — daemon serve and the CLI builder —
        // already goes through this seam.
        //
        // The old `match project_db_path { Some(_) => .., None => skip }`
        // gate is gone for the same reason the scheduler gate widened:
        // `run_daily_batch_distill` distills every manifest-attached
        // named-project DB and treats the bound project as one extra
        // target, so a global-only invocation — the owner daemon's own
        // shape — still has real work. The skip made the documented
        // manual remediation a no-op on exactly the host that needed it.
        match crate::cli_client::build_in_process_server_with_migration_authority(
            &global_db_path.to_path_buf(),
            project_db_path.map(|p| p.to_path_buf()).as_ref(),
            schema_migration.clone(),
        ) {
            Ok(server) => {
                // Names the credential state the batch actually ran with,
                // so a "Missing API key" run is self-diagnosing in the
                // doctor report instead of needing a code-level trace.
                let provider_keys = server.llm.provider_secret_count();
                match crate::foundry_runtime_ops::run_daily_batch_distill(&server).await {
                    Ok(report) if report.errors.is_empty() => {
                        // Write success marker (same shape as the scheduler).
                        if let Some(parent) = marker_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let marker_body = serde_json::json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "groups_distilled": report.groups_distilled,
                            "groups_skipped": report.groups_skipped,
                            "fallback_used": report.fallback_used,
                            "errors": 0,
                        })
                        .to_string();
                        let _ = std::fs::write(&marker_path, marker_body);
                        Ok(format!(
                            "distill: dispatched={} distilled={} skipped={} fallback={} errors=0 provider_keys={provider_keys}",
                            report.batches_dispatched,
                            report.groups_distilled,
                            report.groups_skipped,
                            report.fallback_used,
                        ))
                    }
                    Ok(report) => {
                        // Absorbed per-group/API errors are still a distill
                        // failure for the packaged doctor receipt (#1505): do
                        // not leave a clean success marker. Marker counters
                        // must mirror the terminal summary (same report).
                        let cause = report.errors.join("; ");
                        write_distill_failure_marker(
                            &marker_path,
                            &cause,
                            report.groups_distilled,
                            report.groups_skipped,
                            report.fallback_used,
                            report.errors.len(),
                        );
                        Err(format!(
                            "distill failed: dispatched={} distilled={} skipped={} fallback={} errors={} cause={cause} provider_keys={provider_keys}",
                            report.batches_dispatched,
                            report.groups_distilled,
                            report.groups_skipped,
                            report.fallback_used,
                            report.errors.len(),
                        ))
                    }
                    Err(e) => {
                        // Hard Err with no DistillBatchReport: zeros mean
                        // nothing ran far enough to produce batch counters.
                        write_distill_failure_marker(&marker_path, &e, 0, 0, 0, 0);
                        Err(format!(
                            "distill batch failed: {e} provider_keys={provider_keys}"
                        ))
                    }
                }
            }
            Err(e) => Err(format!("distill skipped (server init failed): {e}")),
        }
    };

    match distill_outcome {
        Ok(distill_summary) => Ok(format!("  {probe_summary}\n  {distill_summary}")),
        Err(distill_summary) => Err(format!("  {probe_summary}\n  {distill_summary}")),
    }
}

/// Write a failure-shaped `.last_distill_run` marker.
///
/// When a `DistillBatchReport` exists (absorbed per-group errors), pass that
/// report's counters so the durable marker cannot contradict the terminal
/// summary. Hard `Err(e)` with no report uses zeros — nothing ran far enough
/// to produce batch counters. `errors` must be `report.errors.len()` when a
/// report exists; the `"error"` cause string is always retained.
fn write_distill_failure_marker(
    marker_path: &Path,
    error: &str,
    groups_distilled: usize,
    groups_skipped: usize,
    fallback_used: usize,
    errors: usize,
) {
    if let Some(parent) = marker_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let marker_body = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "error": error,
        "groups_distilled": groups_distilled,
        "groups_skipped": groups_skipped,
        "fallback_used": fallback_used,
        "errors": errors,
    })
    .to_string();
    let _ = std::fs::write(marker_path, marker_body);
}

fn provider_persistence_receipt(
    refresh: &crate::status_ops::status_health::DoctorProbeCacheRefresh,
) -> Option<&crate::status_ops::status_health::ProviderProbeResult> {
    refresh
        .report
        .probes
        .iter()
        .find(|probe| probe.name == crate::status_ops::status_health::PROVIDER_HEALTH_PERSIST_PHASE)
}

fn provider_persistence_allows_distill(
    refresh: &crate::status_ops::status_health::DoctorProbeCacheRefresh,
) -> bool {
    provider_persistence_receipt(refresh).is_some_and(|receipt| receipt.status.as_str() == "ok")
}

fn provider_persistence_distill_skip_summary(
    refresh: &crate::status_ops::status_health::DoctorProbeCacheRefresh,
) -> String {
    match provider_persistence_receipt(refresh) {
        Some(receipt) => format!(
            "distill skipped (provider_health_persist status={} {}; second writer forbidden)",
            receipt.status,
            receipt.message.as_deref().unwrap_or("cause=unknown")
        ),
        None => "distill skipped (provider_health_persist status=missing cause=provider_health_persist_receipt_missing; second writer forbidden)".to_string(),
    }
}

fn provider_probe_refresh_summary(
    refresh: &crate::status_ops::status_health::DoctorProbeCacheRefresh,
) -> String {
    let failed = refresh
        .report
        .probes
        .iter()
        .filter(|probe| probe.status != "ok")
        .count();
    let mut summary = match &refresh.cache_write {
        Ok(_) => format!(
            "probe cache refreshed ({} probes, {failed} failed)",
            refresh.report.probes.len()
        ),
        Err(error) => format!(
            "probe cache write failed: {error} ({} probes, {failed} failed)",
            refresh.report.probes.len()
        ),
    };
    if let Some(receipt) = provider_persistence_receipt(refresh) {
        summary.push_str(&format!(
            "; {} status={} {}",
            receipt.name,
            receipt.status,
            receipt.message.as_deref().unwrap_or("cause=unknown")
        ));
    }
    summary
}

async fn collect_provider_key_report(
    global_db_path: &Path,
    probe_keys: bool,
    strict_read_only: bool,
) -> ProviderKeyReport {
    let (keys, probes) = crate::status_ops::status_health::collect_doctor_provider_key_report(
        global_db_path,
        probe_keys,
        strict_read_only,
    )
    .await;
    ProviderKeyReport {
        keys: keys.into_iter().map(ProviderKeyStatus::from).collect(),
        probes,
    }
}

#[derive(Debug, serde::Serialize)]
struct DbJobReport {
    path: String,
    histogram: memcore::JobStatusHistogram,
    error: String,
}

fn collect_job_histograms(
    manifest: &crate::manifest::Manifest,
    immutable: bool,
) -> Vec<DbJobReport> {
    let mut out = Vec::new();
    for entry in &manifest.dbs {
        // Read-only: open the connection directly without going through MemoryStore::open
        // which would try to write schema. We use rusqlite OpenFlags to be paranoid.
        let conn = match if immutable {
            memcore::db::open_immutable_readonly(&crate::doctor::make_immutable_uri(Path::new(
                &entry.path,
            )))
        } else {
            rusqlite::Connection::open_with_flags(
                &entry.path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
        } {
            Ok(c) => c,
            Err(e) => {
                out.push(DbJobReport {
                    path: entry.path.clone(),
                    histogram: memcore::JobStatusHistogram::default(),
                    error: format!("open failed: {e}"),
                });
                continue;
            }
        };
        let hist = match memcore::job_status_histogram(&conn, 30) {
            Ok(h) => h,
            Err(e) => {
                out.push(DbJobReport {
                    path: entry.path.clone(),
                    histogram: memcore::JobStatusHistogram::default(),
                    error: format!("histogram failed: {e}"),
                });
                continue;
            }
        };
        // Skip silent zero-job DBs to keep the report focused.
        if hist.total > 0 {
            out.push(DbJobReport {
                path: entry.path.clone(),
                histogram: hist,
                error: String::new(),
            });
        }
    }
    out
}

pub(super) async fn run_manifest_command(
    action: ManifestAction,
    home: &Path,
    app_home: &Path,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let manifest_path = manifest_path(app_home);

    match action {
        ManifestAction::Show { json } => {
            let m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            if json {
                print_pretty_json(&serde_json::to_value(&m)?)
            } else {
                println!("{}", crate::manifest::render_manifest(&m));
                println!("(stored at {})", manifest_path.display());
                Ok(())
            }
        }
        ManifestAction::Init | ManifestAction::Refresh => {
            let roots = default_scan_roots(home, app_home, git_root);
            let quarantine_dir = app_home.join("quarantine");
            let opts = crate::doctor::ScanOptions {
                auto_fix: false,
                max_depth: 10,
            };
            let report = crate::doctor::scan(&roots, &quarantine_dir, opts);
            let mut m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            m.populate_from_doctor(&report);
            m.save(&manifest_path)?;
            println!(
                "manifest written: {} ({} dbs)",
                manifest_path.display(),
                m.dbs.len()
            );
            Ok(())
        }
        ManifestAction::Resolve { target } => {
            let m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            // Try exact path first.
            if let Some(e) = m.lookup(&target) {
                println!(
                    "[exact] {} role={:?} owner={} writable={} schema={} last={}",
                    e.path, e.role, e.owner, e.allow_write, e.schema_kind, e.last_classification
                );
                return Ok(());
            }
            // Else interpret as a scope hint.
            let matches: Vec<_> = m
                .dbs
                .iter()
                .filter(|e| e.scope_hint == target || e.owner == target)
                .collect();
            if matches.is_empty() {
                println!(
                    "no manifest entry matches '{target}' (try `tachi manifest show` to list, or `tachi doctor` to refresh)"
                );
            } else {
                for e in matches {
                    println!(
                        "[scope] {} role={:?} owner={} writable={} schema={} last={}",
                        e.path,
                        e.role,
                        e.owner,
                        e.allow_write,
                        e.schema_kind,
                        e.last_classification
                    );
                }
            }
            Ok(())
        }
        ManifestAction::Sweep { apply, json } => {
            let roots = default_scan_roots(home, app_home, git_root);
            let quarantine_dir = app_home.join("quarantine");
            let opts = crate::doctor::ScanOptions {
                auto_fix: false,
                max_depth: 10,
            };
            let report = crate::doctor::scan(&roots, &quarantine_dir, opts);
            let m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            let mut plan = crate::manifest::plan_sweep(&report, &m, &quarantine_dir);
            if apply {
                plan = crate::manifest::apply_sweep(plan, &quarantine_dir);
            }
            if json {
                print_pretty_json(&serde_json::to_value(&plan)?)
            } else {
                println!(
                    "sweep {}: planned={} applied={} skipped={}",
                    if apply { "applied" } else { "dry-run" },
                    plan.planned.len(),
                    plan.applied.len(),
                    plan.skipped.len()
                );
                for a in &plan.planned {
                    println!(
                        "  [plan]   {} → {}  ({})",
                        a.path,
                        a.quarantine_to.as_deref().unwrap_or("-"),
                        a.reason
                    );
                }
                for a in &plan.applied {
                    println!(
                        "  [moved]  {} → {}  ({})",
                        a.path,
                        a.quarantine_to.as_deref().unwrap_or("-"),
                        a.reason
                    );
                }
                for a in &plan.skipped {
                    println!("  [skip]   {}  ({})", a.path, a.note);
                }
                if !apply {
                    println!("\n(dry-run; re-run with --apply to move files)");
                }
                Ok(())
            }
        }
        ManifestAction::Gc { json } => {
            let report = crate::manifest::gc_manifest(&manifest_path)?;
            if json {
                print_pretty_json(&serde_json::to_value(&report)?)
            } else {
                println!(
                    "manifest gc: before={} after={} canonicalized={} removed_missing={} removed_fixture={} schema_kind_fixed={} dedup_collapsed={}{}",
                    report.entries_before,
                    report.entries_after,
                    report.canonicalized,
                    report.removed_missing,
                    report.removed_fixture,
                    report.schema_kind_fixed,
                    report.dedup_collapsed,
                    if report.aborted {
                        format!(" ABORTED: {}", report.abort_reason.as_deref().unwrap_or("?"))
                    } else {
                        String::new()
                    }
                );
                println!("(manifest at {})", manifest_path.display());
                Ok(())
            }
        }
        ManifestAction::AuditProjects { json, apply } => {
            run_audit_projects(apply, json, app_home, git_root, &manifest_path)
        }
    }
}

/// DRY-RUN project-DB relocation audit. Enumerates
/// `<app_home>/projects/*/memory.db`, classifies each, and prints a relocation
/// plan. Moves/deletes NOTHING. `--apply` is intentionally refused for now —
/// any real mutation must take a backup and refuse on ambiguity, which is out of
/// scope for this read-only audit surface.
fn run_audit_projects(
    apply: bool,
    json: bool,
    app_home: &Path,
    git_root: Option<&PathBuf>,
    manifest_path: &Path,
) -> Result<(), Box<dyn Error>> {
    use memory_server_manifest_audit::{build_plan, gather_project_inputs, render_plan};

    let projects_dir = app_home.join("projects");

    // (scope_hint, path) pairs for project-role entries, used to resolve owning
    // repos from repo-local `.tachi/memory.db` paths already in the manifest.
    let manifest = crate::manifest::Manifest::load_or_empty(manifest_path);
    let manifest_project_paths: Vec<(String, String)> = manifest
        .dbs
        .iter()
        .filter(|e| matches!(e.role, crate::manifest::DbRole::Project))
        .map(|e| (e.scope_hint.clone(), e.path.clone()))
        .collect();

    // Candidate git roots: the current invocation's git root (if any). Kept
    // minimal and read-only; the manifest path resolution covers most cases.
    let candidate_git_roots: Vec<PathBuf> = git_root.into_iter().cloned().collect();

    let inputs =
        gather_project_inputs(&projects_dir, &manifest_project_paths, &candidate_git_roots)?;
    let plan = build_plan(&inputs);

    if json {
        print_pretty_json(&serde_json::to_value(&plan)?)?;
    } else {
        print!("{}", render_plan(&plan, &projects_dir));
        println!(
            "\nsummary: {} relocatable, {} home-resident, {} symlink-alias, {} broken-symlink, {} garbage",
            plan.n_relocatable,
            plan.n_home_resident,
            plan.n_symlink_alias,
            plan.n_symlink_broken,
            plan.n_garbage,
        );
        println!("(DRY-RUN: nothing was moved or deleted)");
    }

    if apply {
        // Explicit refusal: this audit surface is plan-only by design. A real
        // mutation pass must back up first and refuse on ambiguity; until that
        // is built and reviewed, --apply must not touch real per-project data.
        return Err(
            "--apply is not yet implemented for `manifest audit-projects`: this command is \
             plan-only and refuses to move or delete real project data. Review the printed plan \
             and relocate manually, or wait for the guarded --apply pass."
                .into(),
        );
    }

    Ok(())
}
