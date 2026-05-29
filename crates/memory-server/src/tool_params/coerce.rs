use serde::de;
use serde::Deserialize;

pub(crate) fn opt_u64_from_string_or_number<'de, D>(
    deserializer: D,
) -> Result<Option<u64>, D::Error>
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

pub(crate) fn opt_u32_from_string_or_number<'de, D>(
    deserializer: D,
) -> Result<Option<u32>, D::Error>
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
