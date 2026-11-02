use rmcp::schemars::{self, Schema, SchemaGenerator};
use serde::de;
use serde::Deserialize;

/// Lenient value-level coercion: the single source of the Null/Number/
/// String-empty/String-parse mapping shared by strict param deserialization
/// ([`opt_u64_from_string_or_number`]) and by callers (e.g. the RPC
/// transport layer, see #970) that only need a *hint* and must never
/// hard-error on a malformed value.
///
/// Mapping: `Null` → `None`; `Number` → `Some(n)` when representable as
/// `u64`, else `None`; `String("")` → `None`; `String(s)` → `Some(parsed)`
/// when `s` parses as `u64`, else `None`; every other JSON type (bool,
/// array, object) → `None`.
///
/// Unlike the deserializer, this never errors — non-numeric strings and
/// out-of-range numbers collapse to `None` rather than failing parsing.
/// That's why it isn't used directly as a `deserialize_with` for strictly
/// validated params (those still want a hard error on garbage input so the
/// caller finds out their request was malformed); it's meant for lenient
/// hint-reading call sites that would rather fail safe to a default than
/// reject a whole call over an un-parseable auxiliary field.
pub fn opt_u64_from_value(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                s.parse::<u64>().ok()
            }
        }
        _ => None,
    }
}

pub fn opt_u64_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match &value {
        serde_json::Value::Null | serde_json::Value::Number(_) | serde_json::Value::String(_) => {
            // Delegate the Null/Number/String-empty/String-parse mapping to
            // the shared lenient helper so the two cannot drift. The
            // lenient helper collapses "garbage" (non-numeric string,
            // non-representable number) to `None`, which this strict
            // deserializer must instead reject — re-derive those error
            // cases here rather than silently accepting them as `None`.
            if let Some(secs) = opt_u64_from_value(&value) {
                return Ok(Some(secs));
            }
            match &value {
                serde_json::Value::Null => Ok(None),
                serde_json::Value::String(s) if s.is_empty() => Ok(None),
                serde_json::Value::Number(_) => Err(de::Error::custom("expected unsigned integer")),
                serde_json::Value::String(s) => match s.parse::<u64>() {
                    Err(e) => Err(de::Error::custom(format!("invalid number: {e}"))),
                    Ok(_) => unreachable!("parse succeeded but helper returned None"),
                },
                _ => unreachable!(),
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
    let parsed: Result<Option<f64>, D::Error> = match value {
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
    };
    let parsed = parsed?;
    if parsed.is_some_and(|number| !number.is_finite()) {
        Err(de::Error::custom("expected finite number"))
    } else {
        Ok(parsed)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    struct FloatParam {
        #[serde(default, deserialize_with = "opt_f64_from_string_or_number")]
        value: Option<f64>,
    }

    #[test]
    fn optional_float_rejects_nonfinite_strings() {
        for value in [
            "NaN",
            "nan",
            "+NaN",
            "-NaN",
            "inf",
            "+inf",
            "-inf",
            "Infinity",
            "-Infinity",
            "1e999",
            "-1e999",
        ] {
            let error = serde_json::from_value::<FloatParam>(json!({ "value": value }))
                .expect_err("nonfinite parameter must fail deserialization");
            assert!(
                error.to_string().contains("finite number"),
                "{value:?}: {error}"
            );
        }
    }

    #[test]
    fn optional_float_preserves_finite_numbers_strings_and_absence() {
        for value in [json!({}), json!({ "value": null }), json!({ "value": "" })] {
            assert!(serde_json::from_value::<FloatParam>(value)
                .unwrap()
                .value
                .is_none());
        }

        for (value, expected) in [
            (json!(0), 0.0),
            (json!("0"), 0.0),
            (json!("-0"), -0.0),
            (json!(12.5), 12.5),
            (json!("12.5"), 12.5),
            (json!("-2.5e-3"), -0.0025),
            (json!("1.7976931348623157e308"), f64::MAX),
        ] {
            let parsed = serde_json::from_value::<FloatParam>(json!({ "value": value }))
                .unwrap()
                .value
                .unwrap();
            assert_eq!(parsed.to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn optional_float_rejects_other_json_types_and_invalid_strings() {
        for value in [
            json!(true),
            json!([]),
            json!({}),
            json!("no number"),
            json!(" "),
        ] {
            assert!(serde_json::from_value::<FloatParam>(json!({ "value": value })).is_err());
        }
    }
}
