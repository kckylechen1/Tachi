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
            Regex::new(r"(?i)^HEARTBEAT").unwrap(),
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
            Regex::new(r"(?i)HEARTBEAT").unwrap(),
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

/// Determine if a query should skip memory retrieval.
/// Returns `true` if retrieval should be skipped (query is not worth searching).
pub fn should_skip_query(query: &str) -> bool {
    let trimmed = query.trim();
    let char_count = trimmed.chars().count();

    // Force retrieve if query has memory-related intent (checked FIRST)
    if force_retrieve_patterns().iter().any(|p| p.is_match(trimmed)) {
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
        if patterns
            .iter()
            .any(|p| p.as_str().contains("HEARTBEAT") && p.is_match(trimmed))
        {
            return true;
        }
    }

    // CJK-aware minimum length threshold (uses char count, not byte count)
    let has_cjk = trimmed.chars().any(is_cjk);
    let default_min_length = if has_cjk { 4 } else { 10 };
    let has_question = trimmed.contains('?') || trimmed.contains('？');

    if char_count < default_min_length && !has_question {
        return true;
    }

    // Default: do retrieve
    false
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_noise_text ────────────────────────────────────────────────────

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
        assert!(!should_skip_query(
            "什么是 memory-core 的 ACT-R decay 公式？"
        ));
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
        assert!(should_skip_query("[System] heartbeat check"));
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
