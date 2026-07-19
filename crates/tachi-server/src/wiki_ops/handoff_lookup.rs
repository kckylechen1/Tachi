use super::*;

/// #1285 P0: one `/wiki/handoffs/...` campaign-handoff mirror, as read back
/// for previous-handoff discovery (`tachi_gh(action='handoff_draft'|
/// 'handoff_publish'|'handoff_repair')`). Distinct from the `orchestrator_ops`
/// task-id-keyed `HandoffPacket` and the `tachi_gh(action='pr_handoff')`
/// flow-id-keyed artifact — this is the session/campaign domain (see
/// `handoff_ops` module doc for the full three-way split #1037 reserved).
#[derive(Debug, Clone)]
pub(crate) struct HandoffMirrorRef {
    pub(crate) path: String,
    pub(crate) issue: u64,
    pub(crate) published_at: Option<DateTime<Utc>>,
    pub(crate) supersedes_issue: Option<u64>,
    pub(crate) references: Vec<String>,
    pub(crate) text: String,
}

/// List wiki `/wiki/handoffs/...` mirrors scoped to `repo` (exact
/// `metadata.repo` match — #1285's multi-repo isolation requirement), newest
/// first by `metadata.published_at`. The isolation happens *before* a
/// `HandoffMirrorRef` is ever constructed (the `filter_map` below rejects any
/// entry whose `metadata.repo` doesn't match the caller's `repo` argument),
/// so `HandoffMirrorRef` itself doesn't need to carry `repo` (or `id` —
/// mirror identity downstream is keyed by `path`/`issue`, not the wiki
/// entry's own id) for any caller; every ref this function returns is
/// already known-scoped by construction. Local-only (no `gh`, no network) —
/// safe to call when GitHub transport is unavailable, which is exactly why
/// `handoff_draft`'s offline-degrade path leans on this instead of a gh
/// query for previous-handoff discovery.
pub(crate) fn list_handoff_mirrors_for_repo(
    server: &MemoryServer,
    repo: &str,
) -> Result<Vec<HandoffMirrorRef>, String> {
    let (entries, _source) = list_wiki_entries(server, "wiki", 5000)?;
    let mut mirrors: Vec<HandoffMirrorRef> = entries
        .into_iter()
        .filter(|entry| entry.path.starts_with("/wiki/handoffs/"))
        .filter_map(|entry| {
            let meta_repo = entry.metadata.get("repo").and_then(Value::as_str)?;
            if meta_repo != repo {
                return None;
            }
            let issue = entry.metadata.get("issue").and_then(Value::as_u64)?;
            let published_at = entry
                .metadata
                .get("published_at")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            let supersedes_issue = entry
                .metadata
                .get("supersedes_issue")
                .and_then(Value::as_u64);
            let references = preferred_wiki_references(&entry.metadata);
            Some(HandoffMirrorRef {
                path: entry.path.clone(),
                issue,
                published_at,
                supersedes_issue,
                references,
                text: entry.text.clone(),
            })
        })
        .collect();
    mirrors.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    Ok(mirrors)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built via `serde_json::from_value` (not a raw struct literal) — most
    // `MemoryEntry` fields carry `#[serde(default = ...)]`, so this stays
    // correct as new low-frequency fields get added upstream instead of
    // needing a matching edit here every time.
    fn entry(path: &str, repo: &str, issue: u64, published_at: &str) -> MemoryEntry {
        serde_json::from_value(json!({
            "id": format!("wiki-{issue}"),
            "path": path,
            "text": format!("body for {issue}"),
            "timestamp": published_at,
            "metadata": {
                "repo": repo,
                "issue": issue,
                "published_at": published_at,
            },
        }))
        .expect("minimal MemoryEntry fixture")
    }

    /// `list_handoff_mirrors_for_repo`'s repo-domain filter is pure string
    /// equality on `metadata.repo` — this locks that in without needing a
    /// live store: a repo mismatch must never leak into another repo's
    /// previous-handoff discovery (#1285 multi-repo isolation).
    #[test]
    fn repo_filter_is_exact_match_not_substring() {
        let a = entry(
            "/wiki/handoffs/2026-07-01-a",
            "owner/repo",
            1,
            "2026-07-01T00:00:00Z",
        );
        let b = entry(
            "/wiki/handoffs/2026-07-02-b",
            "owner/repo-fork",
            2,
            "2026-07-02T00:00:00Z",
        );
        let matches = |repo: &str| -> Vec<u64> {
            [&a, &b]
                .iter()
                .filter(|e| e.metadata.get("repo").and_then(Value::as_str) == Some(repo))
                .filter_map(|e| e.metadata.get("issue").and_then(Value::as_u64))
                .collect()
        };
        assert_eq!(matches("owner/repo"), vec![1]);
        assert_eq!(matches("owner/repo-fork"), vec![2]);
    }
}
