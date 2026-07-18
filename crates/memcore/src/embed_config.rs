//! Embedding backfill tuning (vector sweep / maintenance selection).
//!
//! `TACHI_EMBED_RAW_TIER` gates raw inclusion only on
//! [`crate::MemoryStore::entries_missing_vectors`] and
//! [`crate::db::list_memory_ids_needing_embedding`]. The daemon sweep path
//! ([`crate::MemoryStore::entries_missing_vectors_filtered`]) always includes
//! raw, matching pre-#1242 behavior.

/// Whether raw-tier memories are eligible for gated embedding backfill paths.
///
/// Default **on** when unset. Sentinel parsing mirrors
/// `recall_config::apply_bool` so bool envs parse identically repo-wide.
pub fn embed_raw_tier_enabled() -> bool {
    let mut enabled = true;
    if let Ok(value) = std::env::var("TACHI_EMBED_RAW_TIER") {
        // Same sentinel set as recall_config::apply_bool.
        match value.trim() {
            "1" | "true" | "TRUE" | "True" | "yes" | "YES" | "on" | "ON" => enabled = true,
            "0" | "false" | "FALSE" | "False" | "no" | "NO" | "off" | "OFF" => enabled = false,
            _ => {}
        }
    }
    enabled
}

/// SQL fragment excluding raw tier when [`embed_raw_tier_enabled`] is false.
/// `$alias` is the memories table alias (e.g. `m` or none for unaliased).
pub(crate) fn embed_raw_tier_sql_filter(table_prefix: &str) -> String {
    if embed_raw_tier_enabled() {
        String::new()
    } else {
        format!("AND {table_prefix}tier != 'raw' ")
    }
}

/// ORDER BY clause: non-raw rows first, then raw; importance DESC within each group.
pub(crate) fn embed_selection_order_by(table_prefix: &str) -> String {
    format!(
        "ORDER BY CASE WHEN {table_prefix}tier = 'raw' THEN 1 ELSE 0 END, {table_prefix}importance DESC"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvVarSnapshotRestore {
        saved: Option<std::ffi::OsString>,
    }

    impl EnvVarSnapshotRestore {
        fn capture_and_clear(key: &'static str) -> Self {
            let saved = std::env::var_os(key);
            std::env::remove_var(key);
            Self { saved }
        }
    }

    impl Drop for EnvVarSnapshotRestore {
        fn drop(&mut self) {
            match &self.saved {
                Some(v) => std::env::set_var("TACHI_EMBED_RAW_TIER", v),
                None => std::env::remove_var("TACHI_EMBED_RAW_TIER"),
            }
        }
    }

    #[test]
    fn embed_raw_tier_defaults_on_when_unset() {
        let _restore = EnvVarSnapshotRestore::capture_and_clear("TACHI_EMBED_RAW_TIER");
        assert!(embed_raw_tier_enabled());
    }

    #[test]
    fn embed_raw_tier_respects_false_sentinels() {
        let _restore = EnvVarSnapshotRestore::capture_and_clear("TACHI_EMBED_RAW_TIER");
        std::env::set_var("TACHI_EMBED_RAW_TIER", "0");
        assert!(!embed_raw_tier_enabled());
        std::env::set_var("TACHI_EMBED_RAW_TIER", "false");
        assert!(!embed_raw_tier_enabled());
        std::env::set_var("TACHI_EMBED_RAW_TIER", "OFF");
        assert!(!embed_raw_tier_enabled());
    }

    #[test]
    fn embed_raw_tier_accepts_truthy_sentinels() {
        let _restore = EnvVarSnapshotRestore::capture_and_clear("TACHI_EMBED_RAW_TIER");
        std::env::set_var("TACHI_EMBED_RAW_TIER", "1");
        assert!(embed_raw_tier_enabled());
        std::env::set_var("TACHI_EMBED_RAW_TIER", "true");
        assert!(embed_raw_tier_enabled());
        std::env::set_var("TACHI_EMBED_RAW_TIER", "yes");
        assert!(embed_raw_tier_enabled());
    }

    #[test]
    fn embed_raw_tier_unknown_value_keeps_default() {
        let _restore = EnvVarSnapshotRestore::capture_and_clear("TACHI_EMBED_RAW_TIER");
        std::env::set_var("TACHI_EMBED_RAW_TIER", "maybe");
        assert!(
            embed_raw_tier_enabled(),
            "unknown sentinel must leave the default (on), matching apply_bool"
        );
    }
}
