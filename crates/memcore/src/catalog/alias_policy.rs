//! The alias set as a *policy*: its content-addressed revision, and the
//! snapshot the operational resolver resolves a `ModelRef` against (tachi#1681
//! D2/D5).
//!
//! # Why the revision is a content digest and not a counter
//!
//! `model_aliases.revision` is a per-row counter, which answers "did this one
//! alias move". The question a resolution has to answer is different: "is the
//! alias set I am resolving against the same alias set this `ModelRef` was
//! minted under". A counter cannot answer that — a binding added to a
//! *different* alias changes routing for nobody's counter — so the revision
//! stamped on a `ModelRef` is a canonical-JSON content digest over the whole
//! active set, exactly the [`route_policy_source_revision`] mechanism #1675
//! PR1 already reuses rather than a second invention.
//!
//! [`route_policy_source_revision`]: https://github.com/kckylechen1/tachi/issues/1675
//!
//! Three properties fall out of that choice, and all three are load-bearing:
//!
//! - **Third-party-write-safe.** Another process rebinding an alias moves the
//!   digest, so an approval (or a `ModelRef`) minted against the earlier set
//!   is detectably stale — [`crate::catalog::resolver`] abstains with
//!   `policy_revision_mismatch` rather than routing against a set nobody
//!   reviewed.
//! - **Row revisions are inside the digest**, not just the content. A write
//!   that lands the same JSON still advances each row's counter, and the
//!   digest carries the counters, so "the bytes came back to where they were"
//!   is still a different policy state than "nothing happened".
//! - **Only the active set counts.** A retired alias or a retired binding is
//!   history, not policy. Retiring one therefore moves the digest by leaving
//!   the set, which is the same signal as any other change.

use serde_json::{json, Value};

use crate::canonical_digest::canonical_json_digest_hex;

use super::{ModelAlias, ModelAliasBinding, ALIAS_STATUS_ACTIVE};

/// Scheme prefix of an alias-set policy revision: `ar1:<hex64>`. Versioned in
/// the value so a later canonicalization change is visibly a different scheme
/// rather than a silently different hash of the same shape (the
/// `PRICING_SNAPSHOT_SCHEME` / `PLAN_DIGEST_SCHEME` precedent).
pub const ALIAS_POLICY_REVISION_SCHEME: &str = "ar1";

/// One deployment an alias may resolve to, and where it sits in the operator's
/// stated preference order.
///
/// `priority` is the operator's *intent*, recorded so it can be reviewed. It
/// is deliberately **not** a resolver ordering axis: D5 freezes that order as
/// `pin > health > price > deployment_id`, and letting a hand-written priority
/// outrank health or price would let an alias edit silently re-route around a
/// cooling deployment. Priority decides candidacy and reads back in a plan; it
/// does not decide the winner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasBindingEntry {
    pub deployment_id: String,
    pub priority: i64,
}

/// One alias and every deployment it is actively bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasEntry {
    pub alias_name: String,
    /// Sorted by `(priority, deployment_id)` so the snapshot has one shape for
    /// a given set of rows regardless of the order the store returned them.
    pub bindings: Vec<AliasBindingEntry>,
}

/// The active alias set at one instant, plus the revision that identifies it.
///
/// Built through [`AliasSetSnapshot::from_rows`] rather than assembled by
/// callers: the revision must be *computed from* the rows in the snapshot, and
/// a public struct literal would let a caller pair one set's rows with another
/// set's revision — which is precisely the drift the revision exists to catch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasSetSnapshot {
    policy_revision: String,
    aliases: Vec<AliasEntry>,
}

impl AliasSetSnapshot {
    /// Project stored rows into the active alias set and stamp its revision.
    ///
    /// Retired aliases and retired bindings are dropped: they are history, not
    /// policy. A binding whose alias row is absent or retired is dropped with
    /// it — a binding is a statement *about* an alias, and an orphan cannot
    /// route.
    pub fn from_rows(aliases: &[ModelAlias], bindings: &[ModelAliasBinding]) -> Self {
        let mut entries: Vec<AliasEntry> = aliases
            .iter()
            .filter(|alias| alias.status == ALIAS_STATUS_ACTIVE)
            .map(|alias| AliasEntry {
                alias_name: alias.alias_name.clone(),
                bindings: active_bindings_for(&alias.alias_name, bindings),
            })
            .collect();
        entries.sort_by(|left, right| left.alias_name.cmp(&right.alias_name));

        let policy_revision = alias_set_policy_revision(aliases, bindings);
        Self {
            policy_revision,
            aliases: entries,
        }
    }

    /// An empty alias set, stamped with the revision an empty set has.
    ///
    /// Not a `Default` impl: an empty *set* is a legitimate policy state (no
    /// alias is bound yet) and its revision is a real digest, but reaching it
    /// by `..Default::default()` in a struct update would be the exact
    /// rows/revision mismatch the private fields exist to prevent.
    pub fn empty() -> Self {
        Self::from_rows(&[], &[])
    }

    /// The content-addressed revision of this set.
    pub fn policy_revision(&self) -> &str {
        &self.policy_revision
    }

    /// Every active alias, sorted by name.
    pub fn aliases(&self) -> &[AliasEntry] {
        &self.aliases
    }

    /// The alias of that exact name, if the set binds one.
    pub fn get(&self, alias_name: &str) -> Option<&AliasEntry> {
        self.aliases
            .iter()
            .find(|entry| entry.alias_name == alias_name)
    }
}

/// `ar1:<hex64>` over the active alias set.
///
/// Free-standing as well as reachable through [`AliasSetSnapshot`] because the
/// plan/apply path (#1681 D2) needs to recompute it from rows it is holding
/// inside a write transaction, where building a snapshot would be ceremony.
///
/// `required_capabilities` and `constraints` are stored as JSON *text*. They
/// are parsed here so that two byte-different spellings of the same object
/// (key order, whitespace) do not read as two different policies; text that
/// does not parse is digested as the literal string rather than dropped —
/// unreadable content is still content, and silently ignoring it would let a
/// malformed constraint edit leave the revision unmoved.
pub fn alias_set_policy_revision(aliases: &[ModelAlias], bindings: &[ModelAliasBinding]) -> String {
    let mut rows: Vec<Value> = aliases
        .iter()
        .filter(|alias| alias.status == ALIAS_STATUS_ACTIVE)
        .map(|alias| {
            let bound = active_bindings_for(&alias.alias_name, bindings);
            json!({
                "alias_name": alias.alias_name,
                "required_capabilities": parsed_or_literal(&alias.required_capabilities),
                "constraints": parsed_or_literal(&alias.constraints),
                "revision": alias.revision,
                "bindings": bound
                    .into_iter()
                    .map(|binding| json!({
                        "deployment_id": binding.deployment_id,
                        "priority": binding.priority,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    rows.sort_by(|left, right| {
        left["alias_name"]
            .as_str()
            .cmp(&right["alias_name"].as_str())
    });
    format!(
        "{ALIAS_POLICY_REVISION_SCHEME}:{}",
        canonical_json_digest_hex(&Value::Array(rows))
    )
}

/// Every active binding of one alias, in `(priority, deployment_id)` order.
///
/// One function so the snapshot and the digest cannot disagree about what "the
/// active bindings of this alias" means — a divergence there would let a
/// resolution run against one set while stamping the revision of another.
fn active_bindings_for(alias_name: &str, bindings: &[ModelAliasBinding]) -> Vec<AliasBindingEntry> {
    let mut bound: Vec<AliasBindingEntry> = bindings
        .iter()
        .filter(|binding| !binding.retired && binding.alias_name == alias_name)
        .map(|binding| AliasBindingEntry {
            deployment_id: binding.deployment_id.clone(),
            priority: binding.priority,
        })
        .collect();
    bound.sort_by(|left, right| {
        (left.priority, &left.deployment_id).cmp(&(right.priority, &right.deployment_id))
    });
    bound
}

fn parsed_or_literal(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}
