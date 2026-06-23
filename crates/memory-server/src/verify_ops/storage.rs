use super::*;

pub(super) fn now() -> String {
    Utc::now().to_rfc3339()
}

fn valid_status(status: &str) -> bool {
    matches!(
        status,
        "pending" | "running" | "passed" | "failed" | "skipped" | "stale"
    )
}

pub(super) fn normalize_status(status: Option<&str>, default: &str) -> Result<String, String> {
    let value = status.unwrap_or(default).trim().to_ascii_lowercase();
    if valid_status(&value) {
        Ok(value)
    } else {
        Err(format!(
            "invalid verification status '{value}' (allowed: pending, running, passed, failed, skipped, stale)"
        ))
    }
}

fn slugify_check_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(64));
    let mut last_dash = false;
    for c in raw.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(64).collect();
    if trimmed.is_empty() {
        "check".to_string()
    } else {
        trimmed
    }
}

pub(super) fn check_id_for(params: &TachiVerifyParams, command: Option<&str>) -> String {
    params
        .check_id
        .as_deref()
        .or(params.kind.as_deref())
        .or(command)
        .map(slugify_check_id)
        .unwrap_or_else(|| "check".to_string())
}

pub(super) fn ledger_path_for_flow(flow_id: &str) -> Result<PathBuf, String> {
    Ok(run_dir_for_flow_id(flow_id)?.join(LEDGER_FILE))
}

pub(super) fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| format!("parse {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

pub(super) fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let raw =
        serde_json::to_string_pretty(value).map_err(|e| format!("serialize verification: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)
            .map_err(|e| format!("create {}: {e}", tmp.display()))?;
        file.write_all(raw.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename verification tmp: {e}"))?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("sync verification directory {}: {e}", parent.display()))?;
    }
    Ok(())
}

pub(super) fn empty_ledger(flow_id: &str) -> Value {
    json!({
        "flow_id": flow_id,
        "updated_at": now(),
        "overall": "pending",
        "items": [],
    })
}
