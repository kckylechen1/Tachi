use chrono::Utc;
use memcore::MemoryEntry;
use serde_json::json;

use crate::utils::sanitize_safe_path_name;

pub(crate) fn merge_optional_metadata(metadata: Option<serde_json::Value>) -> serde_json::Value {
    match metadata {
        Some(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
        Some(other) => json!({ "payload": other }),
        None => json!({}),
    }
}

pub(crate) fn serialize_json(value: serde_json::Value) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|e| format!("Failed to serialize response: {e}"))
}

fn summary_from_text(text: &str) -> String {
    text.chars().take(100).collect()
}

pub(crate) fn default_ingest_chunk_size() -> usize {
    1200
}

pub(crate) fn default_ingest_chunk_overlap() -> usize {
    120
}

fn topic_from_path(path: &str) -> String {
    path.trim_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("ingest")
        .replace('-', "_")
}

/// Resolve the domain to stamp on an ingested/pipeline entry. #1041 S2: this
/// used to fall back to the `TACHI_DOMAIN` env var — a daemon-wide, per-process
/// setting (e.g. a trading daemon's fixed "equity_trading") — whenever the
/// caller omitted a domain. That inherits the *project's* main domain onto
/// content whose actual domain was never classified, which is how an
/// engineering probe row ends up double-mislabeled `equity_trading` on a
/// trading daemon. An absent domain is classified-or-`general` now, never a
/// blind per-process env default; `TACHI_DOMAIN` still rides on every write
/// as `metadata.provenance.domain` (see `provenance::inject_provenance`) —
/// that's a legitimate "what context was this captured under" audit trail,
/// distinct from (and no longer feeding) the entry's own canonical domain.
pub(crate) fn resolve_domain(domain: Option<String>) -> Option<String> {
    Some(
        domain
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "general".to_string()),
    )
}

pub(crate) fn is_lazy_source(source: &str) -> bool {
    matches!(source, "extraction" | "auto" | "ingest_event")
}

pub(crate) fn should_enqueue_enrichment(entry: &MemoryEntry) -> bool {
    // Raw-tier memories skip LLM embedding — deferred to post-distillation pass.
    if entry.tier.eq_ignore_ascii_case("raw") {
        return false;
    }
    true
}

fn days_since(timestamp: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| (Utc::now() - dt.with_timezone(&Utc)).num_seconds().max(0) as f64 / 86_400.0)
        .unwrap_or(0.0)
}

/// Calculate whether an accessed ephemeral memory should be promoted.
pub(crate) fn calculate_promotion_score(entry: &MemoryEntry, access_days: usize) -> f64 {
    let frequency = (access_days as f64).ln_1p() / 6.0_f64.ln_1p();
    let age_days = days_since(&entry.timestamp);
    let recency = (-0.693 * age_days / 14.0).exp();
    let conceptual = (entry.keywords.len() as f64 / 5.0).min(1.0);

    (frequency * 0.30 + recency * 0.25 + entry.importance * 0.25 + conceptual * 0.20)
        .clamp(0.0, 1.0)
}

pub(crate) fn default_source_path_prefix(
    source_url: Option<&str>,
    source: Option<&str>,
    domain: Option<&str>,
) -> String {
    let domain_part = domain.unwrap_or("general");
    let source_hint = source
        .filter(|value| !value.trim().is_empty())
        .or(source_url)
        .unwrap_or("source");
    format!(
        "/wiki/{}/{}",
        sanitize_safe_path_name(domain_part),
        sanitize_safe_path_name(source_hint)
    )
}

pub(crate) fn default_event_path_prefix(
    domain: Option<&str>,
    event_type: Option<&str>,
    payload: Option<&serde_json::Value>,
    conversation_id: &str,
) -> String {
    if matches!(domain, Some("trading")) {
        if let Some(ticker) = payload
            .and_then(|value| value.get("ticker"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
        {
            return format!("/trading/journal/{}", sanitize_safe_path_name(ticker));
        }
        return "/trading/journal".to_string();
    }

    if !conversation_id.trim().is_empty() {
        return format!(
            "/events/{}",
            sanitize_safe_path_name(conversation_id.trim())
        );
    }

    format!(
        "/events/{}",
        sanitize_safe_path_name(event_type.unwrap_or("event"))
    )
}

pub(crate) fn chunk_text(
    content: &str,
    chunk_size_chars: usize,
    chunk_overlap_chars: usize,
) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let chunk_size = chunk_size_chars.max(1);
    let overlap = chunk_overlap_chars.min(chunk_size.saturating_sub(1));
    let step = chunk_size.saturating_sub(overlap).max(1);

    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + chunk_size).min(chars.len());
        let chunk: String = chars[start..end].iter().collect();
        let trimmed = chunk.trim();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        if end >= chars.len() {
            break;
        }
        start += step;
    }

    if chunks.is_empty() {
        chunks.push(content.trim().to_string());
    }
    chunks
}

pub(crate) fn build_ingest_entry(
    id: String,
    path: String,
    text: String,
    importance: f64,
    source: String,
    scope: String,
    metadata: serde_json::Value,
    retention_policy: Option<String>,
    domain: Option<String>,
    needs_summary: bool,
) -> MemoryEntry {
    let summary = if needs_summary {
        String::new()
    } else {
        summary_from_text(&text)
    };
    let importance = importance.clamp(0.0, 1.0);
    let retention_policy = if is_lazy_source(&source) && importance < 0.5 {
        Some("ephemeral".to_string())
    } else {
        retention_policy
    };

    MemoryEntry {
        id,
        path: path.clone(),
        summary,
        text,
        importance,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: topic_from_path(&path),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source,
        scope,
        archived: false,
        access_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy,
        domain,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{build_ingest_entry, calculate_promotion_score, resolve_domain};
    use memcore::db::AccessEventKind;
    use memcore::{MemoryEntry, MemoryStore, RecallConfig};
    use serde_json::json;

    /// The literal `daily_pipeline/maintenance.rs` compares
    /// `calculate_promotion_score` against before calling
    /// `promote_memory_to_durable`. Duplicated here rather than extracted into
    /// a shared constant: tachi#1446 lever 6 is scoped to what *feeds* the
    /// gate, and moving the threshold — even to an identical value in a new
    /// place — is a separate judgement with its own evidence requirement.
    const PROMOTION_THRESHOLD: f64 = 0.60;

    fn promotion_fixture(id: &str) -> MemoryEntry {
        let mut entry = build_ingest_entry(
            id.to_string(),
            "/test/promotion".to_string(),
            "a memory the recall pipeline keeps showing".to_string(),
            0.2,
            "test".to_string(),
            "general".to_string(),
            json!({}),
            None,
            None,
            false,
        );
        // `conceptual` saturates at five keywords; pinned so the only term that
        // moves between the two arms below is `frequency`.
        entry.keywords = ["alpha", "beta", "gamma", "delta", "epsilon"]
            .into_iter()
            .map(str::to_string)
            .collect();
        entry
    }

    fn seed_display_days(store: &MemoryStore, id: &str, days: &[&str]) {
        for accessed_at in days {
            store
                .connection()
                .execute(
                    "INSERT INTO access_history (memory_id, accessed_at, query_hash, event_kind)
                     VALUES (?1, ?2, 'q', 'display')",
                    [id, *accessed_at],
                )
                .expect("insert display access row");
        }
    }

    const SIX_EXPOSURE_DAYS: [&str; 6] = [
        "2026-07-01T08:00:00Z",
        "2026-07-02T08:00:00Z",
        "2026-07-03T08:00:00Z",
        "2026-07-04T08:00:00Z",
        "2026-07-05T08:00:00Z",
        "2026-07-06T08:00:00Z",
    ];

    /// tachi#1446 lever 6 — the durable-promotion ratchet, end to end.
    ///
    /// A memory that was displayed on six distinct days and used on none of
    /// them clears the 0.60 promotion gate today. `promote_memory_to_durable`
    /// pins `importance = 0.7` and `retention_policy = 'durable'` with no
    /// inverse operation, so that is the system permanently rewarding a
    /// memory for having been shown. With `use_provenance_recency` on, the
    /// same six exposures contribute zero promotion days and the gate holds.
    #[test]
    fn exposure_alone_cannot_ratchet_a_memory_to_durable_with_the_use_knob_on() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let entry = promotion_fixture("shown-often");
        store.upsert(&entry).expect("seed entry");
        seed_display_days(&store, &entry.id, &SIX_EXPOSURE_DAYS);

        let knob_off = RecallConfig::default();
        let knob_on = RecallConfig {
            use_provenance_recency: true,
            ..RecallConfig::default()
        };

        let exposure_days = store
            .distinct_access_days(&entry.id, AccessEventKind::for_promotion(&knob_off))
            .expect("count days at default config");
        let use_days = store
            .distinct_access_days(&entry.id, AccessEventKind::for_promotion(&knob_on))
            .expect("count days with the knob on");

        assert_eq!(exposure_days, 6);
        assert_eq!(use_days, 0, "nobody used this memory; it was only shown");

        let score_from_exposure = calculate_promotion_score(&entry, exposure_days);
        let score_from_use = calculate_promotion_score(&entry, use_days);

        assert!(
            score_from_exposure >= PROMOTION_THRESHOLD,
            "the defect must still be reachable at default config or this test \
             is not testing anything: got {score_from_exposure}"
        );
        assert!(
            score_from_use < PROMOTION_THRESHOLD,
            "with use provenance on, six displays must not be able to promote \
             a memory to durable: got {score_from_use}"
        );
    }

    /// tachi#1446 lever 6, the property that makes the change safe to land:
    /// at default config the score the gate sees is bit-for-bit what it was
    /// before the `event_kind` split.
    ///
    /// The reference value is computed from the literal pre-#1446 unfiltered
    /// statement, not from a constant, so this fails if the filtered query ever
    /// stops covering the legacy row set.
    #[test]
    fn default_config_promotion_score_is_unchanged_by_the_event_kind_split() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let entry = promotion_fixture("legacy-history");
        store.upsert(&entry).expect("seed entry");
        seed_display_days(&store, &entry.id, &SIX_EXPOSURE_DAYS);

        let pre_1446_days: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(DISTINCT date(accessed_at)) FROM access_history WHERE memory_id = ?1",
                [entry.id.as_str()],
                |row| row.get(0),
            )
            .expect("unfiltered count");
        let knob_off = RecallConfig::default();
        let default_days = store
            .distinct_access_days(&entry.id, AccessEventKind::for_promotion(&knob_off))
            .expect("count days at default config");

        assert_eq!(
            default_days, pre_1446_days as usize,
            "knob OFF must feed the gate the same day count the unfiltered \
             query fed it"
        );
        // Bit-pattern equality, not `==` on f64: "byte-identical" is the
        // literal claim being tested, and it dodges the float-comparison lint
        // without weakening the assertion to a tolerance.
        assert_eq!(
            calculate_promotion_score(&entry, default_days).to_bits(),
            calculate_promotion_score(&entry, pre_1446_days as usize).to_bits(),
            "and therefore the same score, and therefore the same promotion \
             decision"
        );
    }

    #[test]
    fn explicit_domain_wins() {
        assert_eq!(
            resolve_domain(Some("engineering".to_string())),
            Some("engineering".to_string())
        );
    }

    #[test]
    fn blank_or_absent_domain_falls_back_to_general_never_tachi_domain_env() {
        // #1041 S2 regression: absent domain must land on `general`, never
        // silently inherit the process-wide `TACHI_DOMAIN` env var (which is
        // how an engineering row got double-mislabeled `equity_trading` on a
        // trading daemon). Set TACHI_DOMAIN to prove it's genuinely ignored.
        let guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("TACHI_DOMAIN");
        std::env::set_var("TACHI_DOMAIN", "equity_trading");

        assert_eq!(resolve_domain(None), Some("general".to_string()));
        assert_eq!(
            resolve_domain(Some("   ".to_string())),
            Some("general".to_string())
        );

        match previous {
            Some(value) => std::env::set_var("TACHI_DOMAIN", value),
            None => std::env::remove_var("TACHI_DOMAIN"),
        }
        drop(guard);
    }
}
