//! Shared bounded command execution with an explicit partial-failure boundary.

use super::{build_gh_command, sanitize_output, Duration, MemoryServer, Value};

/// Parsed stdout is private observation data, never a successful read receipt.
/// It is not serializable or printable; the adapter may extract independently
/// validated restrictions and must discard the rest. Keeping it separate from
/// the diagnostic avoids losing visibility when textual redaction alters JSON.
pub(in crate::gh_ops) struct GhJsonReadFailure {
    reason: String,
    observed_json: Option<Value>,
}

impl std::fmt::Debug for GhJsonReadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GhJsonReadFailure { details: [REDACTED] }")
    }
}

impl GhJsonReadFailure {
    fn unobserved(reason: String) -> Self {
        Self {
            reason,
            observed_json: None,
        }
    }

    pub(in crate::gh_ops) fn observed_json(&self) -> Option<&Value> {
        self.observed_json.as_ref()
    }

    pub(super) fn into_reason(self) -> String {
        self.reason
    }
}

/// Keep command preparation and process wait under the existing timeout and
/// reuse the same credential/environment hardening and kill-on-drop policy.
/// Synchronous JSON decoding follows the wait; this is not a claim of CPU-time
/// preemption or complete descendant cleanup. A nonzero exit remains Err even
/// when stdout is a well-formed, success-shaped JSON document.
pub(in crate::gh_ops) async fn run_gh_json_observed_bounded(
    server: &MemoryServer,
    args: Vec<String>,
    timeout: Duration,
    context: &str,
) -> Result<Value, GhJsonReadFailure> {
    let server = server.clone();
    let timed = tokio::time::timeout(timeout, async {
        let (cmd, token) = tokio::task::spawn_blocking(move || build_gh_command(&server))
            .await
            .map_err(|error| format!("prepare `gh` command task failed: {error}"))??;
        let mut cmd = tokio::process::Command::from(cmd);
        cmd.args(args).kill_on_drop(true);
        let output = cmd
            .output()
            .await
            .map_err(|error| format!("failed to execute `gh`: {error}"))?;
        Ok::<(std::process::Output, String), String>((output, token))
    })
    .await
    .map_err(|_| GhJsonReadFailure::unobserved(format!("{context} timed out after {timeout:?}")))?;
    let (output, token) = timed.map_err(GhJsonReadFailure::unobserved)?;
    decode_output(output, &token, context)
}

fn decode_output(
    output: std::process::Output,
    token: &str,
    context: &str,
) -> Result<Value, GhJsonReadFailure> {
    let stderr = sanitize_output(&String::from_utf8_lossy(&output.stderr), token);
    if !output.status.success() {
        return Err(GhJsonReadFailure {
            reason: format!(
                "{context} failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                stderr.chars().take(500).collect::<String>()
            ),
            // Parse the complete original byte stream, not a redacted textual
            // diagnostic, not stderr and not a guessed substring. This payload
            // remains internal and must never be logged or serialized.
            observed_json: serde_json::from_slice(&output.stdout).ok(),
        });
    }
    let stdout = sanitize_output(&String::from_utf8_lossy(&output.stdout), token);
    serde_json::from_str(&stdout)
        .map_err(|error| GhJsonReadFailure::unobserved(format!("parse {context} JSON: {error}")))
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
    fn nonzero_complete_json_is_an_error_with_unprinted_observation() {
        let payload = serde_json::json!({
            "data": {"repository": {"visibility": "PRIVATE"}},
            "errors": [{"message": "private-output-sentinel"}]
        });
        let failure = decode_output(
            output(7, payload.to_string().as_bytes(), b"credential-sentinel"),
            "credential-sentinel",
            "fixture",
        )
        .expect_err("well-formed data must not erase a failed exit");
        assert_eq!(failure.observed_json(), Some(&payload));
        let debug = format!("{failure:?}");
        assert!(!debug.contains("private-output-sentinel"));
        assert!(!debug.contains("credential-sentinel"));
        assert!(!debug.contains("PRIVATE"));
        let reason = failure.into_reason();
        assert!(reason.contains("exit 7"));
        assert!(reason.contains("[REDACTED]"));
        assert!(!reason.contains("credential-sentinel"));
        assert!(!reason.contains("private-output-sentinel"));
    }

    #[test]
    fn malformed_truncated_or_stderr_only_json_does_not_fabricate_observation() {
        for stdout in [
            &b""[..],
            &b"PRIVATE"[..],
            &b"{\"data\": {\"repository\": {\"visibility\": \"PRIVATE\"}}"[..],
            &b"{\"data\": null}\xff"[..],
            &b"{} {}"[..],
        ] {
            let failure = decode_output(
                output(
                    1,
                    stdout,
                    br#"{"data":{"repository":{"visibility":"PRIVATE"}}}"#,
                ),
                "",
                "fixture",
            )
            .unwrap_err();
            assert!(failure.observed_json().is_none());
        }
    }

    #[test]
    fn diagnostic_redaction_cannot_destroy_observed_json_structure() {
        let stdout = b"{\"errors\":[{\"message\":\"Bearer ghp_fixture\"}],\"data\":{\"repository\":{\"visibility\":\"PRIVATE\"}}}\n";
        assert!(serde_json::from_str::<Value>(&sanitize_output(
            &String::from_utf8_lossy(stdout),
            ""
        ))
        .is_err());
        let failure = decode_output(output(1, stdout, b""), "", "fixture").unwrap_err();
        assert_eq!(
            failure
                .observed_json()
                .and_then(|value| value.pointer("/data/repository/visibility"))
                .and_then(Value::as_str),
            Some("PRIVATE")
        );
    }

    #[test]
    fn strict_success_parsing_and_unobserved_failures_remain_distinct() {
        assert_eq!(
            decode_output(output(0, br#"{"number":42}"#, b""), "", "fixture").unwrap(),
            serde_json::json!({"number": 42})
        );
        assert!(decode_output(output(0, b"not JSON", b""), "", "fixture")
            .unwrap_err()
            .observed_json()
            .is_none());
        assert!(GhJsonReadFailure::unobserved("timeout".to_string())
            .observed_json()
            .is_none());
    }
}
