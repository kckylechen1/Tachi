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

/// Token-frequency vectors for a batch of documents, tokenized once each.
///
/// [`SimilarityCorpus::for_each_later_pair`] reports exactly (bit for bit) what
/// [`token_cosine_similarity`] returns for the same two strings: term counts,
/// their products and their squares are small integers, so every sum is exact
/// in `f64` regardless of order and the final `dot / (norm_a * norm_b)` is the
/// same expression. The difference is cost: each document is tokenized once
/// instead of once per pair, and the dot product uses interned term ids.
pub(super) struct SimilarityCorpus {
    docs: Vec<TermVector>,
    scratch: Vec<u64>,
}

struct TermVector {
    /// `(term id, count)`, one entry per distinct term.
    terms: Vec<(u32, u64)>,
    norm: f64,
}

impl SimilarityCorpus {
    pub(super) fn new<'a>(texts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut vocabulary: HashMap<String, u32> = HashMap::new();
        let mut docs = Vec::new();
        for text in texts {
            let mut counts: HashMap<u32, u64> = HashMap::new();
            for token in tokenize_for_similarity(text) {
                let next_id = vocabulary.len() as u32;
                let id = *vocabulary.entry(token).or_insert(next_id);
                *counts.entry(id).or_insert(0) += 1;
            }
            let terms: Vec<(u32, u64)> = counts.into_iter().collect();
            let sum_squares: u64 = terms.iter().map(|(_, count)| count * count).sum();
            docs.push(TermVector {
                terms,
                norm: (sum_squares as f64).sqrt(),
            });
        }
        Self {
            docs,
            scratch: vec![0; vocabulary.len()],
        }
    }

    pub(super) fn len(&self) -> usize {
        self.docs.len()
    }

    /// Cosine similarity of document `left` against every later document,
    /// reported through `visit(right_index, similarity)` in ascending order.
    pub(super) fn for_each_later_pair(&mut self, left: usize, mut visit: impl FnMut(usize, f64)) {
        let Self { docs, scratch } = self;
        let left_doc = &docs[left];
        for &(id, count) in &left_doc.terms {
            scratch[id as usize] = count;
        }
        for (right, right_doc) in docs.iter().enumerate().skip(left + 1) {
            let similarity = if left_doc.terms.is_empty() || right_doc.terms.is_empty() {
                0.0
            } else {
                let dot: u64 = right_doc
                    .terms
                    .iter()
                    .map(|&(id, count)| scratch[id as usize] * count)
                    .sum();
                let dot = dot as f64;
                if left_doc.norm == 0.0 || right_doc.norm == 0.0 {
                    0.0
                } else {
                    dot / (left_doc.norm * right_doc.norm)
                }
            };
            visit(right, similarity);
        }
        for &(id, _) in &left_doc.terms {
            scratch[id as usize] = 0;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_corpus_matches_pairwise_cosine_bit_for_bit() {
        let texts = [
            "Follow SOP: inspect logs, isolate failure, add regression test.",
            "Follow SOP: inspect logs, isolate failure, add regression tests!",
            "alpha beta beta gamma gamma gamma delta_1 delta_1",
            "",
            "!!! ???",
            "gamma alpha BETA beta Delta_1 zeta zeta zeta zeta zeta",
            "alpha beta beta gamma gamma gamma delta_1 delta_1",
        ];
        let mut corpus = SimilarityCorpus::new(texts.iter().copied());
        assert_eq!(corpus.len(), texts.len());
        let mut visited = 0;
        for left in 0..texts.len() {
            let mut expected_right = left + 1;
            corpus.for_each_later_pair(left, |right, similarity| {
                assert_eq!(right, expected_right);
                expected_right += 1;
                let expected = token_cosine_similarity(texts[left], texts[right]);
                assert_eq!(
                    similarity.to_bits(),
                    expected.to_bits(),
                    "pair ({left}, {right}): {similarity} != {expected}"
                );
                visited += 1;
            });
        }
        assert_eq!(visited, texts.len() * (texts.len() - 1) / 2);
    }
}
