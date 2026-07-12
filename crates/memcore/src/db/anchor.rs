//! `ensure_anchor` — deterministic anchor rows for external objects
//! (tachi#773 item 4, "设计 v4(冻结稿)" entity-row decision).
//!
//! Anchors are `memories` rows with `category = "entity"` (already a legal
//! CHECK-constraint value — no migration, per `precedent_ops.rs:33-42`'s
//! established convention of reusing existing categories +
//! `metadata.kind`-style discriminators instead of growing the CHECK list)
//! and a deterministic id in the reserved `anchor:` namespace, so
//! `memory_edges.source_id/target_id` (plain `memories.id` foreign keys,
//! no FK constraint) can point at them without touching edge-table
//! semantics.
//!
//! Four guards (sol #773 v3 correction 3):
//! (a) read-verify: if the deterministic id already exists, verify its
//!     kind/key match what the caller asked for before treating it as a
//!     no-op success.
//! (b) fail-closed on type/key mismatch: a collision where the existing row
//!     claims a different kind or key is an error, never a silent overwrite.
//! (c) the `anchor:` id namespace is reserved: enforced in
//!     `memory_crud::upsert` (ordinary upserts to `anchor:`-prefixed ids are
//!     rejected) — `ensure_anchor` is the only creation path, using its own
//!     `INSERT ... OR IGNORE`, not `upsert`'s `ON CONFLICT DO UPDATE`.
//! (d) both edge endpoints must exist in the same physical DB: this module
//!     only creates the anchor row itself; callers that immediately follow
//!     with an edge write get that guarantee for free because
//!     `add_edge`/`ensure_anchor` share one `Connection` — there is no
//!     cross-DB id to smuggle in.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;

use super::common::now_utc_iso;

/// The recognized anchor kinds (tachi#773 item 4). Extend this list, not the
/// id-format convention, when a new external-object type needs an anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    Issue,
    Pr,
    Dispatch,
    Seat,
}

impl AnchorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Pr => "pr",
            Self::Dispatch => "dispatch",
            Self::Seat => "seat",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "issue" => Some(Self::Issue),
            "pr" => Some(Self::Pr),
            "dispatch" => Some(Self::Dispatch),
            "seat" => Some(Self::Seat),
            _ => None,
        }
    }
}

/// Deterministic anchor id: `anchor:<kind>:<key>`.
///
/// `key` is caller-supplied and kind-shaped (e.g. `<owner>/<repo>:<n>` for
/// issue/pr, a dispatch id for dispatch, a seat name for seat) — this
/// function does not interpret it, only concatenates.
pub fn anchor_id(kind: AnchorKind, key: &str) -> String {
    format!("anchor:{}:{key}", kind.as_str())
}

/// Deterministic anchor path: `/anchors/<kind>/...` (mission-specified path
/// shape; `...` is the raw key so multiple keys under one kind list
/// cleanly with `list_by_path`).
pub fn anchor_path(kind: AnchorKind, key: &str) -> String {
    format!("/anchors/{}/{key}", kind.as_str())
}

/// Row shape read back for guard (a)/(b) verification.
struct ExistingAnchor {
    path: String,
    metadata_kind: Option<String>,
    metadata_key: Option<String>,
}

fn read_existing_anchor(
    conn: &Connection,
    id: &str,
) -> Result<Option<ExistingAnchor>, MemoryError> {
    let row = conn
        .query_row(
            "SELECT path, metadata FROM memories WHERE id = ?1",
            params![id],
            |row| {
                let path: String = row.get(0)?;
                let metadata_str: String = row.get(1)?;
                Ok((path, metadata_str))
            },
        )
        .optional()?;
    let Some((path, metadata_str)) = row else {
        return Ok(None);
    };
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_str).unwrap_or_else(|_| serde_json::json!({}));
    Ok(Some(ExistingAnchor {
        path,
        metadata_kind: metadata
            .get("anchor_kind")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        metadata_key: metadata
            .get("anchor_key")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    }))
}

/// Ensure a deterministic anchor row exists for `(kind, key)`, creating it
/// with `INSERT ... OR IGNORE` semantics if absent.
///
/// Returns the anchor's deterministic id on success. Fails closed
/// ([`MemoryError::InvalidArg`]) if a row already exists at that id but its
/// recorded kind/key do not match what the caller asked for (guard b) — this
/// can only happen if something else already claimed the id outside
/// `ensure_anchor`, since the id is a deterministic function of
/// `(kind, key)` and collisions require an actual kind/key mismatch bug.
pub fn ensure_anchor(
    conn: &Connection,
    kind: AnchorKind,
    key: &str,
) -> Result<String, MemoryError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(MemoryError::InvalidArg(
            "ensure_anchor: key must not be empty".to_string(),
        ));
    }
    let id = anchor_id(kind, key);

    // Guard (a): read-verify existing deterministic id matches kind/key.
    if let Some(existing) = read_existing_anchor(conn, &id)? {
        let kind_matches = existing.metadata_kind.as_deref() == Some(kind.as_str());
        let key_matches = existing.metadata_key.as_deref() == Some(key);
        if kind_matches && key_matches {
            return Ok(id);
        }
        // Guard (b): fail-closed on type/key mismatch — never silently
        // reuse or overwrite a row that doesn't actually match.
        return Err(MemoryError::InvalidArg(format!(
            "ensure_anchor: id '{id}' already exists with mismatched kind/key \
             (existing kind={:?} key={:?} path={:?}, requested kind={} key={key})",
            existing.metadata_kind,
            existing.metadata_key,
            existing.path,
            kind.as_str(),
        )));
    }

    let path = anchor_path(kind, key);
    let now = now_utc_iso();
    let metadata = serde_json::json!({
        "anchor_kind": kind.as_str(),
        "anchor_key": key,
    });
    let metadata_json = serde_json::to_string(&metadata)?;
    let summary = format!("anchor:{}:{key}", kind.as_str());

    // guard (c) enforcement lives in memory_crud::upsert (rejects ordinary
    // upserts to `anchor:`-prefixed ids); this INSERT OR IGNORE is the sole
    // creation path and bypasses `upsert` entirely.
    conn.execute(
        r#"INSERT OR IGNORE INTO memories
              (id, path, summary, text, importance,
               timestamp, valid_from, valid_until, category, topic, keywords, entities,
               source, scope, archived, created_at, updated_at,
               access_count, last_access, revision, metadata,
               retention_policy, domain, recall_count, query_diversity, tier)
           VALUES (?1,?2,?3,?4,0.7,
                   ?5,?5,NULL,'entity','',?6,?6,
                   'auto','general',0,?5,?5,
                   0,NULL,1,?7,
                   'pinned',NULL,0,0,'pattern')"#,
        params![id, path, summary, summary, now, "[]", metadata_json],
    )?;

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_schema, register_sqlite_vec, try_load_sqlite_vec};

    fn make_conn() -> Connection {
        libsimple::enable_auto_extension().unwrap();
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        try_load_sqlite_vec(&conn);
        conn
    }

    #[test]
    fn anchor_id_and_path_shapes() {
        assert_eq!(
            anchor_id(AnchorKind::Issue, "kckylechen1/tachi:773"),
            "anchor:issue:kckylechen1/tachi:773"
        );
        assert_eq!(
            anchor_path(AnchorKind::Issue, "kckylechen1/tachi:773"),
            "/anchors/issue/kckylechen1/tachi:773"
        );
        assert_eq!(anchor_id(AnchorKind::Seat, "wizard"), "anchor:seat:wizard");
    }

    #[test]
    fn ensure_anchor_creates_entity_row() {
        let conn = make_conn();
        let id = ensure_anchor(&conn, AnchorKind::Issue, "kckylechen1/tachi:773").unwrap();
        assert_eq!(id, "anchor:issue:kckylechen1/tachi:773");

        let (category, path): (String, String) = conn
            .query_row(
                "SELECT category, path FROM memories WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(category, "entity");
        assert_eq!(path, "/anchors/issue/kckylechen1/tachi:773");
    }

    #[test]
    fn ensure_anchor_is_idempotent_same_kind_key() {
        let conn = make_conn();
        let id1 = ensure_anchor(&conn, AnchorKind::Dispatch, "wf_abc123").unwrap();
        let id2 = ensure_anchor(&conn, AnchorKind::Dispatch, "wf_abc123").unwrap();
        assert_eq!(id1, id2);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE id = ?1",
                params![id1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "second call must not duplicate or error");
    }

    #[test]
    fn ensure_anchor_fails_closed_on_kind_mismatch_for_same_key() {
        let conn = make_conn();
        // Different kinds naturally produce different ids (kind is part of
        // the id), so to exercise the mismatch guard we simulate a collision
        // by hand-inserting a row at the deterministic id with a different
        // recorded kind in metadata (e.g. a corrupted/foreign row).
        let id = anchor_id(AnchorKind::Pr, "kckylechen1/tachi:1013");
        conn.execute(
            r#"INSERT INTO memories
                  (id, path, summary, text, importance, timestamp, valid_from, category,
                   topic, keywords, entities, source, scope, archived, created_at, updated_at,
                   access_count, revision, metadata, retention_policy, recall_count, query_diversity, tier)
               VALUES (?1, '/anchors/pr/other', 's', 't', 0.7, ?2, ?2, 'entity',
                       '', '[]', '[]', 'external:test', 'general', 0, ?2, ?2,
                       0, 1, ?3, 'pinned', 0, 0, 'pattern')"#,
            params![
                id,
                now_utc_iso(),
                serde_json::json!({"anchor_kind": "dispatch", "anchor_key": "kckylechen1/tachi:1013"})
                    .to_string()
            ],
        )
        .unwrap();

        let err = ensure_anchor(&conn, AnchorKind::Pr, "kckylechen1/tachi:1013").unwrap_err();
        assert!(err.to_string().contains("mismatched kind/key"));
    }

    #[test]
    fn ensure_anchor_fails_closed_on_key_mismatch_for_hand_forged_id() {
        let conn = make_conn();
        // Hand-forge a row at an id that matches the deterministic format
        // but whose recorded metadata key disagrees (simulating a corrupted
        // or foreign row landing at the same id via a bug elsewhere).
        let id = anchor_id(AnchorKind::Issue, "kckylechen1/tachi:773");
        conn.execute(
            r#"INSERT INTO memories
                  (id, path, summary, text, importance, timestamp, valid_from, category,
                   topic, keywords, entities, source, scope, archived, created_at, updated_at,
                   access_count, revision, metadata, retention_policy, recall_count, query_diversity, tier)
               VALUES (?1, '/anchors/issue/other', 's', 't', 0.7, ?2, ?2, 'entity',
                       '', '[]', '[]', 'external:test', 'general', 0, ?2, ?2,
                       0, 1, ?3, 'pinned', 0, 0, 'pattern')"#,
            params![
                id,
                now_utc_iso(),
                serde_json::json!({"anchor_kind": "issue", "anchor_key": "some/other:999"})
                    .to_string()
            ],
        )
        .unwrap();

        let err = ensure_anchor(&conn, AnchorKind::Issue, "kckylechen1/tachi:773").unwrap_err();
        assert!(err.to_string().contains("mismatched kind/key"));
    }

    #[test]
    fn ensure_anchor_rejects_empty_key() {
        let conn = make_conn();
        let err = ensure_anchor(&conn, AnchorKind::Seat, "  ").unwrap_err();
        assert!(err.to_string().contains("key must not be empty"));
    }

    #[test]
    fn anchor_kind_round_trips_through_str() {
        for kind in [
            AnchorKind::Issue,
            AnchorKind::Pr,
            AnchorKind::Dispatch,
            AnchorKind::Seat,
        ] {
            assert_eq!(AnchorKind::from_str_opt(kind.as_str()), Some(kind));
        }
        assert_eq!(AnchorKind::from_str_opt("bogus"), None);
    }
}
