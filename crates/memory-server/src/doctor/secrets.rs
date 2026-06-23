use std::fs;
use std::path::Path;

use super::DoctorWarning;

const GENERATED_ENV_REL_PATH: &str = ".tachi/env.generated";
const PROJECT_SECRET_SCAN_PATHS: &[&str] = &[".env", GENERATED_ENV_REL_PATH];
const MAX_SECRET_SCAN_BYTES: u64 = 256 * 1024;

pub fn project_secret_file_warnings(git_root: Option<&Path>) -> Vec<DoctorWarning> {
    let Some(git_root) = git_root else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    if git_index_tracks(git_root, GENERATED_ENV_REL_PATH) {
        let path = git_root.join(GENERATED_ENV_REL_PATH);
        warnings.push(DoctorWarning {
            code: "tracked_generated_env".to_string(),
            path: path.display().to_string(),
            message: format!(
                "{} is tracked by git and may contain plaintext secrets; remove it from the index",
                path.display()
            ),
            remediation: format!(
                "run `git rm --cached -- {GENERATED_ENV_REL_PATH}` and keep {GENERATED_ENV_REL_PATH} ignored"
            ),
        });
    }

    for rel_path in PROJECT_SECRET_SCAN_PATHS {
        let path = git_root.join(rel_path);
        warnings.extend(plaintext_provider_secret_warnings(&path, rel_path));
    }
    warnings
}

fn git_index_tracks(git_root: &Path, rel_path: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["ls-files", "--error-unmatch", "--", rel_path])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn plaintext_provider_secret_warnings(path: &Path, rel_path: &str) -> Vec<DoctorWarning> {
    let Ok(metadata) = fs::metadata(path) else {
        return Vec::new();
    };
    if !metadata.is_file() || metadata.len() > MAX_SECRET_SCAN_BYTES {
        return Vec::new();
    }
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };

    let provider_keys = crate::provider_config::provider_env_keys();
    contents
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let (name, value) = parse_env_assignment(line)?;
            if !is_provider_secret_name(name, &provider_keys)
                || !looks_like_plaintext_provider_secret(value)
            {
                return None;
            }
            let line_no = idx + 1;
            Some(DoctorWarning {
                code: "plaintext_provider_secret".to_string(),
                path: path.display().to_string(),
                message: format!(
                    "{}:{line_no} contains a plaintext provider secret for {name}; move it into Vault",
                    path.display()
                ),
                remediation: format!("replace {name}=... in {rel_path} with {name}=vault:{name}"),
            })
        })
        .collect()
}

fn parse_env_assignment(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("export ").unwrap_or(line).trim();
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let value = strip_env_value_comment(value.trim());
    Some((name, strip_matching_quotes(value.trim())))
}

fn strip_env_value_comment(value: &str) -> &str {
    match value.find(" #") {
        Some(idx) => value[..idx].trim_end(),
        None => value,
    }
}

fn strip_matching_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
        {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn is_provider_secret_name(name: &str, provider_keys: &std::collections::HashSet<String>) -> bool {
    if provider_keys.contains(name) {
        return true;
    }
    crate::provider_config::parse_rotation_member_name(name)
        .is_some_and(|(prefix, _)| provider_keys.contains(prefix))
}

fn looks_like_plaintext_provider_secret(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && !crate::provider_config::is_vault_alias(value)
        && value.len() >= 16
        && value.chars().all(|c| c.is_ascii_graphic())
}
