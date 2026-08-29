use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use super::inference::is_auth_error;
use super::rotation::collect_rotation_sources;
use super::types::{ProviderProbeReport, ProviderProbeResult, ProviderRotationGroupProbe};
use super::vault::load_keychain_vault_api_key_values;
use crate::status_ops::ApiKeyRotationMemberStatus;

pub(crate) const PROVIDER_HEALTH_PERSIST_PHASE: &str = "provider_health_persist";
const PROVIDER_HEALTH_PERSIST_JOIN_TIMEOUT: Duration = Duration::from_secs(10);

async fn provider_health_persist_receipt<F>(terminal: F, timeout: Duration) -> ProviderProbeResult
where
    F: Future<Output = Result<(), String>>,
{
    let started = tokio::time::Instant::now();
    let mut terminal = Box::pin(terminal);
    match tokio::time::timeout(timeout, &mut terminal).await {
        Ok(Ok(())) => ProviderProbeResult {
            name: PROVIDER_HEALTH_PERSIST_PHASE.to_string(),
            status: "ok".to_string(),
            message: Some(format!(
                "phase={PROVIDER_HEALTH_PERSIST_PHASE} elapsed_ms={} cause=none",
                started.elapsed().as_millis()
            )),
        },
        Ok(Err(cause)) => provider_health_persist_failure_receipt(started, cause),
        Err(_) => ProviderProbeResult {
            name: PROVIDER_HEALTH_PERSIST_PHASE.to_string(),
            status: "timeout".to_string(),
            message: Some(format!(
                "phase={PROVIDER_HEALTH_PERSIST_PHASE} elapsed_ms={} timeout_ms={} cause=provider_health_persist_join_timeout writer_joined=true terminal_cause={}",
                started.elapsed().as_millis(),
                timeout.as_millis(),
                provider_health_persist_terminal_cause(terminal.await),
            )),
        },
    }
}

fn provider_health_persist_terminal_cause(result: Result<(), String>) -> String {
    match result {
        Ok(()) => "none".to_string(),
        Err(cause) => cause,
    }
}

fn provider_health_persist_failure_receipt(
    started: tokio::time::Instant,
    cause: String,
) -> ProviderProbeResult {
    let status = if cause.contains(tachi_llm::PROVIDER_HEALTH_PERSIST_SQLITE_DEADLINE_CAUSE) {
        "timeout"
    } else if cause.contains(tachi_llm::PROVIDER_HEALTH_PERSIST_CANCELLED_CAUSE) {
        "cancelled"
    } else {
        "failed"
    };
    ProviderProbeResult {
        name: PROVIDER_HEALTH_PERSIST_PHASE.to_string(),
        status: status.to_string(),
        message: Some(format!(
            "phase={PROVIDER_HEALTH_PERSIST_PHASE} elapsed_ms={} cause={cause}",
            started.elapsed().as_millis()
        )),
    }
}

pub(crate) async fn run_provider_probe_report(global_db_path: &Path) -> ProviderProbeReport {
    run_provider_probe_report_with_migration_authority(
        global_db_path,
        &memcore::MigrationAuthority::Deny,
    )
    .await
}

pub(super) async fn run_provider_probe_report_with_migration_authority(
    global_db_path: &Path,
    migration: &memcore::MigrationAuthority,
) -> ProviderProbeReport {
    let llm = match probe_llm_client_with_migration_authority(global_db_path, migration) {
        Ok(client) => client,
        Err(err) => {
            return ProviderProbeReport {
                probes: vec![ProviderProbeResult {
                    name: "llm_client".to_string(),
                    status: "failed".to_string(),
                    message: Some(err),
                }],
                rotation_groups: run_rotation_group_probes(global_db_path).await,
            };
        }
    };
    let skipped_alias_probe =
        match crate::provider_config::materialize_standalone(&llm, global_db_path) {
            Ok(report) => skipped_alias_probe_result(&report),
            Err(err) => {
                tracing::warn!("[provider] probe secret materialization failed: {err}");
                None
            }
        };

    // Run probes concurrently so adding chat lanes does not make status
    // probes serially accumulate their timeout budgets.
    let rerank_docs = vec![
        "Tachi stores operational memory".to_string(),
        "Unrelated weather note".to_string(),
    ];
    let llm_embed = llm.clone();
    let llm_rerank = llm.clone();
    let llm_extract = llm.clone();
    // Thinking-default Flash/GLM spend the first tokens on reasoning; 8
    // produced finish_reason=length with empty content on live probes.
    const PROVIDER_CHAT_PROBE_MAX_TOKENS: u32 = 64;
    let (embed, rerank, chat_extract, chat_distill) = tokio::join!(
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            llm_embed.embed_voyage("tachi provider probe", "document"),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            llm_rerank.rerank("tachi provider probe", &rerank_docs, 1),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            llm_extract.call_extract_llm(
                "Return exactly OK.",
                "Provider extract probe. Reply OK only.",
                None,
                0.0,
                PROVIDER_CHAT_PROBE_MAX_TOKENS,
            ),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            llm.call_distill_llm(
                "Return exactly OK.",
                "Provider distill probe. Reply OK only.",
                None,
                0.0,
                PROVIDER_CHAT_PROBE_MAX_TOKENS,
            ),
        ),
    );

    let mut out = Vec::new();
    if let Some(probe) = skipped_alias_probe {
        out.push(probe);
    }
    out.push(match embed {
        Ok(Ok(vec)) => ProviderProbeResult {
            name: "voyage_embed".to_string(),
            status: "ok".to_string(),
            message: Some(format!("{} dims", vec.len())),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "voyage_embed".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "voyage_embed".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 15s".to_string()),
        },
    });
    let rerank_probe_name = match tachi_llm::RerankConfig::from_env() {
        Ok(cfg) => format!("{}_rerank", cfg.provider_name()),
        Err(_) => "rerank".to_string(),
    };
    out.push(match rerank {
        Ok(Ok(rows)) => ProviderProbeResult {
            name: rerank_probe_name.clone(),
            status: "ok".to_string(),
            message: Some(format!("{} result(s)", rows.len())),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: rerank_probe_name.clone(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: rerank_probe_name,
            status: "timeout".to_string(),
            message: Some("timed out after 15s".to_string()),
        },
    });
    out.push(match chat_extract {
        Ok(Ok(text)) => ProviderProbeResult {
            name: "chat_extract".to_string(),
            status: "ok".to_string(),
            message: Some(text.chars().take(80).collect()),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "chat_extract".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "chat_extract".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 20s".to_string()),
        },
    });
    out.push(match chat_distill {
        Ok(Ok(text)) => ProviderProbeResult {
            name: "chat_distill".to_string(),
            status: "ok".to_string(),
            message: Some(text.chars().take(80).collect()),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "chat_distill".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "chat_distill".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 20s".to_string()),
        },
    });
    // The probe client is standalone and about to be dropped. Provider calls
    // deliberately persist health off the async runtime thread during normal
    // serving, but this one-shot phase must join those writers before doctor
    // constructs the distill phase's write-capable MemoryServer (#1505).
    out.push(
        provider_health_persist_receipt(
            llm.await_provider_health_persistence(),
            PROVIDER_HEALTH_PERSIST_JOIN_TIMEOUT,
        )
        .await,
    );
    ProviderProbeReport {
        probes: out,
        rotation_groups: run_rotation_group_probes(global_db_path).await,
    }
}

/// tachi#1287 fix 1: the accessor refresh seam
/// (`server_state/accessors.rs::refresh_llm_provider_secrets_from_vault`)
/// already logs every `MaterializeReport.skipped_aliases` entry loudly, but
/// this health-probe seam only checked the `Err` branch of
/// `materialize_standalone` and silently discarded `Ok(report)` — so a
/// per-alias skip on the probe path had no signal in the probe report itself,
/// only (maybe) in tracing output the status snapshot doesn't capture.
/// Surface it both ways: log each skip, and return a `ProviderProbeResult` so
/// `status doctor`/health snapshots show it.
pub(super) fn skipped_alias_probe_result(
    report: &tachi_llm::MaterializeReport,
) -> Option<ProviderProbeResult> {
    if report.skipped_aliases.is_empty() {
        return None;
    }
    for (key, _reason) in &report.skipped_aliases {
        let retained = report
            .retained_from_last_known_good
            .iter()
            .any(|retained_key| retained_key == key);
        tracing::warn!(
            "{}",
            crate::provider_config::format_skipped_alias_warning(
                key,
                retained,
                report.source_availability,
            )
        );
    }
    Some(ProviderProbeResult {
        name: "provider_secret_materialization".to_string(),
        status: "degraded".to_string(),
        message: Some(crate::provider_config::describe_skipped_alias_report(
            report,
        )),
    })
}

fn probe_llm_client_with_migration_authority(
    global_db_path: &Path,
    migration: &memcore::MigrationAuthority,
) -> Result<tachi_llm::LlmClient, String> {
    tachi_llm::LlmClient::new_with_vault_db_and_migration_authority(
        Some(global_db_path),
        migration.clone(),
    )
}

#[cfg(test)]
pub(crate) fn probe_llm_client_for_tests(
    global_db_path: &Path,
) -> Result<tachi_llm::LlmClient, String> {
    probe_llm_client_with_migration_authority(global_db_path, &memcore::MigrationAuthority::Deny)
}

pub(super) async fn run_provider_probes(global_db_path: &Path) -> Vec<ProviderProbeResult> {
    run_provider_probe_report(global_db_path).await.probes
}

pub(super) async fn run_rotation_group_probes(
    global_db_path: &Path,
) -> Vec<ProviderRotationGroupProbe> {
    let rotation_sources = collect_rotation_sources(global_db_path);
    if rotation_sources.is_empty() {
        return Vec::new();
    }
    let values = match load_keychain_vault_api_key_values(global_db_path) {
        Ok(values) => values,
        Err(err) => {
            tracing::warn!("[vault] keychain vault read failed during rotation probes: {err}");
            Vec::new()
        }
    }
    .into_iter()
    .collect::<HashMap<_, _>>();
    let probed_at = chrono::Utc::now().to_rfc3339();

    let mut groups = Vec::new();
    for (logical_name, source) in rotation_sources {
        let mut keys = Vec::new();
        for key_id in &source.members {
            let (status, message) = match values.get(key_id) {
                Some(value) => {
                    probe_rotation_member(global_db_path, &logical_name, key_id, value.clone())
                        .await
                }
                None => (
                    "unavailable".to_string(),
                    Some(
                        "Vault key material unavailable; unlock Vault/Keychain for live probe"
                            .to_string(),
                    ),
                ),
            };
            keys.push(ApiKeyRotationMemberStatus {
                name: key_id.clone(),
                status,
                message,
                last_probe_at: Some(probed_at.clone()),
            });
        }
        let healthy_keys = keys.iter().filter(|key| key.status == "ok").count() as i64;
        let rate_limited_keys = keys
            .iter()
            .filter(|key| key.status == "rate_limited")
            .count() as i64;
        let auth_failed_keys = keys
            .iter()
            .filter(|key| key.status == "auth_failed")
            .count() as i64;
        groups.push(ProviderRotationGroupProbe {
            logical_name,
            total_keys: source.total_keys,
            configured_keys: source.members.len() as i64,
            healthy_keys,
            rate_limited_keys,
            auth_failed_keys,
            current_index: source.current_index,
            strategy: source.strategy,
            next_retry_at: None,
            keys,
        });
    }
    groups.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
    groups
}

async fn probe_rotation_member(
    global_db_path: &Path,
    logical_name: &str,
    key_id: &str,
    value: String,
) -> (String, Option<String>) {
    let client = match tachi_llm::LlmClient::new() {
        Ok(client) => client,
        Err(err) => return ("failed".to_string(), Some(err)),
    };
    if let Err(err) = client.clear_provider_secrets() {
        return ("failed".to_string(), Some(err));
    }
    client.set_provider_secret_pool(
        logical_name,
        vec![tachi_llm::ProviderSecret {
            key_id: key_id.to_string(),
            value: value.clone(),
        }],
    );

    let result = match logical_name {
        "VOYAGE_API_KEY" => {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                client.embed_voyage("tachi provider rotation probe", "document"),
            )
            .await;
            match result {
                Ok(Ok(vec)) => Ok(format!("{} dims", vec.len())),
                Ok(Err(err)) => Err(err),
                Err(_) => Err("timed out after 15s".to_string()),
            }
        }
        "VOYAGE_RERANK_API_KEY" => {
            let docs = vec![
                "Tachi stores operational memory".to_string(),
                "Unrelated weather note".to_string(),
            ];
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                client.rerank_voyage("tachi provider rotation probe", &docs, 1),
            )
            .await;
            match result {
                Ok(Ok(rows)) => Ok(format!("{} result(s)", rows.len())),
                Ok(Err(err)) => Err(err),
                Err(_) => Err("timed out after 15s".to_string()),
            }
        }
        "SILICONFLOW_API_KEY" | "EXTRACT_API_KEY" => {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                client.call_extract_llm(
                    "Return exactly OK.",
                    "Provider rotation probe. Reply OK only.",
                    None,
                    0.0,
                    64,
                ),
            )
            .await;
            match result {
                Ok(Ok(text)) => Ok(text.chars().take(80).collect()),
                Ok(Err(err)) => Err(err),
                Err(_) => Err("timed out after 20s".to_string()),
            }
        }
        "DISTILL_API_KEY" => {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                client.call_distill_llm(
                    "Return exactly OK.",
                    "Provider distill rotation probe. Reply OK only.",
                    None,
                    0.0,
                    64,
                ),
            )
            .await;
            match result {
                Ok(Ok(text)) => Ok(text.chars().take(80).collect()),
                Ok(Err(err)) => Err(err),
                Err(_) => Err("timed out after 20s".to_string()),
            }
        }
        // #1680 D6: every logical key without a hardcoded *generating* probe
        // lane above used to stop here at "unsupported". It now asks the
        // registry whether this key's family has a documented, non-generating
        // authentication endpoint, and probes this member by name if it does.
        _ => {
            return probe_member_auth_from_registry(global_db_path, logical_name, key_id, value)
                .await
        }
    };

    match result {
        Ok(message) => ("ok".to_string(), Some(message)),
        Err(err) => classify_provider_probe_error(err),
    }
}

/// Probe one named pool member through the registry's probe descriptor
/// (#1680 D6), recording the verdict as that member's health with
/// `EvidenceKind::Probed`.
///
/// This is the generalization D6 asked for: the auth probe used to be
/// reachable only as "probe whichever credential the reasoning lane would
/// pick", and is now callable for any `(logical_name, key_id)` whose family
/// the registry declares probeable. Which names are in scope is the
/// registry's decision; which hosts may be dialed remains a compile-time
/// constant in `tachi_llm`'s probe table.
///
/// The client is vault-DB-backed on purpose: the probe records health, so it
/// must both read this member's existing row (a probe reports on one member,
/// it does not rebuild it from nothing) and persist the result. It writes
/// `vault_key_health` and nothing else — account rows are `reconcile apply`'s
/// alone.
async fn probe_member_auth_from_registry(
    global_db_path: &Path,
    logical_name: &str,
    key_id: &str,
    value: String,
) -> (String, Option<String>) {
    let unsupported = || {
        (
            "unsupported".to_string(),
            Some("No live probe lane is registered for this logical key".to_string()),
        )
    };
    let Some(descriptor) = super::auth_probe_descriptor_for_env_name(logical_name) else {
        return unsupported();
    };

    let client = match tachi_llm::LlmClient::new_with_vault_db(Some(global_db_path)) {
        Ok(client) => client,
        Err(err) => return ("failed".to_string(), Some(err)),
    };
    if let Err(err) = client.clear_provider_secrets() {
        return ("failed".to_string(), Some(err));
    }
    client.set_provider_secret_pool(
        logical_name,
        vec![tachi_llm::ProviderSecret {
            key_id: key_id.to_string(),
            value,
        }],
    );

    let (result, _health) = client
        .probe_member_auth_and_record(descriptor, logical_name, key_id)
        .await;
    // Messages name the class and the count, never a provider response body or
    // a model id — the probe receipt is deliberately body-free.
    match result.auth_class {
        tachi_llm::ProviderAuthProbeClass::AuthOk => (
            "ok".to_string(),
            Some(match result.model_count {
                Some(count) => format!("auth probe ok; {count} model(s) visible"),
                None => "auth probe ok".to_string(),
            }),
        ),
        tachi_llm::ProviderAuthProbeClass::AuthFailed => (
            "auth_failed".to_string(),
            Some("auth probe: provider rejected this credential (HTTP 401/403)".to_string()),
        ),
        tachi_llm::ProviderAuthProbeClass::RateLimited => (
            "rate_limited".to_string(),
            Some("auth probe: provider throttled this credential (HTTP 429)".to_string()),
        ),
        tachi_llm::ProviderAuthProbeClass::ProviderExhausted => (
            "failed".to_string(),
            Some(
                "auth probe: provider reports this credential out of quota (HTTP 402)".to_string(),
            ),
        ),
        // Inconclusive: a request went out and came back saying nothing about
        // the credential. Reported as a failed probe, recorded as evidence that
        // changes no part of the health binding.
        tachi_llm::ProviderAuthProbeClass::Transient
        | tachi_llm::ProviderAuthProbeClass::RedirectRefused
        | tachi_llm::ProviderAuthProbeClass::MalformedResponse
        | tachi_llm::ProviderAuthProbeClass::UnexpectedStatus => (
            "failed".to_string(),
            Some(format!(
                "auth probe inconclusive: {}",
                serde_json::to_value(result.auth_class)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".to_string())
            )),
        ),
        tachi_llm::ProviderAuthProbeClass::CredentialUnavailable => (
            "unavailable".to_string(),
            Some("auth probe: no key material for this member".to_string()),
        ),
        tachi_llm::ProviderAuthProbeClass::MalformedConfiguration
        | tachi_llm::ProviderAuthProbeClass::UnsupportedNoDocumentedProbe => unsupported(),
    }
}

fn classify_provider_probe_error(err: String) -> (String, Option<String>) {
    let lower = err.to_ascii_lowercase();
    let status = if lower.contains("429") || lower.contains("rate limit") {
        "rate_limited"
    } else if is_auth_error(&err) {
        "auth_failed"
    } else if lower.contains("timed out") {
        "timeout"
    } else {
        "failed"
    };
    (status.to_string(), Some(err))
}

#[cfg(test)]
mod persistence_phase_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn persistence_join_timeout_waits_for_terminal_completion_before_emitting_receipt() {
        let timeout = Duration::from_secs(7);
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let receipt_task = tokio::spawn(provider_health_persist_receipt(
            async move {
                release_rx
                    .await
                    .expect("test writer completion sender remains alive")
            },
            timeout,
        ));
        tokio::task::yield_now().await;
        tokio::time::advance(timeout).await;
        tokio::task::yield_now().await;
        assert!(
            !receipt_task.is_finished(),
            "a join timeout must not emit before the actual writer reports terminal completion"
        );
        release_tx
            .send(Err(
                "controlled writer completion after deadline".to_string()
            ))
            .expect("release timed-out writer");
        let timeout_receipt = receipt_task.await.expect("receipt task should join");
        assert_eq!(timeout_receipt.name, PROVIDER_HEALTH_PERSIST_PHASE);
        assert_eq!(timeout_receipt.status, "timeout");
        let timeout_json =
            serde_json::to_value(&timeout_receipt).expect("serialize timeout receipt");
        assert_eq!(timeout_json["name"], PROVIDER_HEALTH_PERSIST_PHASE);
        assert_eq!(timeout_json["status"], "timeout");
        let timeout_message = timeout_receipt.message.as_deref().expect("timeout cause");
        assert!(timeout_message.contains("phase=provider_health_persist"));
        assert!(timeout_message.contains("elapsed_ms=7000"));
        assert!(timeout_message.contains("timeout_ms=7000"));
        assert!(timeout_message.contains("cause=provider_health_persist_join_timeout"));
        assert!(timeout_message.contains("writer_joined=true"));
        assert!(
            timeout_message.contains("terminal_cause=controlled writer completion after deadline")
        );

        let failed_receipt = provider_health_persist_receipt(
            std::future::ready(Err("controlled writer failure".to_string())),
            timeout,
        )
        .await;
        assert_eq!(failed_receipt.name, PROVIDER_HEALTH_PERSIST_PHASE);
        assert_eq!(failed_receipt.status, "failed");
        assert!(failed_receipt
            .message
            .as_deref()
            .is_some_and(|message| message.contains("cause=controlled writer failure")));
    }

    #[test]
    fn persistence_sqlite_deadline_is_a_terminal_timeout_not_a_generic_failure() {
        let receipt = provider_health_persist_failure_receipt(
            tokio::time::Instant::now(),
            format!(
                "persist failed: {}: database is locked",
                tachi_llm::PROVIDER_HEALTH_PERSIST_SQLITE_DEADLINE_CAUSE
            ),
        );
        assert_eq!(receipt.status, "timeout");
        assert!(receipt
            .message
            .as_deref()
            .is_some_and(|message| message
                .contains(tachi_llm::PROVIDER_HEALTH_PERSIST_SQLITE_DEADLINE_CAUSE)));
    }

    #[test]
    fn persistence_cancellation_is_a_terminal_receipt_not_a_generic_failure() {
        let receipt = provider_health_persist_failure_receipt(
            tokio::time::Instant::now(),
            format!(
                "persist cancelled: {}",
                tachi_llm::PROVIDER_HEALTH_PERSIST_CANCELLED_CAUSE
            ),
        );
        assert_eq!(receipt.status, "cancelled");
        assert!(receipt.message.as_deref().is_some_and(
            |message| message.contains(tachi_llm::PROVIDER_HEALTH_PERSIST_CANCELLED_CAUSE)
        ));
    }
}
