use memcore::MemoryStore;

use super::memo::sticky_ttl_expired;
use super::STICKY_PATH;

/// TTL sweep: unread stickies past `ttl_days` are archived (never surface in
/// briefing again; visible only via `include_read=true`). Already-claimed
/// stickies are left alone here — their archival happened at claim time.
/// This is a belt-and-suspenders batch pass for the periodic GC job — the
/// same TTL check also runs inline at read time
/// (`pending::claim_unread_stickies_for_briefing`) so expiry is correct even
/// between GC runs.
pub(crate) fn gc_expired_sticky_memories(store: &mut MemoryStore) -> Result<usize, String> {
    let now = chrono::Utc::now();

    let candidates = store
        .list_memories_by_category_and_path_prefix("sticky", &format!("{STICKY_PATH}%"))
        .map_err(|e| format!("query expired sticky memories failed: {e}"))?;

    let mut ids_to_expire = Vec::new();
    for row in candidates {
        if row.archived {
            continue;
        }
        let metadata: serde_json::Value = serde_json::from_str(&row.metadata)
            .map_err(|e| format!("parse sticky metadata for '{}' failed: {e}", row.id))?;
        let status = metadata
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unread");
        if status != "unread" {
            continue;
        }
        let Some(memo) = metadata
            .get("sticky")
            .and_then(|v| serde_json::from_value::<super::memo::StickyMemo>(v.clone()).ok())
        else {
            continue;
        };
        if sticky_ttl_expired(&memo, now) {
            ids_to_expire.push(row.id);
        }
    }

    let mut expired = 0usize;
    for id in ids_to_expire {
        let Some(mut entry) = store
            .get(&id)
            .map_err(|e| format!("load expired sticky '{id}' failed: {e}"))?
        else {
            continue;
        };
        let Some(obj) = entry.metadata.as_object_mut() else {
            continue;
        };
        obj.insert(
            "status".to_string(),
            serde_json::Value::String("expired".to_string()),
        );
        entry.archived = true;
        entry.vector = None;
        store
            .upsert(&entry)
            .map_err(|e| format!("archive expired sticky '{id}' failed: {e}"))?;
        expired += 1;
    }

    Ok(expired)
}
