use super::types::{CredentialProfile, CredentialProfileDocument};
use std::path::{Path, PathBuf};

pub fn default_credentials_dir() -> PathBuf {
    PathBuf::from(".tachi").join("credentials")
}

pub fn load_credential_profile_from_path(
    path: &Path,
    profile_name: &str,
) -> Result<CredentialProfile, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read credential profile config '{}': {e}", path.display()))?;
    let doc: CredentialProfileDocument = serde_json::from_str(&raw)
        .map_err(|e| format!("parse credential profile config '{}': {e}", path.display()))?;
    doc.credential_profiles
        .get(profile_name)
        .cloned()
        .ok_or_else(|| {
            format!(
                "Credential profile '{profile_name}' not found in {}",
                path.display()
            )
        })
}

pub fn find_credential_profile(
    credentials_dir: &Path,
    profile_name: &str,
) -> Result<(PathBuf, CredentialProfile), String> {
    let entries = std::fs::read_dir(credentials_dir).map_err(|e| {
        format!(
            "read credential profile directory '{}': {e}",
            credentials_dir.display()
        )
    })?;
    let mut skipped_invalid_configs = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| format!("read credential profile directory entry: {e}"))?
            .path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) => {
                skipped_invalid_configs.push(format!("{} ({err})", path.display()));
                continue;
            }
        };
        let doc: CredentialProfileDocument = match serde_json::from_str(&raw) {
            Ok(doc) => doc,
            Err(err) => {
                skipped_invalid_configs.push(format!("{} ({err})", path.display()));
                continue;
            }
        };
        if let Some(profile) = doc.credential_profiles.get(profile_name).cloned() {
            return Ok((path, profile));
        }
    }
    let mut err = format!(
        "Credential profile '{profile_name}' not found under {}",
        credentials_dir.display()
    );
    if !skipped_invalid_configs.is_empty() {
        err.push_str(&format!(
            "; skipped invalid configs: {}",
            skipped_invalid_configs.join(", ")
        ));
    }
    Err(err)
}
