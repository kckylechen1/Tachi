use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// tachi#1173 item 7: bounded, ANSI-free tail of a dispatch's terminal
/// output for `wait`/`board` responses on a failed dispatch, so a caller can
/// autopsy a failure without separately reading files under `~/.tachi`.
///
/// Prefers the newest non-empty `output_tail` string found by scanning
/// `<run_dir>/progress.jsonl` in reverse (that field is written at more than
/// one lifecycle point by `dispatch_ops::dispatch::execution`, so this reads
/// by field presence rather than a specific `event` value), falling back to
/// `<run_dir>/result.md` when no such line/field exists. Returns `None` when
/// neither source is available.
const FAILURE_TAIL_MAX_BYTES: usize = 2048;
const FAILURE_TAIL_SOURCE_READ_MAX_BYTES: u64 = 16 * 1024;

pub(crate) fn read_failure_tail(run_dir: &Path) -> Option<String> {
    let raw = read_progress_output_tail(run_dir)
        .or_else(|| read_bounded_tail(&run_dir.join("result.md")))?;
    let stripped = strip_ansi_escapes(&raw);
    Some(tail_at_char_boundary(&stripped, FAILURE_TAIL_MAX_BYTES))
}

fn read_progress_output_tail(run_dir: &Path) -> Option<String> {
    let content = read_bounded_tail(&run_dir.join("progress.jsonl"))?;
    content.lines().rev().find_map(|line| {
        let event: serde_json::Value = serde_json::from_str(line).ok()?;
        event
            .get("output_tail")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn read_bounded_tail(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }

    let mut file = std::fs::File::open(path).ok()?;
    let start = metadata
        .len()
        .saturating_sub(FAILURE_TAIL_SOURCE_READ_MAX_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut raw = String::new();
    file.take(FAILURE_TAIL_SOURCE_READ_MAX_BYTES)
        .read_to_string(&mut raw)
        .ok()?;
    Some(raw)
}

/// Returns the last `max_bytes` bytes of `s`, backed off to the nearest char
/// boundary so a multi-byte UTF-8 sequence is never split.
fn tail_at_char_boundary(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Strips ANSI CSI (`ESC [ ... final-byte`) and the whole string-terminated
/// family sharing OSC's shape -- OSC (`ESC ]`), DCS (`ESC P`), SOS (`ESC X`),
/// PM (`ESC ^`), and APC (`ESC _`) -- each of which runs until a BEL or an
/// ST (`ESC \`) terminator (tachi#1173 board autopsy review: the original
/// implementation only recognized CSI/OSC, so a DCS/SOS/PM/APC sequence's
/// body -- everything up to its terminator -- leaked into the tail verbatim
/// instead of being swallowed). Any other lone ESC byte is dropped without
/// consuming following characters -- a best-effort handling for other/
/// malformed sequences, but every ESC (`\x1b`) byte itself is always
/// removed, which is the property the discriminator test checks.
fn strip_ansi_escapes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                chars.next();
                loop {
                    match chars.next() {
                        None => break,
                        Some('\u{7}') => break,
                        Some('\u{1b}') => {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        Some(_) => continue,
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_color_codes() {
        let input = "\u{1b}[31merror\u{1b}[0m: build failed";
        let stripped = strip_ansi_escapes(input);
        assert_eq!(stripped, "error: build failed");
        assert!(!stripped.contains('\u{1b}'));
    }

    #[test]
    fn strips_osc_title_sequence() {
        let input = "\u{1b}]0;window title\u{7}visible text";
        let stripped = strip_ansi_escapes(input);
        assert_eq!(stripped, "visible text");
        assert!(!stripped.contains('\u{1b}'));
    }

    /// tachi#1173 board autopsy review discriminator: DCS/SOS/PM/APC
    /// (`ESC P`/`ESC X`/`ESC ^`/`ESC _`), the rest of the string-terminated
    /// escape family alongside OSC, must be swallowed to their ST (`ESC \`)
    /// terminator just like OSC -- previously only CSI/OSC were recognized,
    /// so these leaked their body verbatim into the tail.
    #[test]
    fn strips_dcs_sos_pm_apc_sequences_to_st_terminator() {
        for (open, label) in [('P', "DCS"), ('X', "SOS"), ('^', "PM"), ('_', "APC")] {
            let input = format!("before\u{1b}{open}hidden payload\u{1b}\\after");
            let stripped = strip_ansi_escapes(&input);
            assert_eq!(
                stripped, "beforeafter",
                "{label} sequence body should be swallowed to its ST terminator, got: {stripped:?}"
            );
            assert!(
                !stripped.contains('\u{1b}'),
                "{label}: no raw ESC byte should remain, got: {stripped:?}"
            );
        }
    }

    /// Same family, terminated by BEL instead of ST (xterm accepts both).
    #[test]
    fn strips_dcs_sequence_terminated_by_bel() {
        let input = "before\u{1b}Phidden payload\u{7}after";
        let stripped = strip_ansi_escapes(input);
        assert_eq!(stripped, "beforeafter");
        assert!(!stripped.contains('\u{1b}'));
    }

    #[test]
    fn tail_caps_at_char_boundary_not_mid_char() {
        // 3 multi-byte chars repeated so max_bytes lands mid-character if we
        // sliced by raw byte index without adjusting.
        let s = "测".repeat(1200); // 3 bytes/char * 1200 = 3600 bytes
        let tail = tail_at_char_boundary(&s, 2048);
        assert!(tail.len() <= 2048);
        // Must still be valid UTF-8 / round-trippable.
        assert!(s.ends_with(&tail));
    }

    #[test]
    fn read_failure_tail_prefers_progress_jsonl_output_tail() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path();
        std::fs::write(
            run_dir.join("progress.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::json!({"event": "started", "dispatch_id": "x"}),
                serde_json::json!({
                    "event": "subprocess_finished",
                    "output_tail": "\u{1b}[31mFAILED\u{1b}[0m: exit 1",
                }),
            ),
        )
        .expect("write progress.jsonl");
        std::fs::write(run_dir.join("result.md"), "should not be used").expect("write result.md");

        let tail = read_failure_tail(run_dir).expect("failure tail");
        assert_eq!(tail, "FAILED: exit 1");
        assert!(!tail.contains('\u{1b}'));
    }

    #[test]
    fn read_failure_tail_falls_back_to_result_md() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path();
        std::fs::write(
            run_dir.join("result.md"),
            "# Verdict\nFAILED: no output_tail",
        )
        .expect("write result.md");

        let tail = read_failure_tail(run_dir).expect("failure tail");
        assert!(tail.contains("FAILED: no output_tail"));
    }

    #[test]
    fn read_failure_tail_reads_the_end_of_an_oversized_result_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path();
        std::fs::write(
            run_dir.join("result.md"),
            format!("{}\nFAILED: final bounded tail", "prefix ".repeat(4_000)),
        )
        .expect("write oversized result.md");

        let tail = read_failure_tail(run_dir).expect("failure tail");
        assert!(tail.contains("FAILED: final bounded tail"));
        assert!(tail.len() <= FAILURE_TAIL_MAX_BYTES);
    }

    #[test]
    fn read_failure_tail_none_when_nothing_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(read_failure_tail(tmp.path()).is_none());
    }
}
