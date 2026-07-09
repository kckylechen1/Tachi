//! DecayPolicy resolution for the portable server (tachi #791 hook).
//!
//! The kernel owns the scorer call site and exposes a `DecayPolicy` trait plus
//! `SearchOptions::decay_policy` (`Arc<dyn DecayPolicy>`) as the injection
//! point. This module turns a config string into an `Option<Arc<dyn
//! DecayPolicy>>` that the search handler feeds into `SearchOptions`, so
//! downstream products (A-share half-lives, chat affect decay) re-land as
//! *policy configuration on this binary* instead of fork patches to the kernel.
//!
//! - `default` -> `None` -> kernel uses `DEFAULT_DECAY_POLICY` (unchanged behavior).
//! - `flat`    -> a bin-local no-time-decay policy, kept out of the kernel per
//!   the #924 non-goal ("no trading semantics in the kernel"). It exists here to
//!   prove the hook is really wired and injectable; real downstream policies
//!   plug in the same way without touching memcore.

use std::sync::Arc;

use portable_kernel::{DecayPolicy, MemoryEntry, RecallConfig};

/// A demonstration policy that removes time decay entirely: recall strength is
/// governed only by `importance`. Not a product policy — it is the concrete
/// evidence that the #791 injection point reaches this binary's config.
#[derive(Debug, Clone, Copy, Default)]
pub struct FlatDecayPolicy;

impl DecayPolicy for FlatDecayPolicy {
    fn score_decay(
        &self,
        entry: &MemoryEntry,
        _recall_config: &RecallConfig,
        _access_ages: Option<&[f64]>,
    ) -> f64 {
        entry.importance.clamp(0.0, 1.0)
    }
}

/// Resolve a policy name (from `--decay-policy` / `PORTABLE_DECAY_POLICY`) into
/// an optional injected policy. `None` means "use the kernel default"
/// (current behavior), which is what `default`/unset selects.
pub fn resolve(name: &str) -> Result<Option<Arc<dyn DecayPolicy>>, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "" | "default" => Ok(None),
        "flat" => Ok(Some(Arc::new(FlatDecayPolicy))),
        other => Err(format!(
            "unknown decay policy '{other}' (known: default, flat)"
        )),
    }
}
