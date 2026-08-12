use super::*;

pub(crate) fn resolve_app_home() -> PathBuf {
    std::env::var("TACHI_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".tachi")
        })
}

pub(crate) fn runtime_observability_json(
    server: &crate::MemoryServer,
    app_home: &Path,
    daemon: Option<&DaemonStatus>,
    verbose: bool,
) -> serde_json::Value {
    let current_pid = std::process::id();
    let binary = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string());
    let collected_daemon;
    let daemon = match daemon {
        Some(daemon) => Some(daemon),
        None => {
            collected_daemon = collect_daemon_status(app_home, &server.global_db_path_buf());
            Some(&collected_daemon)
        }
    };
    let (daemon_pid, daemon_state, daemon_reason, daemon_global_db, daemon_authoritative) =
        match daemon {
            Some(DaemonStatus::Running { pid, .. }) => (Some(*pid), "running", None, None, true),
            Some(DaemonStatus::Foreign {
                pid,
                reason,
                global_db,
                ..
            }) => (
                Some(*pid),
                "foreign",
                Some(reason.clone()),
                global_db.clone(),
                false,
            ),
            Some(DaemonStatus::StalePid { pid, .. }) => (Some(*pid), "stale", None, None, false),
            Some(DaemonStatus::None) => (None, "none", None, None, false),
            None => (
                read_pid_file(app_home.join("daemon.lock")),
                "unknown",
                None,
                None,
                false,
            ),
        };
    let daemon_process_running = daemon_pid.map(process_alive).unwrap_or(false);
    let daemon_running = daemon_process_running && daemon_authoritative;
    let serving_daemon = daemon_pid
        .map(|pid| daemon_running && pid as u32 == current_pid)
        .unwrap_or(false);
    let mode = if serving_daemon {
        "daemon"
    } else if daemon_running {
        "sidecar_or_stdio"
    } else {
        "single_process"
    };
    let process_role = if serving_daemon {
        "daemon_authority"
    } else if daemon_running {
        "stdio_daemon_client"
    } else {
        "embedded_stdio"
    };
    let authoritative_runtime = if daemon_running {
        "daemon"
    } else {
        "current_process"
    };
    let stdio_adapter = !serving_daemon;
    let write_forwarding = json!({
        "expected": daemon_running && !serving_daemon,
        "target": if daemon_running && !serving_daemon { "daemon" } else { "current_process" },
        "fallback": if daemon_running && !serving_daemon { "in_process_before_dispatch_only" } else { "none" },
    });
    let read_forwarding = json!({
        "expected": daemon_running && !serving_daemon,
        "target": if daemon_running && !serving_daemon { "daemon" } else { "current_process" },
        "fallback": if daemon_running && !serving_daemon { "in_process_on_transport_error" } else { "none" },
    });

    let vault = {
        let state = server.vault_read();
        let unlocked_for_seconds = state.unlock_time.map(|instant| instant.elapsed().as_secs());
        let lockout_remaining_seconds = state.failed_attempts.1.map(|until| {
            until
                .saturating_duration_since(std::time::Instant::now())
                .as_secs()
        });
        json!({
            // `auto_lock_expired()` is the single source of truth: when
            // auto-lock is disabled (secs == 0) it returns false, so the
            // runtime status reports `unlocked: true` as long as the key is
            // present — matching the enforcement path. The old bare
            // `elapsed <= auto_lock_after_secs` comparison reported
            // `unlocked: false` one second after unlock for a 0-configured
            // daemon while the key was still live.
            "unlocked": state.key.is_some() && !state.auto_lock_expired(),
            "unlocked_for_seconds": unlocked_for_seconds,
            "auto_lock_after_seconds": state.auto_lock_after_secs,
            "failed_attempts": state.failed_attempts.0,
            "lockout_remaining_seconds": lockout_remaining_seconds,
        })
    };

    // Deploy verification (#728): always surface THIS process's stamped build
    // identity. Agents must compare the *serving* daemon's git_sha (pid file /
    // health) against the intended release, never a local `cargo build` artifact
    // that is not on the launchd/brew path.
    let build = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": crate::build_info::GIT_SHA,
        "git_sha_short": crate::build_info::git_sha_short(),
        "build_time": crate::build_info::BUILD_TIME,
        "build_id": crate::build_info::build_version_string(),
    });

    let mut out = json!({
        "pid": current_pid,
        "binary": binary,
        "build": build,
        "mode": mode,
        "process_role": process_role,
        "authoritative_runtime": authoritative_runtime,
        "stdio_adapter": stdio_adapter,
        "write_forwarding": write_forwarding,
        "read_forwarding": read_forwarding,
        "serving_daemon": serving_daemon,
        "daemon": {
            "pid": daemon_pid,
            "running": daemon_running,
            "process_running": daemon_process_running,
            "authoritative": daemon_running,
            "state": daemon_state,
            "foreign": daemon_state == "foreign" && daemon_process_running,
            "reason": daemon_reason,
            "global_db": daemon_global_db,
            "matches_current_process": serving_daemon,
        },
        "provider_secret_count": server.llm.provider_secret_count(),
        "vault": vault,
        "host_profile": crate::host_profile::runtime_json(),
    });
    // The full provider_health/provider_pools arrays are heavy (~20 entries
    // each) and duplicate what the status `api_keys` block already carries.
    // Only emit them in verbose surfaces (runtime_info); the default `tachi
    // status` runtime block stays a compact routing/identity signal.
    if verbose {
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "provider_health".into(),
                serde_json::to_value(server.llm.provider_health_status())
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "provider_pools".into(),
                serde_json::to_value(server.llm.provider_pool_statuses())
                    .unwrap_or(serde_json::Value::Null),
            );
        }
    }
    out
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max <= 3 {
        s.chars().take(max).collect()
    } else {
        format!(
            "{}...",
            s.chars().take(max.saturating_sub(3)).collect::<String>()
        )
    }
}

fn slim_provider_health_value(value: serde_json::Value) -> serde_json::Value {
    let has_error = value
        .get("last_error")
        .is_some_and(|error| !error.is_null())
        || value
            .get("persist_last_error")
            .is_some_and(|error| !error.is_null());
    if has_error {
        return value;
    }

    json!({
        "status": "ok",
        "last_success_age_secs": value
            .get("last_success_age_secs")
            .and_then(serde_json::Value::as_u64)
            .or_else(|| {
                value
                    .get("persist_last_success_age_secs")
                    .and_then(serde_json::Value::as_u64)
            })
            .unwrap_or(0),
        "source_of_truth": value
            .get("source_of_truth")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or_else(|| json!("unknown")),
    })
}

fn status_provider_health_json(server: &crate::MemoryServer) -> serde_json::Value {
    let value = serde_json::to_value(server.llm.provider_health_status())
        .unwrap_or(serde_json::Value::Null);
    slim_provider_health_value(value)
}

/// Render a status response `Value` per the raw `tachi_status` tool's
/// `format` semantics (tachi#1201 k3): defaults to markdown, "json" opts
/// into the full JSON payload unchanged.
fn render_status_response(
    value: &serde_json::Value,
    format: Option<&str>,
) -> Result<String, String> {
    if crate::agent_markdown::wants_explicit_json(format) {
        serde_json::to_string(value).map_err(|e| e.to_string())
    } else {
        Ok(crate::agent_markdown::format_status_markdown(value))
    }
}

/// MCP tool handler: returns a concise health summary for agents.
///
/// `format`: omitted/anything other than "json" renders a compact markdown
/// digest (tachi#1201 k3 default); "json" returns the full JSON payload,
/// byte-identical to the pre-k3 unconditional shape.
pub(crate) async fn handle_tachi_status_agent(
    server: &crate::MemoryServer,
    format: Option<&str>,
) -> Result<String, String> {
    handle_tachi_status_detail(server, false, format).await
}

/// Full diagnostic payload for tests, doctor flows, and readiness checks.
/// Same `format` semantics as [`handle_tachi_status_agent`].
pub(crate) async fn handle_tachi_status_full(
    server: &crate::MemoryServer,
    format: Option<&str>,
) -> Result<String, String> {
    handle_tachi_status_detail(server, true, format).await
}

async fn handle_tachi_status_detail(
    server: &crate::MemoryServer,
    full: bool,
    format: Option<&str>,
) -> Result<String, String> {
    let app_home = server.tachi_home_dir();
    let global_db_path = server.global_db_path_buf();
    let project_db_path = server.project_db_path_buf();

    let snapshot = collect_snapshot(&app_home, &global_db_path, project_db_path.as_deref());

    let daemon_state = match &snapshot.daemon {
        DaemonStatus::Running { pid, .. } => json!({
            "running": true,
            "pid": pid,
        }),
        DaemonStatus::Foreign {
            pid,
            reason,
            version,
            port,
            global_db,
            ..
        } => json!({
            "running": false,
            "foreign": true,
            "pid": pid,
            "reason": reason,
            "version": version,
            "port": port,
            "global_db": global_db,
        }),
        DaemonStatus::StalePid { pid, .. } => json!({
            "running": false,
            "stale": true,
            "pid": pid,
        }),
        DaemonStatus::None => json!({
            "running": false,
        }),
    };

    let total_dbs = snapshot.dbs.len();
    let total_pending: usize = snapshot.dbs.iter().map(|d| d.pending).sum();
    let total_active: usize = snapshot.dbs.iter().map(|d| d.active_jobs).sum();
    let total_terminal: usize = snapshot.dbs.iter().map(|d| d.terminal_jobs).sum();
    let total_failed: usize = snapshot.dbs.iter().map(|d| d.failed).sum();
    let total_stuck: usize = snapshot.dbs.iter().map(|d| d.stuck_in_progress).sum();
    let total_outcome_events: usize = snapshot
        .dbs
        .iter()
        .map(|d| d.continuity.session_outcomes.outcome_events)
        .sum();
    let total_eligible_outcomes: usize = snapshot
        .dbs
        .iter()
        .map(|d| d.continuity.session_outcomes.eligible_outcomes)
        .sum();
    let total_ai_corrected: usize = snapshot
        .dbs
        .iter()
        .map(|d| d.continuity.session_outcomes.ai_corrected)
        .sum();
    let aggregate_challenge_rate = (total_eligible_outcomes > 0)
        .then(|| total_ai_corrected as f64 / total_eligible_outcomes as f64);
    let continuity_summary = json!({
        "session_outcomes": {
            "outcome_events": total_outcome_events,
            "eligible_outcomes": total_eligible_outcomes,
            "ai_corrected": total_ai_corrected,
            "challenge_rate": aggregate_challenge_rate,
            "note": "read-only signal; not a verdict or routing gate",
        }
    });
    let worker_queues: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .map(|d| {
            json!({
                "label": d.label,
                "path": d.path,
                "queue_state": if d.active_jobs == 0 && d.failed == 0 && d.stuck_in_progress == 0 {
                    "idle"
                } else if d.stuck_in_progress > 0 {
                    "stuck"
                } else if d.failed > 0 {
                    "failed"
                } else {
                    "active"
                },
                "active_jobs": d.active_jobs,
                "pending_jobs": d.pending,
                "running_jobs": d.running,
                "failed_jobs": d.failed,
                "stuck_jobs": d.stuck_in_progress,
                "terminal_jobs": d.terminal_jobs,
                "completed_jobs": d.completed,
                "skipped_jobs": d.skipped,
                "gc_eligible_jobs": d.gc_eligible,
                "latest_active_job": &d.latest_active_job,
                "latest_terminal_job": &d.latest_terminal_job,
                "backfill": {
                    "needed": d.vector_missing.saturating_sub(d.pending_enrichment) > 0 || vector_dimension_mismatch(d),
                    "pending_enrichment": d.pending_enrichment,
                    "missing_vectors": d.vector_missing,
                    "coverage": d.vector_coverage,
                    "dimension": d.vector_dimension,
                    "expected_dimension": EXPECTED_EMBEDDING_DIM,
                    "command": if d.vector_missing.saturating_sub(d.pending_enrichment) > 0 || vector_dimension_mismatch(d) {
                        Some(status_health::format_backfill_command(d))
                    } else {
                        None
                    },
                },
                "vector_sweep": &d.vector_sweep,
                "vector_sweep_error": &d.vector_sweep_error,
                "namespace": &d.namespace,
                "continuity": &d.continuity,
            })
        })
        .collect();
    let low_coverage: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| low_vector_coverage(d))
        .map(|d| {
            json!({
                "label": d.label,
                "coverage": format!("{:.1}%", d.vector_coverage * 100.0),
                "missing": d.vector_missing,
                "pending_enrichment": d.pending_enrichment,
                "total": d.memory_total,
                "dimension": d.vector_dimension,
                "backfill_command": status_health::format_backfill_command(d),
            })
        })
        .collect();
    let vector_dimension_mismatches: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| vector_dimension_mismatch(d))
        .map(|d| {
            json!({
                "label": d.label,
                "path": d.path,
                "dimension": d.vector_dimension,
                "expected_dimension": EXPECTED_EMBEDDING_DIM,
                "remediation": "Vector rows exist but use an unexpected dimension; rebuild vectors with the current embedding model instead of treating this as missing-vector backfill.",
            })
        })
        .collect();
    let vector_orphan_dbs: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| d.vector_orphans > 0)
        .map(|d| {
            json!({
                "label": d.label,
                "path": d.path,
                "orphans": d.vector_orphans,
                "remediation": "Run `tachi repair --rule R7 --apply` to remove vector rows whose memory no longer exists.",
            })
        })
        .collect();
    let enrichment_failure_dbs: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| d.enrichment_failed_recent > 0)
        .map(|d| {
            json!({
                "label": d.label,
                "path": d.path,
                "enrichment_failed": d.enrichment_failed_recent,
                "failures": d.enrichment_failures,
                "remediation": "Inspect failed enrichment metadata and provider probes; vector backfill will not retry summary/metadata enrichment failures. After fixing provider/schema issues, run `tachi repair --rule R10 --apply --db <label>` to clear stale failed markers.",
            })
        })
        .collect();
    let namespace_issues: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| {
            d.namespace.recall_cache_rows > 0
                || d.namespace.wiki_non_source_rows > 0
                || d.namespace.graph_orphan_edges > 0
                || (d.memory_total > 0 && d.namespace.derived_items == 0)
        })
        .map(|d| {
            json!({
                "label": d.label,
                "path": d.path,
                "recall_cache_rows": d.namespace.recall_cache_rows,
                "wiki_rows": d.namespace.wiki_rows,
                "wiki_non_source_rows": d.namespace.wiki_non_source_rows,
                "wiki_non_category_rows": d.namespace.wiki_non_category_rows,
                "kanban_rows": d.namespace.kanban_rows,
                "handoff_rows": d.namespace.handoff_rows,
                "eval_rows": d.namespace.eval_rows,
                "project_scope_rows": d.namespace.project_scope_rows,
                "non_project_scope_rows": d.namespace.non_project_scope_rows,
                "derived_items": d.namespace.derived_items,
                "graph_edges": d.namespace.graph_edges,
                "graph_orphan_edges": d.namespace.graph_orphan_edges,
                "graph_relation_types": &d.namespace.graph_relation_types,
                "remediation": "Search excludes recall-cache rows by default. Use `tachi repair --rule R8 --dry-run` to inspect cache/junk candidates, and use these counts to plan namespace/schema follow-up without dumping memory contents.",
            })
        })
        .collect();
    let auth_failures: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter_map(|d| {
            d.latest_failed_job.as_ref().and_then(|job| {
                let reason = job.reason.as_deref()?;
                job.inferred_invalid_provider.is_some().then(|| {
                    json!({
                        "db": d.label,
                        "kind": job.kind,
                        "lane": job.lane,
                        "updated_at": job.updated_at,
                        "reason": reason,
                        "inferred_invalid_provider": job.inferred_invalid_provider,
                    })
                })
            })
        })
        .collect();
    let failed_jobs: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter_map(|d| {
            d.latest_failed_job.as_ref().map(|job| {
                json!({
                    "db": d.label,
                    "path": d.path,
                    "id": job.id,
                    "kind": job.kind,
                    "lane": job.lane,
                    "updated_at": job.updated_at,
                    "reason": job.reason,
                    "inferred_invalid_provider": job.inferred_invalid_provider,
                })
            })
        })
        .collect();
    let readiness = status_health::agent_readiness_json(&app_home, &snapshot);

    // Full diagnostic keeps the heavy provider arrays in `runtime`; the compact
    // agent surface gets a slim routing/identity block (api_keys already carries
    // the provider detail there, so the arrays don't need repeating).
    let runtime = runtime_observability_json(server, &app_home, Some(&snapshot.daemon), full);
    let mut warnings = build_status_warnings(&snapshot, &daemon_state);
    push_unregistered_project_db_warning(&mut warnings, &app_home);
    let recall_eval = crate::status_ops::recall_eval::read_recall_eval_status(&app_home);
    if let Some(warning) = crate::status_ops::recall_eval::recall_eval_warning(&recall_eval) {
        warnings.push(warning);
    }
    let running_daemons = snapshot
        .daemon_inventory
        .iter()
        .filter(|daemon| daemon.process_running)
        .count();
    if running_daemons > 1 {
        let scopes = snapshot
            .daemon_inventory
            .iter()
            .filter(|daemon| daemon.process_running)
            .map(|daemon| {
                daemon
                    .global_db
                    .as_deref()
                    .unwrap_or(daemon.scope.as_str())
                    .to_string()
            })
            .collect::<Vec<_>>();
        warnings.push(format!(
            "{running_daemons} tachi daemon scopes are running concurrently: {}",
            scopes.join(", ")
        ));
    }
    if runtime["daemon"]["running"].as_bool().unwrap_or(false)
        && !runtime["daemon"]["matches_current_process"]
            .as_bool()
            .unwrap_or(false)
    {
        warnings.push(
            "this MCP process is a stdio adapter while a daemon is running; supported reads and writes should forward to the daemon, otherwise restart stale MCP clients if runtime state looks inconsistent"
                .to_string(),
        );
    }

    // Component governance for active workspace (#799) — registry only.
    let named_project = crate::memory_search_ops::resolve_workspace_named_project();
    let component_governance = crate::component_governance_ops::component_governance_context(
        server,
        named_project.as_deref(),
        None,
    )
    .unwrap_or_else(|e| {
        json!({
            "status": "error",
            "matches": [],
            "note": format!("component governance unavailable: {e}"),
        })
    });
    for line in
        crate::component_governance_ops::component_governance_warning_lines(&component_governance)
    {
        if !warnings.iter().any(|w| w == &line) {
            warnings.push(line);
        }
    }

    if full {
        let value = json!({
            "daemon": daemon_state,
            "daemon_inventory": snapshot.daemon_inventory,
            "runtime": runtime,
            "version": env!("CARGO_PKG_VERSION"),
            "health_score": snapshot.health_score,
            "health_deductions": snapshot.health_deductions,
            "databases": {
                "total": total_dbs,
                "active_jobs": total_active,
                "pending_jobs": total_pending,
                "terminal_jobs": total_terminal,
                "failed_jobs": total_failed,
                "stuck_jobs": total_stuck,
                "worker_queues": worker_queues,
                "low_vector_coverage": low_coverage,
                "vector_dimension_mismatches": vector_dimension_mismatches,
                "vector_orphans": vector_orphan_dbs,
                "enrichment_failures": enrichment_failure_dbs,
                "namespace_issues": namespace_issues,
                "continuity": continuity_summary,
                "plan_c_split_brain": snapshot.plan_c_split_brain,
                "plan_c_alias_integrity": snapshot.plan_c_alias_integrity,
                "provider_auth_failures": auth_failures,
                "latest_failed_jobs": failed_jobs,
            },
            "component_governance": component_governance,
            "warnings": warnings,
            "recall_eval": recall_eval,
            "daily_pipeline": snapshot.last_daily_report,
            "distill": snapshot.distill_marker,
            "api_keys": snapshot.api_keys,
            "provider_health": status_provider_health_json(server),
            "provider_pools": server.llm.provider_pool_statuses(),
            "provider_probe_cache": snapshot.provider_probe_cache,
            "models": status_health::model_lanes_json_for_running_client(&server.llm.runtime_config()),
            "agent_readiness": readiness,
        });
        render_status_response(&value, format)
    } else {
        let api_key_drift = snapshot
            .api_keys
            .iter()
            .filter(|key| key.status == "drift")
            .count();
        let api_key_missing = snapshot
            .api_keys
            .iter()
            .filter(|key| key.required && key.status == "missing")
            .count();
        let distill = snapshot.distill_marker.as_ref().map(|marker| {
            json!({
                "is_stale": marker.is_stale,
                "age": marker.age,
            })
        });
        // Compact surface: the agent default doesn't need the full ~20-entry
        // provider_pools array (it lives in `handle_tachi_status_full` and
        // `runtime_info`). A {total, rate_limited} summary keeps it a glance.
        let pool_statuses = server.llm.provider_pool_statuses();
        let provider_pools_summary = json!({
            "total": pool_statuses.len(),
            "rate_limited": pool_statuses
                .iter()
                .filter(|p| !p.rate_limited_keys.is_empty())
                .count(),
        });
        let mut response = json!({
            "detail": "agent",
            "daemon": daemon_state,
            "daemon_inventory": snapshot.daemon_inventory,
            "runtime": runtime,
            "version": env!("CARGO_PKG_VERSION"),
            "health_score": snapshot.health_score,
            "health_deductions": snapshot.health_deductions,
            "warnings": warnings.into_iter().take(8).collect::<Vec<_>>(),
            "recall_eval": recall_eval,
            "jobs": {
                "active": total_active,
                "failed": total_failed,
                "pending": total_pending,
                "stuck": total_stuck,
                "terminal_history": total_terminal,
            },
            "distill": distill,
            "vector_coverage_issues": low_coverage.len(),
            "vector_orphans": vector_orphan_dbs.len(),
            "enrichment_failures": enrichment_failure_dbs.len(),
            "namespace_issues": namespace_issues.len(),
            "plan_c_split_brain": snapshot.plan_c_split_brain.len(),
            "plan_c_alias_integrity": snapshot.plan_c_alias_integrity.len(),
            "provider_auth_failures": auth_failures.len(),
            "api_keys": {
                "drift": api_key_drift,
                "missing_required": api_key_missing,
                "provider_health": status_provider_health_json(server),
                "provider_pools": provider_pools_summary,
            },
            "provider_probe_cache": snapshot.provider_probe_cache,
            "doctor_hint": readiness.get("doctor_hint"),
            "component_governance": component_governance,
        });
        if total_outcome_events > 0 {
            response
                .as_object_mut()
                .expect("status response object")
                .insert("continuity".to_string(), continuity_summary);
        }
        render_status_response(&response, format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slim_provider_health_value_keeps_full_shape_when_error_exists() {
        let value = json!({
            "source_of_truth": "vault_db",
            "reload_ttl_secs": 300,
            "last_attempt_at": "2026-07-03T00:00:00Z",
            "last_success_at": null,
            "last_success_age_secs": null,
            "last_error": "forced auth failure",
            "persist_last_attempt_at": null,
            "persist_last_success_at": null,
            "persist_last_success_age_secs": null,
            "persist_last_error": null
        });

        let slimmed = slim_provider_health_value(value);

        assert_eq!(slimmed["last_error"], json!("forced auth failure"));
        assert_eq!(slimmed["reload_ttl_secs"], json!(300));
        assert!(slimmed.get("status").is_none());
    }

    #[test]
    fn slim_provider_health_value_uses_persisted_fallback_when_primary_is_null() {
        let value = json!({
            "source_of_truth": null,
            "last_success_age_secs": null,
            "last_error": null,
            "persist_last_success_age_secs": 42,
            "persist_last_error": null
        });

        let slimmed = slim_provider_health_value(value);

        assert_eq!(slimmed["status"], json!("ok"));
        assert_eq!(slimmed["last_success_age_secs"], json!(42));
        assert_eq!(slimmed["source_of_truth"], json!("unknown"));
    }
}
