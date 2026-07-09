use super::*;

pub(crate) async fn handle_vault_record_key_result(
    server: &MemoryServer,
    params: VaultRecordKeyResultParams,
) -> Result<String, String> {
    let logical_name = params.logical_name.trim().to_string();
    let key_id = params.key_id.trim().to_string();
    if logical_name.is_empty() || key_id.is_empty() {
        return Err("logical_name and key_id are required".to_string());
    }
    let llm = Arc::clone(&server.llm);
    let record_logical_name = logical_name.clone();
    let record_key_id = key_id.clone();
    let outcome = params.outcome.clone();
    let reason = params.reason.clone();
    let status_code = params.status_code;
    let retry_after_secs = params.retry_after_secs;
    let health = tokio::task::spawn_blocking(move || {
        llm.record_provider_key_result_blocking(
            &record_logical_name,
            &record_key_id,
            status_code,
            outcome.as_deref(),
            retry_after_secs,
            reason.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("record provider key result task failed: {e}"))?;
    let skipped_by_lease = health.disabled
        || health.auth_failed
        || matches!(
            health.status.as_str(),
            "exhausted" | "rate_limited" | "cooldown"
        );
    let body = json!({
        "recorded": true,
        "logical_name": logical_name,
        "key_id": key_id,
        "status_code": params.status_code,
        "outcome": params.outcome,
        "skipped_by_lease": skipped_by_lease,
        "health": {
            "status": health.status,
            "cooldown_until": health.cooldown_until,
            "auth_failed": health.auth_failed,
            "disabled": health.disabled,
            "error_count": health.error_count,
        },
    });
    let result = serde_json::to_string(&body).map_err(|e| format!("serialize: {e}"));
    let audit_result = record_vault_audit(
        server,
        "vault_record_key_result",
        Some(&params.logical_name),
        result.is_ok(),
        params.reason.as_deref(),
    );
    result_with_vault_audit_warning(result, audit_result)
}
