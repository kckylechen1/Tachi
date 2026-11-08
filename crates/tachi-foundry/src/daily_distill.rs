use memcore::MemoryEntry;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Default batch size when `FOUNDRY_DISTILL_BATCH_SIZE` is unset.
pub const DEFAULT_GROUPS_PER_BATCH: usize = 6;
pub const DEFAULT_PROCESSED_SCAN_LIMIT: usize = 10_000;
pub const DEFAULT_CANDIDATE_SCAN_LIMIT: usize = 5_000;
pub const MAX_DISTILL_SCAN_LIMIT: usize = 50_000;
pub const MIN_BUCKET_SIZE: usize = 3;
/// UTF-8 byte budget for system and user input, including the recorded prompt wrapper.
pub const MAX_BATCH_PAYLOAD_BYTES: usize = 60_000;

/// System prompt for the batch distill mega-call.
/// Enforces structured synthesis using decision markers for durable memory reuse.
pub const DISTILL_DAILY_SYSTEM_PROMPT: &str = r#"You are Tachi's batch memory distiller. You will receive a list of memory groups; each group contains 3+ related memories from a single coherence bucket (shared topic or entity, scoped to a single path prefix).

For EACH group, write a concise, faithful synthesis that:
- Preserves the most important durable facts (decisions, identifiers, file paths, commands, error signatures).
- Drops chit-chat, redundant restatements, time-sensitive scratch notes, tool JSON blobs, and thinking traces.
- Uses neutral third-person prose. Do NOT invent facts not present in the inputs.
- Stays under ~400 words per group.
- Structures key decisions using these markers when applicable:
  - [核心] for the central conclusion or architectural decision
  - [结论] for a derived insight or final determination
  - [方案] for a chosen implementation approach or solution
  - [重构/优化] for a refactoring or performance improvement decision

Return ONLY a JSON array. No prose before or after. Each element MUST be:
{
  "group_id": "<the group_id you were given>",
  "summary": "<one-line ≤120 chars>",
  "text": "<the full distilled synthesis>",
  "keywords": ["<lower-case tag>", ...]
}

If a group cannot be coherently distilled, return an object with an empty "text" and a "skip_reason" field; that group will be skipped.
"#;

/// Same instructions as the batch prompt but for a single group.
pub const DISTILL_DAILY_SYSTEM_PROMPT_SINGLE: &str = r#"You are Tachi's memory distiller. Synthesize the provided related memories into a single concise, faithful summary under ~400 words. Drop chit-chat, keep durable facts (decisions, identifiers, file paths, commands, error signatures). Return ONLY the distilled prose, no JSON, no preamble."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistillBackend {
    ClaudeCli,
    RawApi,
}

impl DistillBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            DistillBackend::ClaudeCli => "claude_cli",
            DistillBackend::RawApi => "raw_api",
        }
    }
}

/// Per-batch outcome surfaced to the scheduler/log.
#[derive(Debug, Default, Serialize)]
pub struct DistillBatchReport {
    pub projects_scanned: usize,
    pub batches_dispatched: usize,
    pub groups_distilled: usize,
    pub groups_skipped: usize,
    pub fallback_used: usize,
    /// #1043 D3: rows collapsed by the pre-selection consolidate pass
    /// (byte-identical duplicates within a candidate bucket, merged into one
    /// survivor before distillation selects/sends the group). Zero when the
    /// pre-pass is disabled via `--no-consolidate` or finds no duplicates.
    pub consolidated: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CandidateGroup {
    pub group_id: String,
    pub path_prefix: String,
    pub coherence_key: String,
    pub entries: Vec<MemoryEntry>,
}

impl CandidateGroup {
    pub fn new(path_prefix: String, coherence_key: String, entries: Vec<MemoryEntry>) -> Self {
        let mut source_ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        source_ids.sort_unstable();
        source_ids.dedup();
        let identity = serde_json::to_vec(&(
            "daily-distill-request-group-v1",
            &path_prefix,
            &coherence_key,
            source_ids,
        ))
        .expect("serializing a daily distill group identity cannot fail");
        let group_id = format!(
            "group:{}",
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &identity)
        );
        Self {
            group_id,
            path_prefix,
            coherence_key,
            entries,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceManifestEntry {
    pub group_id: String,
    pub path_prefix: String,
    pub coherence_key: String,
    pub source_memory_ids: Vec<String>,
    pub written_memory_id: Option<String>,
    pub backend: &'static str,
    pub fallback_used: bool,
    pub skip_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GroupPayload {
    pub summary: String,
    pub text: String,
    pub keywords: Vec<String>,
    pub skip_reason: Option<String>,
}

/// Resolve distill backend from `FOUNDRY_DISTILL_BACKEND`.
/// Defaults to `raw_api` so daemon runs do not require Claude Code CLI.
pub fn resolve_distill_backend() -> DistillBackend {
    match std::env::var("FOUNDRY_DISTILL_BACKEND")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("claude_cli" | "claude-cli" | "cli") => DistillBackend::ClaudeCli,
        Some("raw_api" | "raw-api" | "rawapi" | "api") => DistillBackend::RawApi,
        None | Some(_) => DistillBackend::RawApi,
    }
}

/// Strip agent-internal noise from raw session text before passing to the LLM.
/// Removes thinking traces, tool JSON blobs, SYSTEM_MEMORY injections, and
/// image references that contaminate distillation quality.
pub fn scrub_agent_noise(text: &str) -> String {
    let noise_line_prefixes: &[&str] = &[
        "**Prioritizing",
        "**Refining",
        "**Analyzing",
        "I'm now focusing",
        "I'm now zeroing",
        "<SYSTEM-RETRIEVED-MEMORY",
        "<image name=",
    ];
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start();
        if (trimmed.starts_with('{') || trimmed.starts_with('['))
            && (trimmed.contains("\"SearchPath\"")
                || trimmed.contains("\"file_path\"")
                || trimmed.contains("\"todos\"")
                || trimmed.contains("\"prompt\""))
        {
            continue;
        }
        if noise_line_prefixes
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    let mut prev_blank = false;
    let mut result = String::with_capacity(out.len());
    for line in out.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank {
            continue;
        }
        prev_blank = is_blank;
        result.push_str(line);
        result.push('\n');
    }
    result
}

pub fn resolve_batch_size() -> usize {
    std::env::var("FOUNDRY_DISTILL_BATCH_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| (1..=20).contains(&n))
        .unwrap_or(DEFAULT_GROUPS_PER_BATCH)
}

fn resolve_scan_limit(env_key: &str, default: usize) -> usize {
    std::env::var(env_key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| (1..=MAX_DISTILL_SCAN_LIMIT).contains(&n))
        .unwrap_or(default)
}

pub fn resolve_processed_scan_limit() -> usize {
    resolve_scan_limit(
        "FOUNDRY_DISTILL_PROCESSED_SCAN_LIMIT",
        DEFAULT_PROCESSED_SCAN_LIMIT,
    )
}

pub fn resolve_candidate_scan_limit() -> usize {
    resolve_scan_limit(
        "FOUNDRY_DISTILL_CANDIDATE_SCAN_LIMIT",
        DEFAULT_CANDIDATE_SCAN_LIMIT,
    )
}

/// Partition whole source snapshots; never attribute omitted sources to a prompt.
/// A minimum-sized group that still exceeds the budget is retained so callers
/// report its refusal instead of silently dropping or marking its sources processed.
pub fn plan_daily_distill_batches(
    groups: Vec<CandidateGroup>,
    max_groups: usize,
) -> Vec<Vec<CandidateGroup>> {
    let mut pending: Vec<_> = groups.into_iter().rev().collect();
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    while let Some(group) = pending.pop() {
        // The consolidate prepass may have changed the exact source set.
        let mut group = CandidateGroup::new(group.path_prefix, group.coherence_key, group.entries);
        let fits = build_batch_user_payload(std::slice::from_ref(&group)).is_ok()
            && build_fallback_user_payload(&group).is_ok();
        if !fits && group.entries.len() >= MIN_BUCKET_SIZE * 2 {
            let right_entries = group.entries.split_off(group.entries.len() / 2);
            pending.push(CandidateGroup::new(
                group.path_prefix.clone(),
                group.coherence_key.clone(),
                right_entries,
            ));
            pending.push(group);
            continue;
        }
        batch.push(group);
        if batch.len() > 1
            && (batch.len() > max_groups.max(1) || build_batch_user_payload(&batch).is_err())
        {
            let next = batch.pop().expect("batch has more than one group");
            batches.push(std::mem::take(&mut batch));
            batch.push(next);
        }
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

pub fn build_batch_prompt(groups: &[CandidateGroup]) -> Result<String, String> {
    Ok(format!(
        "<system>\n{}\n</system>\n\n{}",
        DISTILL_DAILY_SYSTEM_PROMPT,
        build_batch_user_payload(groups)?
    ))
}

pub fn build_batch_user_payload(groups: &[CandidateGroup]) -> Result<String, String> {
    let mut payload = Vec::with_capacity(groups.len());
    let mut group_ids = HashSet::with_capacity(groups.len());
    for group in groups {
        if !group_ids.insert(&group.group_id) {
            return Err(format!(
                "duplicate distill request group_id: {}",
                group.group_id
            ));
        }
        let entries: Vec<Value> = group
            .entries
            .iter()
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "topic": entry.topic,
                    "path": entry.path,
                    "importance": entry.importance,
                    "summary": entry.summary,
                    "text": scrub_agent_noise(&entry.text).chars().take(800).collect::<String>(),
                    "keywords": entry.keywords,
                    "entities": entry.entities,
                })
            })
            .collect();
        payload.push(json!({
            "group_id": group.group_id,
            "path_prefix": group.path_prefix,
            "coherence_key": group.coherence_key,
            "memories": entries,
        }));
    }
    let groups_json = serde_json::to_string_pretty(&payload)
        .map_err(|error| format!("serialize distill input: {error}"))?;
    let user = format!(
        "Here are {} groups to distill:\n\n{}",
        groups.len(),
        groups_json
    );
    let input_bytes =
        DISTILL_DAILY_SYSTEM_PROMPT.len() + "<system>\n\n</system>\n\n".len() + user.len();
    check_input_budget(input_bytes)?;
    Ok(user)
}

pub fn build_fallback_user_payload(group: &CandidateGroup) -> Result<String, String> {
    let mut buf = String::new();
    buf.push_str(&format!(
        "path_prefix: {}\ncoherence_key: {}\n\n",
        group.path_prefix, group.coherence_key
    ));
    for (idx, entry) in group.entries.iter().enumerate() {
        buf.push_str(&format!(
            "[{}] topic={} importance={:.2}\nSummary: {}\nText: {}\n\n",
            idx + 1,
            if entry.topic.is_empty() {
                "unknown"
            } else {
                &entry.topic
            },
            entry.importance,
            entry.summary,
            entry.text.chars().take(600).collect::<String>()
        ));
        check_input_budget(DISTILL_DAILY_SYSTEM_PROMPT_SINGLE.len() + buf.len())?;
    }
    check_input_budget(DISTILL_DAILY_SYSTEM_PROMPT_SINGLE.len() + buf.len())?;
    Ok(buf)
}

fn check_input_budget(input_bytes: usize) -> Result<(), String> {
    if input_bytes > MAX_BATCH_PAYLOAD_BYTES {
        return Err(format!(
            "distill_input_budget_exceeded: {input_bytes} bytes exceeds {MAX_BATCH_PAYLOAD_BYTES}"
        ));
    }
    Ok(())
}

pub fn parse_distill_response(raw: &str) -> Result<HashMap<String, GroupPayload>, String> {
    let json_text = extract_json_payload(raw)?;
    let arr: Value = serde_json::from_str(json_text)
        .or_else(|_| serde_json::from_str(&escape_control_chars_in_json_strings(json_text)))
        .map_err(|e| {
            format!(
                "invalid distill JSON: {e} (snippet: {})",
                snippet(json_text)
            )
        })?;
    let arr = arr
        .as_array()
        .ok_or_else(|| "distill response must be a JSON array".to_string())?;

    let mut out = HashMap::with_capacity(arr.len());
    for item in arr {
        let Some(group_id) = item.get("group_id").and_then(|v| v.as_str()) else {
            continue;
        };
        if out.contains_key(group_id) {
            return Err(format!("duplicate distill response group_id: {group_id}"));
        }
        let text = item
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let summary = item
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let keywords = item
            .get("keywords")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .map(|s| s.to_string())
            .collect();
        let skip_reason = item
            .get("skip_reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        out.insert(
            group_id.to_string(),
            GroupPayload {
                summary,
                text,
                keywords,
                skip_reason,
            },
        );
    }
    Ok(out)
}

fn strip_code_fence(text: &str) -> &str {
    let text = text.trim();
    let inner = if let Some(stripped) = text.strip_prefix("```json") {
        stripped.trim()
    } else if let Some(stripped) = text.strip_prefix("```") {
        stripped
    } else {
        return text;
    };

    if let Some(idx) = inner.rfind("```") {
        inner[..idx].trim()
    } else {
        inner
    }
}

fn extract_json_payload(text: &str) -> Result<&str, String> {
    let text = strip_code_fence(text).trim();
    let start = text
        .char_indices()
        .find_map(|(idx, ch)| matches!(ch, '{' | '[').then_some((idx, ch)))
        .ok_or_else(|| format!("No JSON object or array found in response: {text}"))?;
    let (start_idx, open) = start;
    let close = if open == '{' { '}' } else { ']' };
    let mut stack = vec![close];
    let mut in_string = false;
    let mut escaped = false;

    for (rel_idx, ch) in text[start_idx..].char_indices().skip(1) {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop() != Some(ch) {
                    return Err(format!("Mismatched JSON delimiter in response: {text}"));
                }
                if stack.is_empty() {
                    let end_idx = start_idx + rel_idx + ch.len_utf8();
                    return Ok(&text[start_idx..end_idx]);
                }
            }
            _ => {}
        }
    }

    Err(format!("Incomplete JSON payload in response: {text}"))
}

fn escape_control_chars_in_json_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
                out.push(ch);
            } else if ch == '\\' {
                escaped = true;
                out.push(ch);
            } else if ch == '"' {
                in_string = false;
                out.push(ch);
            } else if ch == '\n' {
                out.push_str("\\n");
            } else if ch == '\r' {
                out.push_str("\\r");
            } else if ch == '\t' {
                out.push_str("\\t");
            } else if (ch as u32) < 0x20 {
                use std::fmt::Write as _;
                let _ = write!(&mut out, "\\u{:04x}", ch as u32);
            } else {
                out.push(ch);
            }
        } else {
            if ch == '"' {
                in_string = true;
            }
            out.push(ch);
        }
    }
    out
}

fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_distill_response_rejects_duplicate_ids_instead_of_overwriting() {
        let raw = r#"[{"group_id":"same","text":"refund facts"},{"group_id":"same","text":"login facts"}]"#;
        let error = parse_distill_response(raw).expect_err("ambiguous source association");
        assert!(error.contains("duplicate distill response group_id: same"));
    }

    #[test]
    fn parse_distill_response_handles_raw_newlines_inside_json_strings() {
        let raw = "[\n  {\n    \"group_id\": \"g1\",\n    \"summary\": \"s\",\n    \"text\": \"[核心] line 1\n[结论] line 2\",\n    \"keywords\": [\"a\"]\n  }\n]";
        let parsed = parse_distill_response(raw).expect("parse distill response with raw newline");
        let g1 = parsed.get("g1").expect("g1 payload");
        assert_eq!(g1.text, "[核心] line 1\n[结论] line 2");
    }

    #[test]
    fn parse_distill_response_handles_fenced_array() {
        let raw = "```json\n[{\"group_id\":\"g1\",\"summary\":\"s\",\"text\":\"body\",\"keywords\":[\"a\"]}]\n```";
        let parsed = parse_distill_response(raw).expect("parse distill response");

        let g1 = parsed.get("g1").expect("g1 payload");
        assert_eq!(g1.summary, "s");
        assert_eq!(g1.text, "body");
        assert_eq!(g1.keywords, vec!["a"]);
    }

    #[test]
    fn parse_distill_response_ignores_prefix_and_suffix() {
        let raw = "<think>ignore</think>\n[{\"group_id\":\"g1\",\"summary\":\"s\",\"text\":\"body\"}]\nextra text";
        let parsed = parse_distill_response(raw).expect("parse distill response");

        assert_eq!(parsed.get("g1").expect("g1 payload").text, "body");
    }

    #[test]
    fn parse_distill_response_preserves_nested_json_and_string_brackets() {
        let raw = r#"
            [
              {
                "group_id": "g1",
                "summary": "s",
                "text": "body with ] and { inside a string",
                "keywords": ["a", "b"]
              }
            ]
            trailing notes with ] that should be ignored
        "#;
        let parsed = parse_distill_response(raw).expect("parse distill response");

        let g1 = parsed.get("g1").expect("g1 payload");
        assert_eq!(g1.text, "body with ] and { inside a string");
        assert_eq!(g1.keywords, vec!["a", "b"]);
    }

    #[test]
    fn parse_distill_response_rejects_non_array_payload() {
        let err = parse_distill_response(r#"{"group_id":"g1"}"#).unwrap_err();

        assert!(err.contains("must be a JSON array"), "got: {err}");
    }

    #[test]
    fn scrub_agent_noise_removes_tool_blobs_and_agent_traces() {
        let scrubbed = scrub_agent_noise(
            "**Analyzing path\n{\"file_path\":\"src/lib.rs\"}\nkeep this line\n\n\nnext line",
        );

        assert!(!scrubbed.contains("Analyzing"));
        assert!(!scrubbed.contains("file_path"));
        assert!(scrubbed.contains("keep this line\n\nnext line"));
    }
}
