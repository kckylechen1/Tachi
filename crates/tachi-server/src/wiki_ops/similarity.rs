use super::*;

pub(super) fn parse_rfc3339_utc(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn tokenize_for_similarity(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub(super) fn token_cosine_similarity(a: &str, b: &str) -> f64 {
    let mut freq_a: HashMap<String, f64> = HashMap::new();
    let mut freq_b: HashMap<String, f64> = HashMap::new();
    for token in tokenize_for_similarity(a) {
        *freq_a.entry(token).or_insert(0.0) += 1.0;
    }
    for token in tokenize_for_similarity(b) {
        *freq_b.entry(token).or_insert(0.0) += 1.0;
    }
    if freq_a.is_empty() || freq_b.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let norm_a = freq_a.values().map(|v| v * v).sum::<f64>().sqrt();
    let norm_b = freq_b.values().map(|v| v * v).sum::<f64>().sqrt();
    for (token, value_a) in &freq_a {
        if let Some(value_b) = freq_b.get(token) {
            dot += value_a * value_b;
        }
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

pub(super) fn contradiction_score(a: &str, b: &str) -> f64 {
    let negations = [
        "never", "not", "avoid", "disable", "forbid", "against", "cannot",
    ];
    let affirmations = ["always", "use", "enable", "allow", "prefer", "should"];
    let a_lower = a.to_ascii_lowercase();
    let b_lower = b.to_ascii_lowercase();
    let a_neg = negations.iter().any(|token| a_lower.contains(token));
    let b_neg = negations.iter().any(|token| b_lower.contains(token));
    let a_aff = affirmations.iter().any(|token| a_lower.contains(token));
    let b_aff = affirmations.iter().any(|token| b_lower.contains(token));
    if (a_neg && b_aff) || (b_neg && a_aff) {
        token_cosine_similarity(a, b)
    } else {
        0.0
    }
}

pub(super) fn relation_exists(
    edges: &[memcore::MemoryEdge],
    a: &str,
    b: &str,
    relation: Option<&str>,
) -> bool {
    edges.iter().any(|edge| {
        let matches_nodes = (edge.source_id == a && edge.target_id == b)
            || (edge.source_id == b && edge.target_id == a);
        let matches_relation = relation.map(|rel| edge.relation == rel).unwrap_or(true);
        matches_nodes && matches_relation
    })
}
