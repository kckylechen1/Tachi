use rmcp::schemars::{self, Schema, SchemaGenerator};
use serde::de;
use serde::Deserialize;

pub fn opt_u64_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| de::Error::custom("expected unsigned integer")),
        serde_json::Value::String(s) => {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<u64>()
                    .map(Some)
                    .map_err(|e| de::Error::custom(format!("invalid number: {e}")))
            }
        }
        _ => Err(de::Error::custom("expected number or string")),
    }
}

pub fn opt_u32_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => n
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| de::Error::custom("expected unsigned integer")),
        serde_json::Value::String(s) => {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<u32>()
                    .map(Some)
                    .map_err(|e| de::Error::custom(format!("invalid number: {e}")))
            }
        }
        _ => Err(de::Error::custom("expected number or string")),
    }
}

pub fn opt_i64_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| de::Error::custom("expected integer")),
        serde_json::Value::String(s) => {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<i64>()
                    .map(Some)
                    .map_err(|e| de::Error::custom(format!("invalid number: {e}")))
            }
        }
        _ => Err(de::Error::custom("expected number or string")),
    }
}

pub fn opt_f64_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => n
            .as_f64()
            .map(Some)
            .ok_or_else(|| de::Error::custom("expected number")),
        serde_json::Value::String(s) => {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<f64>()
                    .map(Some)
                    .map_err(|e| de::Error::custom(format!("invalid number: {e}")))
            }
        }
        _ => Err(de::Error::custom("expected number or string")),
    }
}

// ─── Schema helpers: advertise string-or-number acceptance ───────────────────
//
// The deserialize_with helpers above accept both JSON numbers and numeric
// strings, but schemars derives the schema from the field type (`Option<u64>`,
// `Option<f64>`, …) and therefore advertises only `integer`/`number`. MCP
// clients that send numeric strings — which the server happily accepts — get
// rejected by schema validation. These helpers emit the matching union schema
// so the advertised JSON Schema and runtime deserializer agree. They include
// `null` because every field using them is `Option<T>`.

pub(crate) fn opt_integer_from_string_or_number_schema(_generator: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "anyOf": [
            { "type": "integer" },
            { "type": "string" },
            { "type": "null" }
        ]
    })
}

pub(crate) fn opt_number_from_string_or_number_schema(_generator: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "anyOf": [
            { "type": "number" },
            { "type": "string" },
            { "type": "null" }
        ]
    })
}
