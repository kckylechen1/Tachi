pub(crate) fn normalize_supported_values<'a>(
    raw_values: &[String],
    supported: &'a [&'a str],
    value_label: &str,
) -> Result<Vec<&'a str>, String> {
    if raw_values.is_empty() {
        return Ok(supported.to_vec());
    }

    let mut out = Vec::new();
    for raw in raw_values {
        let normalized = raw.trim().to_ascii_lowercase();
        let Some(supported_value) = supported
            .iter()
            .copied()
            .find(|candidate| *candidate == normalized)
        else {
            return Err(format!(
                "unsupported {value_label} '{raw}'. Supported: {}",
                supported.join(", ")
            ));
        };
        if !out.contains(&supported_value) {
            out.push(supported_value);
        }
    }
    Ok(out)
}

/// Trusted command list for caller-supplied `agent=custom` dispatch.
pub(crate) fn is_trusted_command(cmd: &str) -> bool {
    tachi_dispatch::is_trusted_dispatch_command(cmd)
}
