use super::types::CredentialMaterializer;

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

pub(super) fn render_config_content(
    materializer: &CredentialMaterializer,
    secret: &str,
) -> Result<String, String> {
    let Some(template) = materializer.template.as_ref() else {
        return Ok(secret.to_string());
    };
    let rendered = render_template_value(template, secret);
    serde_json::to_string(&rendered).map_err(|e| {
        format!(
            "serialize credential config template for target '{}': {e}",
            materializer.target
        )
    })
}
