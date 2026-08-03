//! tachi-server's `KernelPolicy` adapter (tachi#1585 D5).
//!
//! `memcore::KernelPolicy::default()` is deliberately pure — no env, no
//! process-wide state (see `memcore::kernel_policy` module docs). This
//! module is the ONE place in tachi-server that resolves `TACHI_*` env /
//! `config.env` and turns that resolution into a `KernelPolicy`, so no
//! individual `MemoryStore::open*` call site has to know about env at all.
//!
//! [`resolve_kernel_policy`] is byte-identical in env precedence to the
//! pre-#1585 ambient-read behavior it replaces:
//! - `recall` delegates to `memcore::RecallConfig::load()` — the exact
//!   function the old `RecallConfig::get()` `OnceLock` called to populate
//!   itself; this adapter changes *who* calls it and *when* (once, here,
//!   instead of lazily on first process-wide access), not what it resolves.
//! - `embed.raw_tier_enabled` delegates to `memcore::embed_raw_tier_enabled()`
//!   (`TACHI_EMBED_RAW_TIER`), unchanged.
//! - `path_validation_escape_hatch` delegates to
//!   `memcore::kernel_policy::path_validation_escape_hatch_from_env()`
//!   (`TACHI_DISABLE_PATH_VALIDATION`), which replaced two byte-identical
//!   env-reading free functions memcore used to carry.
//! - `TACHI_MIGRATION_BACKUP_RETAIN` isn't a `KernelPolicy` field (its one
//!   call site fires before any store — and therefore any policy — exists);
//!   this function injects it as a side effect via
//!   `memcore::kernel_policy::set_migration_backup_retain`.
//!
//! Call [`resolve_kernel_policy`] once, early in process startup, before any
//! store opens.
//!
//! ## Known gap (tachi#1585 D5, this lane)
//!
//! Building this adapter is this lane's D5 deliverable; *wiring* it into
//! tachi-server's `MemoryStore::open*` call sites (the global-store and
//! per-project funnels in `memory-server-runtime`, plus ~200 direct call
//! sites elsewhere in this crate) is NOT done here — those funnels are
//! actively owned by a parallel #1585 lane (D1/D2 identity + profile
//! threading) and touching them here would collide. Until that wiring
//! lands, every `MemoryStore` this crate opens carries the pure default
//! policy, not this adapter's resolution — including
//! `TACHI_DISABLE_PATH_VALIDATION`, which several existing test fixtures
//! (`tests/mod.rs::ensure_test_env`,
//! `tests/profile_tests/tool_profile_router_coverage.rs::ensure_test_env`)
//! set expecting ambient, store-independent effect. That expectation no
//! longer holds for any store not explicitly given this adapter's
//! `KernelPolicy` via `MemoryStore::with_kernel_policy`.

use std::sync::Arc;

use memcore::{DefaultDecayPolicy, EmbedPolicy, KernelPolicy};

/// Resolve this process's `TACHI_*` env / `config.env` once and build the
/// `KernelPolicy` its stores should use. See module docs for the exact
/// env-var/precedence mapping and the known wiring gap.
pub fn resolve_kernel_policy() -> KernelPolicy {
    if let Some(retain) = migration_backup_retain_from_env() {
        memcore::kernel_policy::set_migration_backup_retain(retain);
    }

    KernelPolicy {
        recall: memcore::RecallConfig::load(),
        decay: Arc::new(DefaultDecayPolicy),
        embed: EmbedPolicy {
            raw_tier_enabled: memcore::embed_raw_tier_enabled(),
        },
        path_validation_escape_hatch: memcore::kernel_policy::path_validation_escape_hatch_from_env(
        ),
    }
}

fn migration_backup_retain_from_env() -> Option<usize> {
    std::env::var("TACHI_MIGRATION_BACKUP_RETAIN")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvVarSnapshotRestore {
        key: &'static str,
        saved: Option<std::ffi::OsString>,
    }

    impl EnvVarSnapshotRestore {
        fn capture_and_clear(key: &'static str) -> Self {
            let saved = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, saved }
        }
    }

    impl Drop for EnvVarSnapshotRestore {
        fn drop(&mut self) {
            match &self.saved {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    // tachi-server's test binary runs hundreds of tests concurrently, many of
    // which call `crate::tests::ensure_test_env()` — a `std::sync::Once` that
    // sets `TACHI_TEST_DISABLE_RECALL_CONFIG=1` and `TACHI_DISABLE_PATH_VALIDATION=1`
    // process-wide, permanently, the first time ANY test calls it, with no
    // lock coordinating against concurrently-running tests. That makes
    // "confirm X is unset/default" assertions for those two keys genuinely
    // racy in this binary (a concurrent test's `Once` can fire mid-assertion)
    // — so the tests below either (a) compare the adapter's output against a
    // same-moment direct call to the function it delegates to (robust to
    // ambient pollution because both sides observe identical state), or (b)
    // assert only the "honored when explicitly set" direction using the one
    // value (`"1"`) every existing caller in this suite ever sets these keys
    // to, so a concurrent writer can only agree with, never contradict, this
    // test's own `set_var`. The "unset -> pure default" direction is instead
    // pinned deterministically in `memcore::kernel_policy`'s own tests, which
    // run in memcore's isolated test binary with no such pollution.

    #[test]
    fn adapter_recall_config_delegates_to_recall_config_load() {
        // Same-moment comparison: whatever ambient env/config.env state this
        // process is in, both calls observe it identically, so this is
        // deterministic regardless of what concurrently-running tests have
        // done to recall-config env vars.
        let policy = resolve_kernel_policy();
        let direct = memcore::RecallConfig::load();
        assert_eq!(
            policy.recall, direct,
            "adapter's KernelPolicy.recall must be exactly RecallConfig::load()'s \
             result — the adapter changes who calls it and when, not what it resolves"
        );
    }

    #[test]
    fn adapter_embed_raw_tier_honors_env() {
        // TACHI_EMBED_RAW_TIER has no other reader/writer anywhere in this
        // crate's test suite (unlike the two keys called out above), so the
        // unset -> on -> off round trip is safe here.
        let _restore = EnvVarSnapshotRestore::capture_and_clear("TACHI_EMBED_RAW_TIER");

        let policy = resolve_kernel_policy();
        assert!(
            policy.embed.raw_tier_enabled,
            "unset env: pure default matches the historical TACHI_EMBED_RAW_TIER \
             -unset behavior (on)"
        );

        std::env::set_var("TACHI_EMBED_RAW_TIER", "0");
        let policy = resolve_kernel_policy();
        assert!(
            !policy.embed.raw_tier_enabled,
            "adapter must honor TACHI_EMBED_RAW_TIER=0, matching the pre-#1585 \
             embed_raw_tier_enabled() read"
        );

        std::env::set_var("TACHI_EMBED_RAW_TIER", "1");
        let policy = resolve_kernel_policy();
        assert!(policy.embed.raw_tier_enabled);
    }

    #[test]
    fn adapter_path_validation_escape_hatch_honors_env_when_set() {
        std::env::set_var("TACHI_DISABLE_PATH_VALIDATION", "1");
        let policy = resolve_kernel_policy();
        assert!(
            policy.path_validation_escape_hatch,
            "set env: adapter must honor TACHI_DISABLE_PATH_VALIDATION=1, matching \
             the pre-#1585 duplicated env reads it replaced"
        );
        // Deliberately not restored: every other caller of this env var in
        // this crate's test suite also sets it to "1" and never unsets it
        // (see module docs), so leaving it set matches this binary's
        // established convention instead of racing a concurrent test that
        // expects it to still be "1".
    }
}
