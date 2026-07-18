//! Embedding backfill tuning (vector sweep / maintenance selection).

/// Whether raw-tier memories are eligible for vector embedding backfill.
///
/// Default **on** (`TACHI_EMBED_RAW_TIER` unset, `"1"`, or `"true"`).
/// Set `"0"` or `"false"` to restore the pre-#1242 predicate that excluded raw.
pub fn embed_raw_tier_enabled() -> bool {
    !matches!(
        std::env::var("TACHI_EMBED_RAW_TIER")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("0")
            | Some("false")
            | Some("FALSE")
            | Some("False")
            | Some("no")
            | Some("NO")
            | Some("off")
            | Some("OFF")
    )
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
    }

    #[test]
    fn embed_raw_tier_accepts_truthy_sentinels() {
        let _restore = EnvVarSnapshotRestore::capture_and_clear("TACHI_EMBED_RAW_TIER");
        std::env::set_var("TACHI_EMBED_RAW_TIER", "1");
        assert!(embed_raw_tier_enabled());
        std::env::set_var("TACHI_EMBED_RAW_TIER", "true");
        assert!(embed_raw_tier_enabled());
    }
}
