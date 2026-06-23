/// B5: known historical migrations for `metadata.quarantine.expected_db`.
///
/// PR-3 v4 stamped `expected_db` with the absolute DB path that *should*
/// have owned the row. Some of those paths are now stale because the
/// extension layout moved on disk:
///
///   `~/.openclaw/local-plugins/extensions/memory-hybrid-bridge/data/agents/<X>/memory.db`
///                                ↓
///   `~/.openclaw/extensions/tachi/data/agents/<X>/memory.db`
///
/// Without this rewrite, `restore-all --to-db <label>` filters by canonical
/// path equality and matches zero rows even though the user's intent —
/// "move these to the modern home of the same agent" — is unambiguous.
///
/// We deliberately keep the mapping table tiny and explicit (one entry).
/// Generic tail-matching would risk collapsing unrelated DBs that happen
/// to share an `agents/<X>/memory.db` suffix.
const LEGACY_EXPECTED_DB_REWRITES: &[(&str, &str)] = &[(
    "/.openclaw/local-plugins/extensions/memory-hybrid-bridge/data/agents/",
    "/.openclaw/extensions/tachi/data/agents/",
)];

/// Apply known legacy→current path rewrites to a stale `expected_db`. Pure
/// string substitution — no I/O. Returns the original on no-match so call
/// sites can chain with `canonicalize` without losing information.
pub(crate) fn rewrite_legacy_expected_db(raw: &str) -> String {
    for (from, to) in LEGACY_EXPECTED_DB_REWRITES {
        if let Some(idx) = raw.find(from) {
            let mut out = String::with_capacity(raw.len() + to.len());
            out.push_str(&raw[..idx]);
            out.push_str(to);
            out.push_str(&raw[idx + from.len()..]);
            return out;
        }
    }
    raw.to_string()
}
