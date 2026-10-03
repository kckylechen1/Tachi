//! Native DSH headless JSON contract, verified against @deepseek-ai/dsh
//! 0.2.0-rc.2. A `final` event alone is not a successful run: the runner also
//! writes it for failed turns. Keep child exit and semantic completion separate.

use serde_json::Value;

pub(super) fn completed_answer(output: &str) -> Result<String, String> {
    // The shared subprocess collector keeps stdout first and appends stderr.
    // Diagnostics are not part of DSH's newline-delimited stdout protocol.
    let stdout = output
        .split_once(super::subprocess::STDERR_SEPARATOR)
        .map_or(output, |(stdout, _)| stdout);
    let mut session_seen = false;
    let mut completed = false;
    let mut final_text = None;
    for (index, line) in stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        if final_text.is_some() {
            return Err("dsh emitted an event after final".to_string());
        }
        let event: Value = serde_json::from_str(line)
            .map_err(|_| format!("dsh stdout event {} is not valid JSON", index + 1))?;
        let event_type = event["type"]
            .as_str()
            .ok_or_else(|| "dsh stdout event has no type".to_string())?;
        if index == 0 && event_type != "session" {
            return Err("dsh stdout did not open with a session event".to_string());
        }
        match event_type {
            "session" => {
                if session_seen || !event["sessionId"].as_str().is_some_and(|id| !id.is_empty()) {
                    return Err("dsh stdout has an invalid or duplicate session event".to_string());
                }
                session_seen = true;
            }
            "status" if event["phase"] == "turn_start" => completed = false,
            "status" if event["phase"] == "turn_end" => {
                completed = event["reason"]["kind"] == "completed";
            }
            "final" => {
                if !completed {
                    return Err("dsh final has no completed terminal turn".to_string());
                }
                final_text = Some(
                    event["text"]
                        .as_str()
                        .ok_or_else(|| "dsh final has no text".to_string())?
                        .to_string(),
                );
            }
            "error" => return Err("dsh reported a fatal headless error".to_string()),
            "status" | "thinking" | "text" | "tool_call" | "tool_result" => {}
            _ => {
                return Err(format!(
                    "dsh emitted an unsupported event type '{event_type}'"
                ))
            }
        }
    }
    final_text.ok_or_else(|| "dsh stdout has no final event".to_string())
}

/// The runner preserves channels with the shared delimiter even for stderr
/// alone. Publication never infers a stream from its JSON content.
pub(super) fn publish_output(
    run_dir: &std::path::Path,
    output: &str,
    managed_ephemeral_credential_cleanup: bool,
) -> Result<(), String> {
    let (events, diagnostics) = output
        .split_once(super::subprocess::STDERR_SEPARATOR)
        .map_or((output, None), |(stdout, stderr)| (stdout, Some(stderr)));
    super::dispatch::persist_dispatch_result_artifact(
        &run_dir.join("dsh-events.jsonl"),
        events.as_bytes(),
        managed_ephemeral_credential_cleanup,
    )?;
    if let Some(stderr) = diagnostics {
        super::dispatch::persist_dispatch_result_artifact(
            &run_dir.join("dsh-stderr.log"),
            stderr.as_bytes(),
            managed_ephemeral_credential_cleanup,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str =
        "{\"type\":\"session\",\"sessionId\":\"session-1\",\"cwd\":\"/admitted\"}\n";
    const COMPLETED: &str = "{\"type\":\"status\",\"phase\":\"turn_end\",\"turn\":1,\"reason\":{\"kind\":\"completed\"}}\n";
    const FINAL: &str = "{\"type\":\"final\",\"text\":\"42\\nanswer\"}\n";

    #[test]
    fn dsh_completed_json_maps_only_final_answer_and_ignores_stderr() {
        let events = format!("{SESSION}{{\"type\":\"thinking\",\"text\":\"private reasoning\"}}\n{COMPLETED}{FINAL}\n\n--- stderr ---\ndiagnostics");
        assert_eq!(
            completed_answer(&events).expect("complete stream"),
            "42\nanswer"
        );
    }

    #[test]
    fn dsh_final_only_failed_and_incomplete_streams_never_map_to_success() {
        let failed =
            "{\"type\":\"status\",\"phase\":\"turn_end\",\"reason\":{\"kind\":\"error\"}}\n";
        for events in [
            FINAL.to_string(),
            format!("{SESSION}{FINAL}"),
            format!("{SESSION}{failed}{FINAL}"),
            format!("{SESSION}{COMPLETED}"),
            format!(
                "{SESSION}{COMPLETED}{{\"type\":\"status\",\"phase\":\"turn_start\"}}\n{FINAL}"
            ),
            format!("{SESSION}{COMPLETED}{FINAL}{FINAL}"),
            format!(
                "{SESSION}{COMPLETED}{{\"type\":\"error\",\"message\":\"boot failed\"}}\n{FINAL}"
            ),
            format!("{SESSION}{COMPLETED}not-json\n{FINAL}"),
        ] {
            assert!(
                completed_answer(&events).is_err(),
                "accepted invalid stream: {events}"
            );
        }
    }
}
