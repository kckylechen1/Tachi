use memory_core::MemoryEntry;

pub(in crate::foundry_runtime_ops::maintenance::distill_helpers) fn guide_text_fragments<'a>(
    distill_text: &'a str,
    source_entries: &'a [MemoryEntry],
) -> impl Iterator<Item = &'a str> {
    std::iter::once(distill_text).chain(source_entries.iter().flat_map(|entry| {
        std::iter::once(entry.summary.as_str())
            .chain(std::iter::once(entry.text.as_str()))
            .chain(std::iter::once(entry.topic.as_str()))
            .chain(entry.keywords.iter().map(String::as_str))
    }))
}

pub(in crate::foundry_runtime_ops::maintenance::distill_helpers) fn contains_ignore_ascii_case(
    haystack: &str,
    needle: &str,
) -> bool {
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    let h = haystack.as_bytes();
    if h.len() < n.len() {
        return false;
    }
    h.windows(n.len())
        .any(|window| window.eq_ignore_ascii_case(n))
}

pub(in crate::foundry_runtime_ops::maintenance::distill_helpers) fn fragments_contain_any<'a, I>(
    fragments: I,
    needles: &[&str],
) -> bool
where
    I: IntoIterator<Item = &'a str>,
{
    for fragment in fragments {
        for needle in needles {
            if contains_ignore_ascii_case(fragment, needle) {
                return true;
            }
        }
    }
    false
}

pub(in crate::foundry_runtime_ops::maintenance::distill_helpers) fn contains_any(
    haystack: &str,
    needles: &[&str],
) -> bool {
    needles
        .iter()
        .any(|needle| contains_ignore_ascii_case(haystack, needle))
}

pub(in crate::foundry_runtime_ops::maintenance::distill_helpers) fn has_numbered_steps(
    text: &str,
) -> bool {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            let mut chars = trimmed.chars();
            matches!(chars.next(), Some(ch) if ch.is_ascii_digit())
                && matches!(chars.next(), Some('.' | ')'))
        })
        .take(2)
        .count()
        >= 2
}
