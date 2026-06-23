use super::types::CredentialMaterializer;
use std::fs;
use std::path::Path;

fn render_template_value(template: &serde_json::Value, secret: &str) -> serde_json::Value {
    match template {
        serde_json::Value::String(text) => serde_json::Value::String(
            text.replace("{{secret}}", secret)
                .replace("{{value}}", secret),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| render_template_value(value, secret))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), render_template_value(value, secret)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn render_config_overlay_value(
    materializer: &CredentialMaterializer,
    secret: &str,
) -> Result<String, String> {
    let Some(template) = materializer.template.as_ref() else {
        return Ok(secret.to_string());
    };
    let rendered = render_template_value(template, secret);
    serde_json::to_string(&rendered).map_err(|e| {
        format!(
            "serialize config_overlay template for target '{}': {e}",
            materializer.target
        )
    })
}

fn render_template_json(materializer: &CredentialMaterializer, secret: &str) -> serde_json::Value {
    materializer
        .template
        .as_ref()
        .map(|template| render_template_value(template, secret))
        .unwrap_or_else(|| serde_json::Value::String(secret.to_string()))
}

fn merge_json_patch(base: &mut serde_json::Value, patch: serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(base), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                if value.is_null() {
                    base.remove(&key);
                } else if let Some(existing) = base.get_mut(&key) {
                    merge_json_patch(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, patch) => {
            *base = patch;
        }
    }
}

pub(super) fn render_config_patch_value(
    materializer: &CredentialMaterializer,
    secret: &str,
    target: &Path,
) -> Result<String, String> {
    let patch = render_template_json(materializer, secret);
    if !patch.is_object() {
        return Err(format!(
            "config_patch template for target '{}' must render to a JSON object",
            materializer.target
        ));
    }
    let mut base = if target.exists() {
        let raw = fs::read_to_string(target)
            .map_err(|e| format!("read config_patch target '{}': {e}", target.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("parse config_patch target '{}': {e}", target.display()))?
    } else {
        serde_json::json!({})
    };
    if !base.is_object() {
        return Err(format!(
            "config_patch target '{}' must contain a JSON object",
            target.display()
        ));
    }
    merge_json_patch(&mut base, patch);
    serde_json::to_string_pretty(&base)
        .map_err(|e| format!("serialize config_patch target '{}': {e}", target.display()))
}
