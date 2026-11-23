use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

const MAX_STAFF_CONFIG_BYTES: usize = 64 * 1024;
const DEFAULT_OPENCODE_PROVIDER: &str = "openai";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpencodeStaffConfig {
    #[serde(default)]
    pub providers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SeatModelConfig {
    #[serde(default)]
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaffConfig {
    #[serde(default)]
    pub opencode: OpencodeStaffConfig,
    #[serde(default)]
    pub seats: BTreeMap<String, SeatModelConfig>,
}

impl StaffConfig {
    pub fn admitted_opencode_providers(&self) -> Vec<String> {
        match &self.opencode.providers {
            Some(providers) => providers.clone(),
            None => vec![DEFAULT_OPENCODE_PROVIDER.to_string()],
        }
    }

    pub fn seat_default_model(&self, seat: &str) -> Option<&str> {
        self.seats
            .get(seat)
            .and_then(|seat_cfg| seat_cfg.default_model.as_deref())
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }

    pub fn validate_opencode_model(&self, raw: &str) -> Result<String, String> {
        let model = raw.trim();
        if model.is_empty()
            || model
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(
                "OpenCode model must be a non-empty `provider/model` string without whitespace"
                    .to_string(),
            );
        }
        let Some((provider, model_id)) = model.split_once('/') else {
            return Err(format!(
                "OpenCode model '{model}' must include a `provider/model` prefix"
            ));
        };
        if provider.is_empty() || model_id.is_empty() {
            return Err(format!(
                "OpenCode model '{model}' must include non-empty provider and model identifiers"
            ));
        }
        let allowed = self.admitted_opencode_providers();
        if !allowed.iter().any(|entry| entry == provider) {
            let allowlist = if allowed.is_empty() {
                "<empty>".to_string()
            } else {
                allowed.join(", ")
            };
            return Err(format!(
                "OpenCode provider '{provider}' is not permitted by staff.json opencode.providers allowlist ([{allowlist}])"
            ));
        }
        Ok(model.to_string())
    }
}

pub fn load_staff_config(tachi_home: &Path) -> Result<StaffConfig, String> {
    let path = tachi_home.join("staff.json");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StaffConfig::default());
        }
        Err(err) => {
            return Err(format!(
                "read staff config metadata {}: {err}",
                path.display()
            ))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "staff config {} must be a regular file, not a symlink or directory",
            path.display()
        ));
    }
    if metadata.len() > MAX_STAFF_CONFIG_BYTES as u64 {
        return Err(format!(
            "staff config {} exceeds {MAX_STAFF_CONFIG_BYTES} bytes",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "staff config {} must not be group- or world-writable",
                path.display()
            ));
        }
    }
    let raw = std::fs::read(&path)
        .map_err(|err| format!("read staff config {}: {err}", path.display()))?;
    if raw.len() > MAX_STAFF_CONFIG_BYTES {
        return Err(format!(
            "staff config {} exceeds {MAX_STAFF_CONFIG_BYTES} bytes",
            path.display()
        ));
    }
    let config: StaffConfig = serde_json::from_slice(&raw)
        .map_err(|err| format!("parse staff config {}: {err}", path.display()))?;
    validate_loaded_staff_config(&config)?;
    Ok(config)
}

fn validate_loaded_staff_config(config: &StaffConfig) -> Result<(), String> {
    if let Some(providers) = &config.opencode.providers {
        for provider in providers {
            if provider.is_empty()
                || provider.trim() != provider
                || provider.contains('/')
                || provider
                    .chars()
                    .any(|ch| ch.is_whitespace() || ch.is_control())
            {
                return Err(format!(
                    "invalid provider entry '{provider}' in staff.json opencode.providers"
                ));
            }
        }
    }
    for (seat, seat_cfg) in &config.seats {
        let Some(profile) = crate::resolve_dispatch_profile(seat) else {
            return Err(format!(
                "unknown seat '{seat}' in staff.json seats; only admitted OpenCode native profiles can be configured"
            ));
        };
        if profile.backend != "opencode" || profile.native_session.is_none() {
            return Err(format!(
                "seat '{seat}' in staff.json is not an OpenCode native profile"
            ));
        }
        if let Some(default_model) = seat_cfg.default_model.as_deref() {
            config.validate_opencode_model(default_model)?;
        }
    }
    Ok(())
}
