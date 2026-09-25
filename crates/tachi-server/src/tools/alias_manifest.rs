//! Machine-readable registry of legacy tool names folded into canonical verb
//! facades (#757). Each entry records where a legacy MCP tool now lives
//! (canonical verb + action), when the alias was introduced, and the release
//! in which it must be removed — so a version tripwire test can turn
//! "we shipped past the removal release but the zombie alias is still routed"
//! into a red test instead of a silent stale surface.
//!
//! S1 (Cut3) registered the six folded sandbox tools. Their `remove_in_release`
//! deadline (1.10.0) was reached at v2.0.0 and the six routes were deleted, so
//! the S1 entries are now **tombstones** ([`AliasKind::RetiredAlias`]): the
//! mapping, lifecycle, and admin/destructive classification stay on record
//! with their original deadlines (never bumped), and the router tripwires pin
//! that the retired names never come back. Later cuts (S2–S7) append their own
//! folds to [`ALIAS_MANIFEST`] as live forwarding-alias entries (re-introducing
//! the `ForwardingAlias` kind in the same change, since a kind with no
//! constructor is dead code this module refuses to carry); the tripwire and
//! router-coverage tests iterate the whole manifest, so a new fold gets the
//! same guarantees for free once its entries are added here.

/// How a legacy name relates to its canonical replacement.
///
/// v2 note: a `ForwardingAlias` variant existed while the six sandbox
/// aliases were still routed; with every manifest entry now a tombstone it
/// had no constructor left (dead-code, and test builds deliberately keep the
/// lint strict here) and was removed. An S2–S7 fold that registers a LIVE
/// forwarding alias re-introduces the variant in the same change that adds
/// its entries, together with the routed-with-deprecation-prefix branch of
/// the lifecycle gate in `tests/sandbox_fold.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AliasKind {
    /// Legacy tool name that reached its `remove_in_release` deadline and has
    /// been deleted from the router. The entry survives as a tombstone: the
    /// mapping, deadline (unchanged from its live span), and admin/destructive
    /// classification stay on record so the lifecycle tripwires can pin that
    /// the name stays unrouted — and rejected even for the admin profile —
    /// forever. Re-introducing the name requires a brand-new manifest entry
    /// with a fresh lifecycle, not a kind flip back.
    RetiredAlias,
}

/// One legacy → canonical mapping with its deprecation lifecycle.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AliasEntry {
    /// The legacy MCP tool name still registered on the router.
    pub legacy_name: &'static str,
    /// Relationship of the legacy name to its canonical replacement.
    pub alias_kind: AliasKind,
    /// Canonical verb the legacy name now forwards into.
    pub canonical_tool: &'static str,
    /// Canonical `action=` selector on `canonical_tool`.
    pub canonical_action: &'static str,
    /// Release (semver `X.Y.Z`) in which this alias was introduced.
    pub introduced_release: &'static str,
    /// Release (semver `X.Y.Z`) in which this alias MUST be removed. Once the
    /// running `CARGO_PKG_VERSION` reaches this version the alias may no longer
    /// be routed (enforced by the version tripwire test).
    pub remove_in_release: &'static str,
    /// Whether the legacy tool was admin-only pre-fold (it must stay so).
    pub admin_only: bool,
    /// Whether the underlying operation mutates state.
    pub destructive: bool,
}

impl AliasEntry {
    /// The `DEPRECATED: …` prefix a folded alias must carry in its
    /// `tools/list` description while it is still routed. Kept here so the
    /// manifest is the single source of the wording the tripwire test
    /// cross-checks against the live router; for [`AliasKind::RetiredAlias`]
    /// tombstones it remains the record of what the route's description said
    /// (and what callers must migrate to).
    pub(crate) fn deprecation_prefix(&self) -> String {
        format!(
            "DEPRECATED: use {}(action='{}'); removed in {}.",
            self.canonical_tool, self.canonical_action, self.remove_in_release
        )
    }
}

/// The current package version (`CARGO_PKG_VERSION`), i.e. the release the
/// running binary reports. Split out so the tripwire test can compare it
/// against each entry's `remove_in_release`.
pub(crate) const CURRENT_RELEASE: &str = env!("CARGO_PKG_VERSION");

/// #757 Cut3-S1: the six sandbox tools folded into the `tachi_sandbox` verb.
/// All were admin-only pre-fold and the fold preserved that (tool-level
/// visibility: `tachi_sandbox` is absent from every profile bundle, so only
/// the admin profile can see or call it).
///
/// v2.0: the aliases' `remove_in_release` deadline (1.10.0) was reached and
/// the six routes were deleted, so every entry below is a
/// [`AliasKind::RetiredAlias`] tombstone. Deadlines are deliberately UNCHANGED
/// from their live span — bumping `remove_in_release` on a tombstone would
/// rewrite history and reopen the zombie window the tripwires exist to close.
const SANDBOX_ALIASES: &[AliasEntry] = &[
    AliasEntry {
        legacy_name: "sandbox_set_rule",
        alias_kind: AliasKind::RetiredAlias,
        canonical_tool: "tachi_sandbox",
        canonical_action: "set_rule",
        introduced_release: "1.9.0",
        remove_in_release: "1.10.0",
        admin_only: true,
        destructive: true,
    },
    AliasEntry {
        legacy_name: "sandbox_check",
        alias_kind: AliasKind::RetiredAlias,
        canonical_tool: "tachi_sandbox",
        canonical_action: "check",
        introduced_release: "1.9.0",
        remove_in_release: "1.10.0",
        admin_only: true,
        destructive: false,
    },
    AliasEntry {
        legacy_name: "sandbox_set_policy",
        alias_kind: AliasKind::RetiredAlias,
        canonical_tool: "tachi_sandbox",
        canonical_action: "set_policy",
        introduced_release: "1.9.0",
        remove_in_release: "1.10.0",
        admin_only: true,
        destructive: true,
    },
    AliasEntry {
        legacy_name: "sandbox_get_policy",
        alias_kind: AliasKind::RetiredAlias,
        canonical_tool: "tachi_sandbox",
        canonical_action: "get_policy",
        introduced_release: "1.9.0",
        remove_in_release: "1.10.0",
        admin_only: true,
        destructive: false,
    },
    AliasEntry {
        legacy_name: "sandbox_list_policies",
        alias_kind: AliasKind::RetiredAlias,
        canonical_tool: "tachi_sandbox",
        canonical_action: "list_policies",
        introduced_release: "1.9.0",
        remove_in_release: "1.10.0",
        admin_only: true,
        destructive: false,
    },
    AliasEntry {
        legacy_name: "sandbox_exec_audit",
        alias_kind: AliasKind::RetiredAlias,
        canonical_tool: "tachi_sandbox",
        canonical_action: "exec_audit",
        introduced_release: "1.9.0",
        remove_in_release: "1.10.0",
        admin_only: true,
        destructive: false,
    },
];

/// Every registered fold entry across all cuts — live forwarding aliases AND
/// retired tombstones. Later cuts extend this by appending their own slice.
pub(crate) const ALIAS_MANIFEST: &[&[AliasEntry]] = &[SANDBOX_ALIASES];

/// Flattened view of [`ALIAS_MANIFEST`].
pub(crate) fn all_aliases() -> impl Iterator<Item = &'static AliasEntry> {
    ALIAS_MANIFEST.iter().flat_map(|group| group.iter())
}

/// Look up a manifest entry by its legacy tool name.
pub(crate) fn find_alias(legacy_name: &str) -> Option<&'static AliasEntry> {
    all_aliases().find(|entry| entry.legacy_name == legacy_name)
}

/// Parse a `major.minor.patch` semver core (any pre-release/build suffix is
/// ignored) into an ordered tuple for comparison. Returns `None` if the string
/// is not at least `major.minor.patch` of integers.
pub(crate) fn parse_release(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Whether `current` is at or past `target` (semver core comparison). Used by
/// the tripwire: an alias whose `remove_in_release` has been reached must no
/// longer be routed.
pub(crate) fn release_at_or_past(current: &str, target: &str) -> bool {
    match (parse_release(current), parse_release(target)) {
        (Some(cur), Some(tgt)) => cur >= tgt,
        // A malformed version string is a manifest bug; fail loud in the caller
        // rather than silently treating the alias as still-permitted.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_release_reads_core_and_ignores_suffix() {
        assert_eq!(parse_release("1.9.0"), Some((1, 9, 0)));
        assert_eq!(parse_release("1.10.0"), Some((1, 10, 0)));
        assert_eq!(parse_release("2.0.3-rc.1"), Some((2, 0, 3)));
        assert_eq!(parse_release("nonsense"), None);
    }

    #[test]
    fn release_ordering_is_numeric_not_lexicographic() {
        // The classic string-compare trap: "1.9.0" > "1.10.0" lexically.
        assert!(!release_at_or_past("1.9.0", "1.10.0"));
        assert!(release_at_or_past("1.10.0", "1.10.0"));
        assert!(release_at_or_past("1.11.0", "1.10.0"));
        assert!(release_at_or_past("2.0.0", "1.10.0"));
    }

    #[test]
    fn deprecation_prefix_names_canonical_action_and_removal() {
        let entry = find_alias("sandbox_check").expect("sandbox_check registered");
        let prefix = entry.deprecation_prefix();
        assert!(prefix.starts_with("DEPRECATED: use tachi_sandbox(action='check')"));
        assert!(prefix.contains("removed in 1.10.0"));
    }
}
