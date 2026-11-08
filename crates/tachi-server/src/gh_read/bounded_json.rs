//! Bounded strict JSON read: a nonzero exit is always an error.

use super::{
    format_gh_auth_source_suffix, sanitize_output, Duration, GhCall, GhCredentialSource,
    MemoryServer, Value,
};

/// Runs on the shared bounded executor ([`GhCall`]): command preparation and
/// process wait share one deadline, the credential/environment hardening is
/// the same, and a timed-out child is killed and reaped. Synchronous JSON
/// decoding follows the wait; this is not a claim of CPU-time preemption or
/// complete descendant cleanup. A nonzero exit remains Err even when stdout
/// is a well-formed, success-shaped JSON document.
pub(super) async fn run_gh_json_bounded(
    server: &MemoryServer,
    args: Vec<String>,
    timeout: Duration,
    context: &str,
) -> Result<Value, String> {
    let mut call = GhCall::read();
    call.args(args).timeout(timeout).context(context);
    let output = call
        .output(server)
        .await
        .map_err(|error| error.to_string())?;
    let (output, token, source) = output.into_parts();
    decode_output_with_source(output, &token, source, context)
}

#[cfg(test)]
fn decode_output(
    output: std::process::Output,
    token: &str,
    context: &str,
) -> Result<Value, String> {
    decode_output_with_source(output, token, GhCredentialSource::GhNative, context)
}

fn decode_output_with_source(
    output: std::process::Output,
    token: &str,
    source: GhCredentialSource,
    context: &str,
) -> Result<Value, String> {
    let stderr = sanitize_output(&String::from_utf8_lossy(&output.stderr), token);
    if !output.status.success() {
        let truncated: String = stderr.chars().take(500).collect();
        let suffix = format_gh_auth_source_suffix(&truncated, source);
        return Err(format!(
            "{context} failed (exit {}): {truncated}{suffix}",
            output.status.code().unwrap_or(-1),
        ));
    }
    let stdout = sanitize_output(&String::from_utf8_lossy(&output.stdout), token);
    serde_json::from_str(&stdout).map_err(|error| format!("parse {context} JSON: {error}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn output(code: i32, stdout: &[u8], stderr: &[u8]) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn nonzero_complete_json_is_an_error_with_redacted_reason() {
        let payload = serde_json::json!({
            "data": {"repository": {"visibility": "PRIVATE"}},
            "errors": [{"message": "private-output-sentinel"}]
        });
        let reason = decode_output(
            output(7, payload.to_string().as_bytes(), b"credential-sentinel"),
            "credential-sentinel",
            "fixture",
        )
        .expect_err("well-formed data must not erase a failed exit");
        assert!(reason.contains("exit 7"));
        assert!(reason.contains("[REDACTED]"));
        assert!(!reason.contains("credential-sentinel"));
        assert!(!reason.contains("private-output-sentinel"));
    }

    #[test]
    fn strict_success_parsing() {
        assert_eq!(
            decode_output(output(0, br#"{"number":42}"#, b""), "", "fixture").unwrap(),
            serde_json::json!({"number": 42})
        );
        assert!(decode_output(output(0, b"not JSON", b""), "", "fixture").is_err());
    }
}
