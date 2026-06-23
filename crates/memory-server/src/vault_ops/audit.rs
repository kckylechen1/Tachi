use crate::server_state::MemoryServer;
use chrono::Utc;
use serde_json::json;

pub(super) fn record_vault_audit(
    server: &MemoryServer,
    operation: &str,
    secret_name: Option<&str>,
    success: bool,
    detail: Option<&str>,
) -> Result<(), String> {
    let timestamp = Utc::now().to_rfc3339();
    server
        .with_global_store(|store| {
            store
                .vault_insert_audit(&timestamp, operation, secret_name, success, detail)
                .map_err(|e| e.to_string())
        })
        .map_err(|err| {
            tracing::warn!("failed to record vault audit for {operation}: {err}");
            format!("failed to record vault audit for {operation}: {err}")
        })
}

fn attach_vault_audit_warning(body: String, warning: String) -> Result<String, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("serialize vault audit warning: {e}"))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "vault_audit_warning".to_string(),
            json!(format!(
                "Vault operation succeeded, but its audit record was not persisted: {warning}"
            )),
        );
    }
    serde_json::to_string(&value).map_err(|e| format!("serialize: {e}"))
}

pub(super) fn result_with_vault_audit_warning(
    result: Result<String, String>,
    audit_result: Result<(), String>,
) -> Result<String, String> {
    match (result, audit_result) {
        (Ok(body), Err(warning)) => attach_vault_audit_warning(body, warning),
        (result, _) => result,
    }
}
