pub(in crate::foundry_runtime_ops) fn durable_recall_cache_enabled() -> bool {
    std::env::var("TACHI_ENABLE_DURABLE_RECALL_CACHE")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false)
}

/// Recall-cache rows are ephemeral; never write them into the user-facing wiki tree.
pub(in crate::foundry_runtime_ops::recall_cache) fn recall_cache_write_path(
    path_prefix: &str,
    cache_topic: &str,
    _named_project: Option<&str>,
) -> String {
    let prefix = path_prefix.trim_end_matches('/');
    if prefix == "/wiki" || prefix.starts_with("/wiki/") {
        return format!("/scratch/recall-cache/{cache_topic}");
    }
    format!("{prefix}/recall-cache/{cache_topic}")
}

#[cfg(test)]
mod tests {
    use super::{durable_recall_cache_enabled, recall_cache_write_path};

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn unset(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, old }
        }

        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.old {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn durable_recall_cache_is_opt_in() {
        let _guard = EnvGuard::unset("TACHI_ENABLE_DURABLE_RECALL_CACHE");
        assert!(!durable_recall_cache_enabled());

        let _guard = EnvGuard::set("TACHI_ENABLE_DURABLE_RECALL_CACHE", "1");
        assert!(durable_recall_cache_enabled());
    }

    #[test]
    fn recall_cache_path_remaps_wiki_prefix_for_non_wiki_project() {
        let p = recall_cache_write_path("/wiki/engineering", "Tachi_MCP_facade", Some("hyperion"));
        assert_eq!(p, "/scratch/recall-cache/Tachi_MCP_facade");
    }

    #[test]
    fn recall_cache_path_remaps_wiki_prefix_for_wiki_project() {
        let p = recall_cache_write_path("/wiki/engineering", "smoke", Some("wiki"));
        assert_eq!(p, "/scratch/recall-cache/smoke");
    }

    #[test]
    fn recall_cache_path_keeps_non_wiki_prefix_with_wiki_substring() {
        let p = recall_cache_write_path("/wikipedia/engineering", "smoke", Some("wiki"));
        assert_eq!(p, "/wikipedia/engineering/recall-cache/smoke");
    }

    #[test]
    fn recall_cache_path_keeps_scratch_prefix_for_sigil() {
        let p = recall_cache_write_path("/scratch/verify", "graph", Some("sigil"));
        assert_eq!(p, "/scratch/verify/recall-cache/graph");
    }
}
