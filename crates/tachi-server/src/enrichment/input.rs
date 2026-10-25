use super::EnrichmentItem;

pub(super) fn embedding_input_for_item(
    item: &EnrichmentItem,
    generated_summary: Option<&str>,
) -> String {
    if item.text.len() <= 500 {
        return item.text.clone();
    }

    let summary = generated_summary
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(item.summary.as_str())
        .trim();
    let keywords = item.keywords.join(", ");
    let entities = item.entities.join(", ");
    let condensed = format!("{summary}\n{keywords}\n{entities}")
        .trim()
        .to_string();
    if condensed.is_empty() {
        item.text.clone()
    } else if item.auto_chunked {
        // A parent summary cannot replace the child's unique facts.
        format!("{}\n{condensed}", item.text)
    } else {
        condensed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evidence_repair_ordinary_long_records_keep_condensed_input() {
        let mut entry = crate::tests::make_entry("ordinary-long");
        entry.text = "ordinary source ".repeat(10000);
        entry.summary = "short summary".into();
        entry.keywords = vec!["tag".into()];
        entry.entities = vec![];
        let item = crate::enrichment::build_enrichment_item(
            &entry,
            true,
            false,
            crate::DbScope::Global,
            None,
            None,
            None,
            None,
            1,
        );
        assert!(!item.auto_chunked);
        assert_eq!(embedding_input_for_item(&item, None), "short summary\ntag");
        assert_eq!(
            embedding_input_for_item(&item, Some("generated summary")),
            "generated summary\ntag"
        );
    }
    #[test]
    fn evidence_repair_shared_summary_keeps_distinct_chunk_facts_in_embedding() {
        let mut entry = crate::tests::make_entry("embedding-chunk");
        entry.summary = "Shared parent summary".into();
        entry.metadata = serde_json::json!({"auto_chunked": true});
        entry.keywords = vec!["shared".into()];
        let mut inputs = Vec::new();
        for unique in ["bluewater", "redmountain"] {
            entry.text = format!("{} {unique}", "shared words ".repeat(150));
            let item = crate::enrichment::build_enrichment_item(
                &entry,
                true,
                false,
                crate::DbScope::Global,
                None,
                None,
                None,
                None,
                1,
            );
            let input = embedding_input_for_item(&item, None);
            assert!(input.contains(unique));
            assert!(input.contains("Shared parent summary"));
            assert!(input.starts_with(&entry.text));
            inputs.push(input);
        }
        assert_ne!(inputs[0], inputs[1]);
    }
}
