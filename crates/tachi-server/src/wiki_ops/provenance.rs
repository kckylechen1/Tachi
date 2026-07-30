use super::*;

/// Applies the #1072 truthful-retrieval gate and provenance exposure to a
/// set of already-slimmed wiki search rows.
///
/// Canon doc §7 required behavior, RED case 2 ("Pending `/wiki/drafts/...`
/// currently leaks into normal search; GREEN excludes it by default"):
/// - `requested_lifecycle == None` (the default, no explicit scope): rows
///   are kept only when their derived lifecycle is `active`.
/// - `requested_lifecycle == Some("all")`: no lifecycle filtering (the
///   explicit "give me everything" scope).
/// - `requested_lifecycle == Some(<value>)` for one of the six closed
///   vocabulary values: rows are kept only when their derived lifecycle
///   equals that value exactly (e.g. `"pending_review"` to browse drafts).
///
/// Canon doc §7 required behavior, RED case 3 ("Read/search currently hide
/// authority/revision/source refs; GREEN exposes them"): every row that
/// survives the gate gets `lifecycle`, `authority`, `revision`,
/// `source_refs`, `evidence_refs_v1`, and (when present) `review_receipt`
/// attached from the full stored entry.
///
/// Cross-vendor review fix (#1215, BUG 4): rows whose `id` cannot be
/// resolved against this function's own enrichment lookup — because the
/// candidate lookup itself failed, or because the id simply isn't among the
/// (capped) candidates returned — used to be left UNFILTERED ("serve
/// everything" on a lookup miss/failure), which is a fail-OPEN gate on a
/// truthful-retrieval boundary: a DB hiccup or a >5000-row wiki silently
/// bypassed lifecycle filtering entirely. The gate now fails CLOSED: an
/// unresolved row is excluded from both the default (active-only) scope and
/// any explicit named-lifecycle scope (we cannot honestly confirm it
/// matches what was asked for), and is kept ONLY for the explicit `"all"`
/// escape hatch (`requested_lifecycle == Some("all")`) — a caller who
/// deliberately asked for "everything, unfiltered" still gets everything,
/// but every other caller loses nothing it could safely have gotten (an
/// unresolved active row that legitimately matched the requested scope was
/// never distinguishable from an unresolved draft anyway; excluding both is
/// the safe default). The candidate lookup failure itself is now propagated
/// as an error instead of silently degrading to an empty candidate set.
#[cfg(test)]
pub(crate) fn apply_wiki_lifecycle_gate(
    server: &MemoryServer,
    project: Option<&str>,
    rows: &mut Vec<Value>,
    requested_lifecycle: Option<&str>,
) -> Result<(), String> {
    let plan = WikiReadPlan::from_project(project)?;
    apply_wiki_lifecycle_gate_for_plan(server, &plan, rows, requested_lifecycle)
}

#[cfg(test)]
pub(crate) fn apply_wiki_lifecycle_gate_for_plan(
    server: &MemoryServer,
    plan: &WikiReadPlan,
    rows: &mut Vec<Value>,
    requested_lifecycle: Option<&str>,
) -> Result<(), String> {
    let is_all_scope = requested_lifecycle.is_some_and(|value| value.eq_ignore_ascii_case("all"));
    let requested: Option<WikiLifecycleV1> = match requested_lifecycle {
        None => None,
        Some(_) if is_all_scope => None,
        Some(value) => Some(
            value
                .parse::<WikiLifecycleV1>()
                .map_err(|e| format!("invalid lifecycle scope '{value}': {e}"))?,
        ),
    };
    let default_to_active_only = requested_lifecycle.is_none();

    let entries = list_wiki_entries_for_plan(server, plan, "/wiki", 5000)
        .map_err(|e| format!("wiki lifecycle gate candidate lookup failed: {e}"))?;
    let mut by_id: HashMap<&str, Vec<&StoredWikiEntry>> = HashMap::new();
    for entry in &entries {
        by_id
            .entry(entry.entry.id.as_str())
            .or_default()
            .push(entry);
    }

    rows.retain_mut(|row| {
        let Some(id) = row.get("id").and_then(Value::as_str).map(str::to_string) else {
            return is_all_scope;
        };
        let Some(candidates) = by_id.get(id.as_str()) else {
            // Unresolved row: fail closed (see doc comment above) — kept
            // only under the explicit "all" opt-out.
            return is_all_scope;
        };
        let requested_store = row
            .get("store")
            .cloned()
            .and_then(|value| serde_json::from_value::<StoreRef>(value).ok());
        let resolved = requested_store
            .as_ref()
            .and_then(|store_ref| candidates.iter().find(|entry| &entry.store == store_ref))
            .copied()
            .or_else(|| (candidates.len() == 1).then_some(candidates[0]));
        let Some(resolved) = resolved else {
            return is_all_scope;
        };
        let entry = &resolved.entry;
        let lifecycle = derive_wiki_lifecycle(&entry.metadata, &entry.path);
        let keep = if let Some(wanted) = requested {
            lifecycle == wanted
        } else if default_to_active_only {
            lifecycle.is_default_retrievable()
        } else {
            true
        };
        if keep {
            attach_wiki_provenance(row, entry, &resolved.store, lifecycle);
        }
        keep
    });
    Ok(())
}

/// Full-`MemoryEntry` sibling of `apply_wiki_lifecycle_gate`'s row-level
/// scope predicate, for callers (browse) that still hold the full entry
/// rather than an already-slimmed search row.
pub(super) fn wiki_entry_matches_lifecycle_scope(
    entry: &MemoryEntry,
    requested_lifecycle: Option<&str>,
) -> Result<bool, String> {
    let lifecycle = derive_wiki_lifecycle(&entry.metadata, &entry.path);
    match requested_lifecycle {
        None => Ok(lifecycle.is_default_retrievable()),
        Some(value) if value.eq_ignore_ascii_case("all") => Ok(true),
        Some(value) => {
            let wanted = value
                .parse::<WikiLifecycleV1>()
                .map_err(|e| format!("invalid lifecycle scope '{value}': {e}"))?;
            Ok(lifecycle == wanted)
        }
    }
}

/// Canon doc §7.1: "readers prefer typed refs and fall back to strings."
/// Returns the typed `evidence_refs_v1`'s `ref` strings when non-empty
/// (the common case going forward), or the legacy `source_refs` strings for
/// an entry written before this leaf's dual-write landed.
pub(super) fn preferred_wiki_references(metadata: &Value) -> Vec<String> {
    let typed: Vec<String> = metadata
        .get("evidence_refs_v1")
        .and_then(Value::as_array)
        .map(|refs| {
            refs.iter()
                .filter_map(|r| r.get("ref").and_then(Value::as_str))
                .filter(|reference| !reference.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if !typed.is_empty() {
        return typed;
    }
    metadata
        .get("source_refs")
        .and_then(Value::as_array)
        .map(|refs| {
            refs.iter()
                .filter_map(Value::as_str)
                .filter(|reference| !reference.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn attach_wiki_provenance(
    row: &mut Value,
    entry: &MemoryEntry,
    store_ref: &StoreRef,
    lifecycle: WikiLifecycleV1,
) {
    let Some(obj) = row.as_object_mut() else {
        return;
    };
    let authority = derive_wiki_authority(&entry.metadata);
    obj.insert("store".to_string(), json!(store_ref));
    obj.insert("lifecycle".to_string(), json!(lifecycle.as_str()));
    obj.insert("authority".to_string(), json!(authority.as_str()));
    obj.insert("revision".to_string(), json!(entry.revision));
    obj.insert(
        "source_refs".to_string(),
        entry
            .metadata
            .get("source_refs")
            .cloned()
            .unwrap_or_else(|| json!([])),
    );
    obj.insert(
        "evidence_refs_v1".to_string(),
        entry
            .metadata
            .get("evidence_refs_v1")
            .cloned()
            .unwrap_or_else(|| json!([])),
    );
    obj.insert(
        "references".to_string(),
        json!(preferred_wiki_references(&entry.metadata)),
    );
    if let Some(receipt) = derive_wiki_review_receipt(&entry.metadata) {
        obj.insert(
            "review_receipt".to_string(),
            serde_json::to_value(receipt).unwrap_or(Value::Null),
        );
    }
}
