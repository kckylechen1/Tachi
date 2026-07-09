use crate::tool_params::Message;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BracketSelfEvolutionNote {
    pub id: String,
    pub text: String,
    pub category: String,
}

fn bracket_note_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"（([^（）\r\n]{4,240})）|\(([^()\r\n]{4,240})\)")
            .expect("bracket_note_regex is a valid compile-time regex")
    })
}

fn bracket_strategy_regexes() -> &'static [Regex] {
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    REGEXES
        .get_or_init(|| {
            [
                r"原来.{0,20}(喜欢|不喜欢)",
                r"记住了",
                r"下次我要|下次我会",
                r"以后我要|以后我会",
                r"雷区",
                r"更吃这一套|不吃这一套",
                r"这样更有效|这种方式有用",
                r"策略失败|无效",
            ]
            .into_iter()
            .map(|pattern| {
                Regex::new(pattern).expect("bracket_strategy regex is a valid compile-time pattern")
            })
            .collect()
        })
        .as_slice()
}

fn bracket_decision_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"记住了|下次我要|下次我会|以后")
            .expect("bracket_decision_regex is a valid compile-time regex")
    })
}

fn bracket_preference_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"喜欢|不喜欢|雷区|偏好|讨厌|更吃|不吃")
            .expect("bracket_preference_regex is a valid compile-time regex")
    })
}

pub(crate) fn build_bracket_self_evolution_id(agent_id: &str, note_text: &str) -> String {
    let seed = format!("{}{}", agent_id.trim(), note_text.trim());
    format!(
        "bracket-self-evolution:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes())
    )
}

pub(crate) fn classify_bracket_self_evolution(note_text: &str) -> String {
    let trimmed = note_text.trim();
    if bracket_decision_regex().is_match(trimmed) {
        "decision".to_string()
    } else if bracket_preference_regex().is_match(trimmed) {
        "preference".to_string()
    } else {
        "experience".to_string()
    }
}

pub(crate) fn extract_bracket_self_evolution_notes(
    agent_id: &str,
    messages: &[Message],
) -> Vec<BracketSelfEvolutionNote> {
    let mut seen_ids = HashSet::new();
    let mut notes = Vec::new();

    for message in messages
        .iter()
        .filter(|message| message.role.trim().eq_ignore_ascii_case("assistant"))
    {
        for captures in bracket_note_regex().captures_iter(&message.content) {
            let note_text = captures
                .get(1)
                .or_else(|| captures.get(2))
                .map(|value| value.as_str().trim())
                .unwrap_or("");
            let char_count = note_text.chars().count();
            if !(4..=240).contains(&char_count) {
                continue;
            }
            if !bracket_strategy_regexes()
                .iter()
                .any(|pattern| pattern.is_match(note_text))
            {
                continue;
            }

            let id = build_bracket_self_evolution_id(agent_id, note_text);
            if !seen_ids.insert(id.clone()) {
                continue;
            }

            notes.push(BracketSelfEvolutionNote {
                id,
                text: note_text.to_string(),
                category: classify_bracket_self_evolution(note_text),
            });
        }
    }

    notes
}

pub(crate) fn matches_agent_tag(agent_id: &str, tag: &str) -> bool {
    if tag.is_empty() {
        return false;
    }
    // Single-token tags (e.g. "jayne"): split on non-alphanumeric delimiters.
    if agent_id
        .split(|c: char| !c.is_alphanumeric())
        .any(|part| part.eq_ignore_ascii_case(tag))
    {
        return true;
    }
    // Hyphenated slugs (e.g. "user-memory", "user-memory-v3"): match path-like
    // segments without treating interior hyphens as substring false positives.
    agent_id
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|segment| !segment.is_empty())
        .any(|segment| segment.eq_ignore_ascii_case(tag) || segment.starts_with(&format!("{tag}-")))
}
