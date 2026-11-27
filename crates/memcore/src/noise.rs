// noise.rs — Text noise detection and query skip logic
//
// Ported from memory-lancedb-pro's noise-filter.ts + adaptive-retrieval.ts.
// Runs entirely in Rust — zero I/O, zero LLM calls.

use regex::Regex;
use std::sync::OnceLock;

// ─── Noise Detection (for storing) ──────────────────────────────────────────

fn denial_patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)i don'?t have (any )?(information|data|memory|record)").unwrap(),
            Regex::new(r"(?i)i'?m not sure about").unwrap(),
            Regex::new(r"(?i)i don'?t recall").unwrap(),
            Regex::new(r"(?i)i don'?t remember").unwrap(),
            Regex::new(r"(?i)it looks like i don'?t").unwrap(),
            Regex::new(r"(?i)i wasn'?t able to find").unwrap(),
            Regex::new(r"(?i)no (relevant )?memories found").unwrap(),
            Regex::new(r"(?i)i don'?t have access to").unwrap(),
            Regex::new(r"(?i)^i apologize\b").unwrap(),
            Regex::new(r"(?i)^as an ai\b").unwrap(),
            Regex::new(r"(?i)^i cannot\b").unwrap(),
            Regex::new(r"(?i)^i can'?t\b").unwrap(),
            Regex::new(r"(?i)^i am unable to\b").unwrap(),
            Regex::new(r"(?i)^i'?m unable to\b").unwrap(),
            Regex::new(r"(?i)^let me know if\b").unwrap(),
            Regex::new(r"(?i)^feel free to\b").unwrap(),
            Regex::new(r"(?i)^is there anything else\b").unwrap(),
        ]
    })
}

fn meta_question_patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)\bdo you (remember|recall|know about)\b").unwrap(),
            Regex::new(r"(?i)\bcan you (remember|recall)\b").unwrap(),
            Regex::new(r"(?i)\bdid i (tell|mention|say|share)\b").unwrap(),
            Regex::new(r"(?i)\bhave i (told|mentioned|said)\b").unwrap(),
            Regex::new(r"(?i)\bwhat did i (tell|say|mention)\b").unwrap(),
        ]
    })
}

fn boilerplate_patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)^(hi|hello|hey|good morning|good evening|greetings)\b").unwrap(),
            Regex::new(r"(?i)^fresh session").unwrap(),
            Regex::new(r"(?i)^new session").unwrap(),
            // A bare heartbeat ping, not text that mentions heartbeats.
            Regex::new(r"(?i)^\s*\[?heartbeat(_ok)?\]?\s*[.!]?\s*$").unwrap(),
        ]
    })
}

/// Maximum char count for boilerplate matching.
/// "Hello, my name is Kyle" should NOT be noise; "Hello" should be.
const BOILERPLATE_MAX_CHARS: usize = 30;

/// Check if a text is noise that should NOT be stored as memory.
/// Returns `true` if the text is noise.
pub fn is_noise_text(text: &str) -> bool {
    let trimmed = text.trim();
    let char_count = trimmed.chars().count();

    // Too short to be meaningful
    if char_count < 5 {
        return true;
    }

    // Denial patterns match at any length (entire response is a denial)
    if denial_patterns().iter().any(|p| p.is_match(trimmed)) {
        return true;
    }

    // Meta-question patterns match at any length
    if meta_question_patterns().iter().any(|p| p.is_match(trimmed)) {
        return true;
    }

    // Boilerplate only applies to short texts to avoid false-positives
    if char_count <= BOILERPLATE_MAX_CHARS
        && boilerplate_patterns().iter().any(|p| p.is_match(trimmed))
    {
        return true;
    }

    false
}

// ─── Adaptive Retrieval (for querying) ──────────────────────────────────────

fn skip_patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)^(hi|hello|hey|good\s*(morning|afternoon|evening|night)|greetings|yo|sup|howdy)\b").unwrap(),
            Regex::new(r"^/").unwrap(),
            Regex::new(r"(?i)^(run|build|test|ls|cd|git|npm|pip|docker|curl|cat|grep|find|make|sudo)\s").unwrap(),
            Regex::new(r"(?i)^(yes|no|yep|nope|ok|okay|sure|fine|thanks|thank you|thx|ty|got it|understood|cool|nice|great|good|perfect|awesome)\s*[.!]?$").unwrap(),
            Regex::new(r"(?i)^(go ahead|continue|proceed|do it|start|begin|next|实施|開始|开始|继续|繼續|好的|可以|行)\s*[.!]?$").unwrap(),
            // A bare heartbeat ping. Queries about claim heartbeats must still search (#2236).
            Regex::new(r"(?i)^\s*\[?heartbeat(_ok)?\]?\s*[.!]?\s*$").unwrap(),
            Regex::new(r"(?i)^\[System").unwrap(),
            Regex::new(r"(?i)^(ping|pong|test|debug)\s*[.!?]?$").unwrap(),
        ]
    })
}

fn force_retrieve_patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)\b(remember|recall|forgot|memory|memories)\b").unwrap(),
            Regex::new(r"(?i)\b(last time|before|previously|earlier|yesterday|ago)\b").unwrap(),
            Regex::new(r"(?i)\b(my (name|email|phone|address|birthday|preference))\b").unwrap(),
            Regex::new(r"(?i)\b(what did (i|we)|did i (tell|say|mention))\b").unwrap(),
            Regex::new(r"(你记得|之前|上次|以前|还记得|還記得|提到过|提到過|说过|說過)").unwrap(),
        ]
    })
}

/// Maximum char count for skip-pattern-based skipping.
/// Longer queries only skip via explicit full-string patterns (affirmations etc.)
const SKIP_MAX_CHARS: usize = 40;

/// Check if a character is CJK (shared with scorer.rs).
#[inline]
pub fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{AC00}'..='\u{D7AF}' // Hangul
    )
}

/// Check if a string is pure emoji/whitespace.
fn is_pure_emoji(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    s.chars()
        .all(|c| c.is_whitespace() || (!c.is_alphanumeric() && !is_cjk(c)))
}

fn has_compound_technical_token(s: &str) -> bool {
    s.split_whitespace().any(|token| {
        let has_joiner = token.contains('-') || token.contains('_') || token.contains('/');
        if !has_joiner {
            return false;
        }
        token
            .split(|c: char| !c.is_alphanumeric())
            .filter(|part| part.chars().count() >= 2)
            .count()
            >= 2
    })
}

/// Determine if a query should skip memory retrieval.
/// Returns `true` if retrieval should be skipped (query is not worth searching).
pub fn should_skip_query(query: &str) -> bool {
    let trimmed = query.trim();
    let char_count = trimmed.chars().count();

    // Force retrieve if query has memory-related intent (checked FIRST)
    if force_retrieve_patterns()
        .iter()
        .any(|p| p.is_match(trimmed))
    {
        return false;
    }

    // Too short
    if char_count < 3 {
        return true;
    }

    // Pure emoji
    if is_pure_emoji(trimmed) {
        return true;
    }

    // Skip patterns: greeting/command patterns only on short text,
    // full-string patterns (affirmations) always apply
    if char_count <= SKIP_MAX_CHARS && skip_patterns().iter().any(|p| p.is_match(trimmed)) {
        return true;
    }
    // Even for long text, check full-string-anchored patterns (affirmations, pings)
    if char_count > SKIP_MAX_CHARS {
        let patterns = skip_patterns();
        if patterns.get(3).is_some_and(|p| p.is_match(trimmed))
            || patterns.get(4).is_some_and(|p| p.is_match(trimmed))
        {
            return true;
        }
    }

    // CJK-aware minimum length threshold (uses char count, not byte count)
    let has_cjk = trimmed.chars().any(is_cjk);
    let default_min_length = if has_cjk { 4 } else { 10 };
    let has_question = trimmed.contains('?') || trimmed.contains('？');

    if char_count < default_min_length && !has_question && !has_compound_technical_token(trimmed) {
        return true;
    }

    // Default: do retrieve
    false
}

/// Remove `<think>...</think>` tags from a text block.
pub fn scrub_think_tags(text: &str) -> String {
    static THINK_BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    let re = THINK_BLOCK_RE.get_or_init(|| {
        Regex::new(r"(?is)<think\b[^>]*>.*?</think>").expect("valid think-tag regex")
    });
    re.replace_all(text, "").trim().to_string()
}

/// Position of `needle` in `text` at or after `from`, ASCII
/// case-insensitive.
fn find_ascii_ci(text: &str, needle: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let last = bytes.len().checked_sub(needle.len())?;
    (from..=last).find(|&at| bytes[at..at + needle.len()].eq_ignore_ascii_case(needle.as_bytes()))
}

/// Byte ranges of the `<think>` blocks in `text`. A block that never closes
/// runs to the end: it was cut off mid-thought, and nothing after it is an
/// answer.
fn reasoning_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(open) = find_ascii_ci(text, "<think", from) {
        let after_name = open + "<think".len();
        from = after_name;
        let is_tag = text[after_name..].starts_with('>')
            || text[after_name..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace);
        if !is_tag {
            continue;
        }
        let close = find_ascii_ci(text, "</think>", after_name).map(|at| at + "</think>".len());
        let end = close.unwrap_or(text.len());
        ranges.push((open, end));
        if close.is_none() {
            break;
        }
        from = end;
    }
    ranges
}

/// Remove ```json markdown code fences from a response
pub fn strip_code_fence(text: &str) -> &str {
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

/// Whether `rest` could be the inside of a JSON value: the bracket before it
/// may be an answer. "[核心]" and "{name}" or "{1}" are prose.
fn starts_a_json_value(open: char, rest: &str) -> bool {
    let rest = rest.trim_start();
    match (open, rest.as_bytes().first()) {
        // A JSON object only begins with a quoted key or an immediate `}`.
        ('{', Some(b'"' | b'}')) => true,
        ('{', _) => false,
        ('[', Some(b'{' | b'[' | b'"' | b']' | b'-' | b'0'..=b'9')) => true,
        ('[', Some(_)) => ["true", "false", "null"]
            .iter()
            .any(|word| rest.starts_with(word)),
        (_, None) | (_, Some(_)) => false,
    }
}

/// Index just past the bracket that closes the one at `start_idx`.
fn balanced_end(text: &str, start_idx: usize, open: char) -> Result<usize, String> {
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
                    return Ok(start_idx + rel_idx + ch.len_utf8());
                }
            }
            _ => {}
        }
    }

    Err(format!("Incomplete JSON payload in response: {text}"))
}

/// Extract the first complete JSON object/array from an LLM response.
///
/// Some reasoning models prepend hidden-thought text or other prose before
/// the JSON even when the prompt asks for JSON-only. Keep strict JSON
/// parsing, but feed the parser the first balanced JSON payload instead of
/// the whole response. The reasoning is passed over, and so is a bracket
/// that is prose (a `[1]` citation, a `[核心]` marker).
pub fn extract_json_payload(text: &str) -> Result<&str, String> {
    let shown = strip_code_fence(text);
    let offset = shown.as_ptr() as usize - text.as_ptr() as usize;
    let reasoning = reasoning_ranges(text);
    let mut passed_over: Option<&str> = None;
    for (idx, open) in shown
        .char_indices()
        .filter(|(_, ch)| matches!(ch, '{' | '['))
    {
        let at = idx + offset;
        let in_reasoning = reasoning.iter().any(|&(from, to)| from <= at && at < to);
        if in_reasoning || !starts_a_json_value(open, &shown[idx + open.len_utf8()..]) {
            continue;
        }
        let payload = &shown[idx..balanced_end(shown, idx, open)?];
        // A lone number in brackets (`[1]`) is a citation while an answer follows;
        // empty `[]` or `{}` is already a valid empty JSON container.
        let inner = &payload[1..payload.len() - 1];
        if open == '[' && !inner.is_empty() && inner.bytes().all(|byte| byte.is_ascii_digit()) {
            passed_over.get_or_insert(payload);
            continue;
        }
        return Ok(payload);
    }
    passed_over.ok_or_else(|| format!("No JSON object or array found in response: {shown}"))
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_noise_text ────────────────────────────────────────────────────

    // ── extract_json_payload ────────────────────────────────────────────

    #[test]
    fn extract_json_payload_takes_the_answer_after_the_reasoning() {
        let raw = "<think>draft: {\"ok\": false}</think>\n{\"ok\": true}";
        assert_eq!(
            extract_json_payload(raw).expect("payload"),
            "{\"ok\": true}"
        );
    }

    #[test]
    fn extract_json_payload_passes_over_prose_markers() {
        let raw = "See [1] and [核心] for context:\n{\"ok\": true}";
        assert_eq!(
            extract_json_payload(raw).expect("payload"),
            "{\"ok\": true}"
        );
    }

    #[test]
    fn extract_json_payload_keeps_a_lone_number_array_that_is_the_answer() {
        assert_eq!(extract_json_payload("[7]").expect("payload"), "[7]");
        assert_eq!(extract_json_payload("ids: [7]").expect("payload"), "[7]");
    }

    #[test]
    fn extract_json_payload_reads_a_fenced_answer_after_prose() {
        let raw = "Here it is:\n```json\n{\"ok\": true}\n```";
        assert_eq!(
            extract_json_payload(raw).expect("payload"),
            "{\"ok\": true}"
        );
    }

    #[test]
    fn extract_json_payload_reports_a_truncated_payload_whole() {
        let inner = "{\"b\": 1}";
        for raw in [format!("{{\"a\": {inner}"), "{\"a\": [1}".to_string()] {
            let err = match extract_json_payload(&raw) {
                Ok(payload) => panic!("a truncated payload yielded {payload}"),
                Err(err) => err,
            };
            assert!(
                err.starts_with("Incomplete JSON payload") || err.starts_with("Mismatched JSON"),
                "{err}"
            );
        }
    }

    #[test]
    fn extract_json_payload_finds_no_answer_in_cut_off_reasoning() {
        let err = match extract_json_payload("<think>still planning {\"step\": 1}") {
            Ok(payload) => panic!("the reasoning yielded {payload}"),
            Err(err) => err,
        };
        assert!(err.starts_with("No JSON object or array found"), "{err}");
    }

    #[test]
    fn extract_json_payload_accepts_empty_array_or_object_after_citation() {
        assert_eq!(extract_json_payload("See [1]\n[]").unwrap(), "[]");
        assert_eq!(extract_json_payload("See [1]\n{}").unwrap(), "{}");
    }

    #[test]
    fn extract_json_payload_keeps_empty_array_ahead_of_later_object() {
        assert_eq!(
            extract_json_payload("[]\nNote: {\"example\": 1}").unwrap(),
            "[]"
        );
    }

    #[test]
    fn extract_json_payload_skips_numeric_braced_prose_before_object() {
        assert_eq!(
            extract_json_payload("{1}\n{\"ok\": true}").unwrap(),
            "{\"ok\": true}"
        );
    }

    #[test]
    fn noise_too_short() {
        assert!(is_noise_text("hi"));
        assert!(is_noise_text("   "));
        assert!(is_noise_text("ok"));
    }

    #[test]
    fn noise_denials() {
        assert!(is_noise_text("I don't have any information about that"));
        assert!(is_noise_text("I don't recall anything relevant"));
        assert!(is_noise_text("No relevant memories found"));
        assert!(is_noise_text("I wasn't able to find anything"));
        assert!(is_noise_text("I apologize, but I can't help with that"));
        assert!(is_noise_text("As an AI, I cannot access that information"));
    }

    #[test]
    fn noise_meta_questions() {
        assert!(is_noise_text("Do you remember what I said yesterday?"));
        assert!(is_noise_text("Can you recall the details?"));
        assert!(is_noise_text("Did I tell you about the project?"));
    }

    #[test]
    fn noise_boilerplate() {
        assert!(is_noise_text("hello there"));
        assert!(is_noise_text("Good morning!"));
        assert!(is_noise_text("HEARTBEAT"));
        assert!(is_noise_text("Hey, how are you?"));
    }

    #[test]
    fn noise_real_content_passes() {
        assert!(!is_noise_text("用户偏好使用 TypeScript 编写代码"));
        assert!(!is_noise_text(
            "The database schema uses SQLite with FTS5 indexing"
        ));
        assert!(!is_noise_text(
            "Decision: use Rust for the memory core engine"
        ));
        assert!(!is_noise_text("记住：每次部署前先运行测试"));
    }

    #[test]
    fn noise_greeting_with_substance_passes() {
        // Bug #4 fix: greeting-prefixed substantive text should NOT be noise
        assert!(!is_noise_text(
            "Hello, my name is Kyle and I prefer TypeScript"
        ));
        assert!(!is_noise_text(
            "Hey, production broke after the migration and we need to roll back"
        ));
    }

    // ── should_skip_query ────────────────────────────────────────────────

    #[test]
    fn skip_greetings() {
        assert!(should_skip_query("hi"));
        assert!(should_skip_query("hello"));
        assert!(should_skip_query("Good morning"));
    }

    #[test]
    fn skip_commands() {
        assert!(should_skip_query("/reset"));
        assert!(should_skip_query("/new"));
        assert!(should_skip_query("git status"));
    }

    #[test]
    fn skip_affirmations() {
        assert!(should_skip_query("ok"));
        assert!(should_skip_query("yes"));
        assert!(should_skip_query("got it"));
        assert!(should_skip_query("好的"));
        assert!(should_skip_query("继续"));
    }

    #[test]
    fn skip_short_non_question() {
        assert!(should_skip_query("do it"));
        assert!(should_skip_query("test"));
    }

    #[test]
    fn technical_compound_queries_are_not_skipped() {
        assert!(!should_skip_query("dry-run"));
        assert!(!should_skip_query("clean-cli"));
        assert!(!should_skip_query("pre_commit"));
    }

    #[test]
    fn force_memory_keywords() {
        assert!(!should_skip_query("你记得上次的决策吗"));
        assert!(!should_skip_query("Do you remember the config?"));
        assert!(!should_skip_query("What did I say about TypeScript?"));
        assert!(!should_skip_query("之前讨论的架构方案"));
    }

    #[test]
    fn real_queries_pass() {
        assert!(!should_skip_query(
            "How to implement hybrid search in Rust?"
        ));
        assert!(!should_skip_query("什么是 memcore 的 ACT-R decay 公式？"));
        assert!(!should_skip_query(
            "Show me the database schema for memories"
        ));
    }

    #[test]
    fn skip_pure_emoji() {
        assert!(should_skip_query("👍"));
        assert!(should_skip_query("👍👎"));
        assert!(should_skip_query("  ✅  "));
    }

    #[test]
    fn skip_heartbeat() {
        assert!(should_skip_query("HEARTBEAT"));
        assert!(should_skip_query("HEARTBEAT_OK"));
        assert!(should_skip_query("[System] heartbeat check"));
    }

    #[test]
    fn queries_and_memories_about_heartbeats_are_not_noise() {
        assert!(!should_skip_query("heartbeat claim lease"));
        assert!(!should_skip_query(
            "What is the worker heartbeat interval for claim leases?"
        ));
        // Within the 30-char boilerplate window, so the old `^HEARTBEAT` matched it.
        assert!(!is_noise_text("Heartbeat lease renews at 90s"));
    }

    #[test]
    fn greeting_with_substance_not_skipped() {
        // Bug #5 fix: greeting-prefixed substantive queries should NOT skip
        assert!(!should_skip_query(
            "hello, can you help me debug the memory engine crash?"
        ));
        assert!(!should_skip_query(
            "Hey, production broke after the migration and we need help"
        ));
    }

    #[test]
    fn cjk_length_uses_chars_not_bytes() {
        // Bug #6 fix: "测试？" is 3 chars (not 9 bytes) — should NOT skip (has question)
        assert!(!should_skip_query("测试？"));
        // "做" is 1 char — too short, should skip
        assert!(should_skip_query("做"));
        // "记忆系统" is 4 CJK chars — at threshold, should NOT skip
        assert!(!should_skip_query("记忆系统"));
    }
}
