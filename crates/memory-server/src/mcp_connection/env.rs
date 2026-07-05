use super::*;

pub(super) fn apply_sanitized_child_env(
    cmd: &mut tokio::process::Command,
    env_map: &HashMap<String, String>,
) {
    cmd.env_clear();
    for var in MCP_PRESERVED_ENV_VARS {
        if let Ok(val) = std::env::var(var) {
            cmd.env(var, val);
        }
    }
    for (k, v) in env_map {
        cmd.env(k, v);
    }
}

pub(super) fn parse_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

pub(super) fn apply_env_allowlist(
    env_map: HashMap<String, String>,
    allowlist: &[String],
) -> HashMap<String, String> {
    if allowlist.is_empty() {
        return env_map;
    }
    let allowed: std::collections::HashSet<&str> = allowlist.iter().map(String::as_str).collect();
    env_map
        .into_iter()
        .filter(|(k, _)| allowed.contains(k.as_str()))
        .collect()
}

pub(super) fn normalize_path(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

fn canonicalize_for_policy(path: &std::path::Path) -> std::path::PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

pub(super) fn path_within_roots(path: &std::path::Path, roots: &[String]) -> bool {
    if roots.is_empty() {
        return true;
    }
    let candidate = canonicalize_for_policy(path);
    roots.iter().any(|root| {
        let root_path = canonicalize_for_policy(&normalize_path(root));
        candidate.starts_with(&root_path)
    })
}

pub(super) fn resolve_env_map_with_secret_resolver<F>(
    def: &serde_json::Value,
    secret_resolver: &F,
) -> Result<HashMap<String, String>, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let mut result = HashMap::new();
    if let Some(obj) = def.get("env").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(val) = v.as_str() {
                let resolved = expand_placeholders_with_secret_resolver(val, secret_resolver)?;
                result.insert(k.clone(), resolved);
            } else {
                return Err(format!(
                    "Invalid env value type for key '{}': expected string",
                    k
                ));
            }
        }
    } else if def.get("env").is_some() {
        return Err("Invalid env field: expected object".to_string());
    }
    Ok(result)
}

#[cfg(test)]
pub(super) fn expand_env_placeholders(value: &str) -> Result<String, String> {
    expand_placeholders_with_secret_resolver(value, &|_| Ok(None))
}

pub(super) fn expand_placeholders_with_secret_resolver<F>(
    value: &str,
    secret_resolver: &F,
) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let mut output = String::new();
    let mut cursor = 0usize;

    while let Some(rel_start) = value[cursor..].find("${") {
        let start = cursor + rel_start;
        output.push_str(&value[cursor..start]);
        let rest = &value[start + 2..];
        let end_rel = rest
            .find('}')
            .ok_or_else(|| format!("Unclosed environment placeholder in '{value}'"))?;
        let end = start + 2 + end_rel;
        let spec = &value[start + 2..end];
        let resolved = if let Some(secret_spec) = spec.strip_prefix("vault:") {
            resolve_secret_fallback_chain(secret_spec, secret_resolver)?
        } else {
            resolve_env_fallback_chain(spec)?
        };
        output.push_str(&resolved);
        cursor = end + 1;
    }

    output.push_str(&value[cursor..]);
    Ok(output)
}

fn resolve_secret_fallback_chain<F>(spec: &str, secret_resolver: &F) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let candidates: Vec<&str> = spec
        .split('|')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .collect();
    if candidates.is_empty() {
        return Err("Empty vault placeholder".to_string());
    }

    let mut last_secret_error = None;
    for key in &candidates {
        validate_secret_key_name(key)?;
        match secret_resolver(key) {
            Ok(Some(value)) if !value.trim().is_empty() => return Ok(value.trim().to_string()),
            Ok(_) => {}
            Err(err) => last_secret_error = Some(err),
        }
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
    }

    let base = if candidates.len() == 1 {
        "Vault secret not available".to_string()
    } else {
        "No configured vault secret fallback is available".to_string()
    };
    if let Some(_err) = last_secret_error {
        Err(format!(
            "{base} (resolver reported an error; check server logs for details)"
        ))
    } else {
        Err(base)
    }
}

pub(super) fn resolve_env_fallback_chain(spec: &str) -> Result<String, String> {
    let candidates: Vec<&str> = spec
        .split('|')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .collect();
    if candidates.is_empty() {
        return Err("Empty environment placeholder".to_string());
    }

    for key in &candidates {
        validate_env_key_name(key)?;
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
    }

    if candidates.len() == 1 {
        Err(format!(
            "Environment variable '{}' not set (required by MCP server)",
            candidates[0]
        ))
    } else {
        Err(format!(
            "None of the environment variables [{}] are set (required by MCP server)",
            candidates.join(", ")
        ))
    }
}

fn validate_env_key_name(key: &str) -> Result<(), String> {
    validate_secret_key_name(key).map_err(|_| {
        format!(
            "Invalid environment variable name '{}': only letters, digits, and underscores are allowed",
            key
        )
    })?;
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err("Environment variable name cannot be empty".to_string());
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!(
            "Invalid environment variable name '{}': must start with a letter or underscore",
            key
        ));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(format!(
            "Invalid environment variable name '{}': only letters, digits, and underscores are allowed",
            key
        ));
    }
    Ok(())
}

fn validate_secret_key_name(key: &str) -> Result<(), String> {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err("Secret name cannot be empty".to_string());
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!(
            "Invalid secret name '{}': must start with a letter or underscore",
            key
        ));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(format!(
            "Invalid secret name '{}': only letters, digits, and underscores are allowed",
            key
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn resolve_header_map(
    def: &serde_json::Value,
) -> Result<HashMap<HeaderName, HeaderValue>, String> {
    resolve_header_map_with_secret_resolver(def, &|_| Ok(None))
}

pub(super) fn resolve_header_map_with_secret_resolver<F>(
    def: &serde_json::Value,
    secret_resolver: &F,
) -> Result<HashMap<HeaderName, HeaderValue>, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let mut headers = HashMap::new();
    if let Some(obj) = def.get("headers").and_then(|value| value.as_object()) {
        for (name, value) in obj {
            let raw_value = value.as_str().ok_or_else(|| {
                format!("Invalid header value type for '{name}': expected string")
            })?;
            let resolved = expand_placeholders_with_secret_resolver(raw_value, secret_resolver)?;
            insert_header(&mut headers, name, &resolved)?;
        }
    } else if def.get("headers").is_some() {
        return Err("Invalid headers field: expected object".to_string());
    }

    if let Some(auth) = def.get("auth") {
        for (name, value) in resolve_broker_auth_headers(auth, secret_resolver)? {
            insert_header(&mut headers, &name, &value)?;
        }
    }

    Ok(headers)
}

fn insert_header(
    headers: &mut HashMap<HeaderName, HeaderValue>,
    name: &str,
    value: &str,
) -> Result<(), String> {
    let header_name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|e| format!("Invalid header name '{name}': {e}"))?;
    let header_value = HeaderValue::from_str(value)
        .map_err(|e| format!("Invalid header value for '{name}': {e}"))?;
    headers.insert(header_name, header_value);
    Ok(())
}

fn resolve_secret_reference<F>(key_spec: &str, secret_resolver: &F) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    resolve_secret_fallback_chain(key_spec, secret_resolver)
}

fn resolve_broker_auth_headers<F>(
    auth: &serde_json::Value,
    secret_resolver: &F,
) -> Result<Vec<(String, String)>, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let obj = auth
        .as_object()
        .ok_or_else(|| "Invalid auth field: expected object".to_string())?;
    let auth_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "auth.type is required".to_string())?;

    match auth_type {
        "bearer" => {
            let key = obj
                .get("token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "auth.token is required for bearer auth".to_string())?;
            let token = resolve_secret_reference(key, secret_resolver)?;
            Ok(vec![(
                "Authorization".to_string(),
                format!("Bearer {token}"),
            )])
        }
        "basic" => {
            let user_key = obj
                .get("username")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "auth.username is required for basic auth".to_string())?;
            let username = resolve_secret_reference(user_key, secret_resolver)?;
            let password = obj
                .get("password")
                .and_then(|v| v.as_str())
                .map(|key| resolve_secret_reference(key, secret_resolver))
                .transpose()?
                .unwrap_or_default();
            let encoded = B64.encode(format!("{username}:{password}"));
            Ok(vec![(
                "Authorization".to_string(),
                format!("Basic {encoded}"),
            )])
        }
        "api-key" => {
            let key = obj
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "auth.key is required for api-key auth".to_string())?;
            let header = obj
                .get("header")
                .and_then(|v| v.as_str())
                .unwrap_or("Authorization");
            let prefix = obj.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
            let value = resolve_secret_reference(key, secret_resolver)?;
            let header_value = if prefix.is_empty() {
                value
            } else {
                format!("{prefix} {value}")
            };
            Ok(vec![(header.to_string(), header_value)])
        }
        "custom" => {
            let headers = obj
                .get("headers")
                .and_then(|v| v.as_object())
                .ok_or_else(|| "auth.headers is required for custom auth".to_string())?;
            let mut out = Vec::new();
            for (name, value) in headers {
                let template = value.as_str().ok_or_else(|| {
                    format!("Invalid custom auth header '{name}': expected string")
                })?;
                out.push((
                    name.clone(),
                    expand_custom_auth_template(template, secret_resolver)?,
                ));
            }
            Ok(out)
        }
        "passthrough" => Ok(Vec::new()),
        other => Err(format!(
            "auth.type '{}' is not supported (expected bearer, basic, api-key, custom, passthrough)",
            other
        )),
    }
}

fn expand_custom_auth_template<F>(template: &str, secret_resolver: &F) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let mut output = String::new();
    let mut cursor = 0usize;

    while let Some(rel_start) = template[cursor..].find("{{") {
        let start = cursor + rel_start;
        output.push_str(&template[cursor..start]);
        let rest = &template[start + 2..];
        let end_rel = rest
            .find("}}")
            .ok_or_else(|| format!("Unclosed credential placeholder in '{template}'"))?;
        let end = start + 2 + end_rel;
        let key = template[start + 2..end].trim();
        output.push_str(&resolve_secret_reference(key, secret_resolver)?);
        cursor = end + 2;
    }

    output.push_str(&template[cursor..]);
    Ok(output)
}
