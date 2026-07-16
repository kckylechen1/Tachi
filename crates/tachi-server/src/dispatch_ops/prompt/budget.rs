//! Bounded prompt-input admission for recalled memory and embedded skills.
//!
//! The limit applies to untrusted/recalled bodies and skill contracts only. It
//! deliberately leaves the task and dispatch envelope intact, and callers keep
//! a short reference when an item's body cannot fit.
//!
//! ## Budget contract (round-2, codex cross-review checkpoint 2.2)
//!
//! The character budget in [`PromptInputBudget`] constrains the raw
//! **untrusted payload itself** — the body text callers pass to [`admit`],
//! before any structural wrapping. It does **not** count:
//!
//! - the `### {path}\n` header a caller prepends to an admitted body, or
//! - the `<untrusted_content>`/`</untrusted_content>` delimiter tags
//!   `super::sanitize_untrusted` wraps around it.
//!
//! Both are trusted structure the assembler itself controls (not attacker
//! input), so they're applied *after* [`admit`] returns — this is also why
//! callers budget the raw body first and wrap second: a mid-budget
//! truncation can land inside the payload but can never sever a boundary
//! tag or header. This is an accepted, deliberate scoping (leader
//! disposition on codex checkpoint 2.2: "semantically acceptable, make it
//! explicit"), not a gap — total rendered prompt length is separately
//! guarded by the unrelated, coarser 50K-character global warning in
//! `assemble_prompt_with_trace` (`prompt.rs`'s `MAX_PROMPT_CHARS` check),
//! which exists to flag oversized prompts overall, not to re-bound the
//! per-item untrusted budget this module already enforces.
//!
//! [`admit`]: PromptInputBudget::admit

const ELLIPSIS: &str = "...";

#[derive(Debug)]
pub(super) struct PromptInputBudget {
    total_chars: usize,
    remaining_chars: usize,
    truncated_items: usize,
    omitted_items: usize,
}

impl PromptInputBudget {
    pub(super) fn from_env(env_name: &str, default_chars: usize) -> Self {
        Self::new(configured_budget(
            std::env::var(env_name).ok().as_deref(),
            default_chars,
        ))
    }

    pub(super) fn new(total_chars: usize) -> Self {
        Self {
            total_chars,
            remaining_chars: total_chars,
            truncated_items: 0,
            omitted_items: 0,
        }
    }

    /// Admit as much as fits, counting Unicode scalar values so a truncation
    /// cannot split a UTF-8 sequence. `None` means no body remains admissible;
    /// the caller should render a reference-only entry instead.
    pub(super) fn admit(&mut self, text: &str) -> Option<String> {
        if self.remaining_chars == 0 {
            self.omitted_items += 1;
            return None;
        }

        let text_len = text.chars().count();
        if text_len <= self.remaining_chars {
            self.remaining_chars -= text_len;
            return Some(text.to_string());
        }

        self.truncated_items += 1;
        let ellipsis_len = ELLIPSIS.chars().count();
        let admitted = if self.remaining_chars >= ellipsis_len {
            let keep = self.remaining_chars - ellipsis_len;
            let mut admitted = text.chars().take(keep).collect::<String>();
            admitted.push_str(ELLIPSIS);
            admitted
        } else {
            text.chars().take(self.remaining_chars).collect()
        };
        self.remaining_chars = 0;
        Some(admitted)
    }

    pub(super) fn summary(&self, kind: &str) -> Option<String> {
        let limited = self.truncated_items + self.omitted_items;
        (limited > 0).then(|| {
            format!(
                "- {kind} input budget: admitted {}/{} characters; truncated {} item(s), \
                 omitted {} body item(s) (references retained without raw content).",
                self.total_chars - self.remaining_chars,
                self.total_chars,
                self.truncated_items,
                self.omitted_items,
            )
        })
    }
}

fn configured_budget(raw: Option<&str>, default_chars: usize) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_budget_truncates_then_omits_without_splitting_utf8() {
        let mut budget = PromptInputBudget::new(5);

        assert_eq!(budget.admit("éééééé"), Some("éé...".to_string()));
        assert_eq!(budget.admit("later body"), None);
        let summary = budget.summary("memory").expect("budget is reported");
        assert!(summary.contains("admitted 5/5"), "{summary}");
        assert!(summary.contains("truncated 1"), "{summary}");
        assert!(summary.contains("omitted 1"), "{summary}");
    }

    #[test]
    fn tiny_budget_never_exceeds_its_limit_for_an_ellipsis() {
        let mut budget = PromptInputBudget::new(1);
        let admitted = budget.admit("oversized").expect("one character fits");

        assert_eq!(admitted, "o");
        assert_eq!(admitted.chars().count(), 1);
    }

    #[test]
    fn configured_budget_accepts_zero_and_rejects_malformed_values() {
        assert_eq!(configured_budget(Some("0"), 100), 0);
        assert_eq!(configured_budget(Some(" 512 "), 100), 512);
        assert_eq!(configured_budget(Some("many"), 100), 100);
        assert_eq!(configured_budget(Some(""), 100), 100);
    }
}
