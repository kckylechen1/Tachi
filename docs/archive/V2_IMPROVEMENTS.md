# Sigil V2 Remaining Improvements — Implementation Spec

Read the existing codebase before making changes. Key files:
- `crates/memory-core/src/db.rs` — database operations
- `crates/memory-core/src/lib.rs` — MemoryStore public API
- `crates/memory-core/src/types.rs` — data types (MemoryEntry, SearchOptions, etc.)
- `crates/memory-server/src/main.rs` — MCP server + tool handlers
- `crates/memory-server/src/pipeline.rs` — pipeline workers (causal, consolidator, distiller)

Run `cargo check -p memory-server` and `cargo test --workspace` after ALL changes.

---

## 1. Idempotent Event Ingestion (幂等去重)

### Problem
Repeated `ingest_event` calls with the same conversation_id + turn_id will re-extract facts and re-trigger pipeline workers, creating duplicate memories.

### Solution
Add a `processed_events` table to track which events have been processed.

### db.rs changes:
1. In `init_schema`, add:
```sql
CREATE TABLE IF NOT EXISTS processed_events (
    event_hash TEXT PRIMARY KEY,
    event_id   TEXT NOT NULL DEFAULT '',
    worker     TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT ''
);
```

2. Add two functions:
```rust
/// Check if an event has already been processed by a specific worker.
pub fn is_event_processed(conn: &Connection, event_hash: &str, worker: &str) -> Result<bool, MemoryError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
        params![event_hash, worker],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Mark an event as processed by a specific worker.
pub fn mark_event_processed(conn: &Connection, event_hash: &str, event_id: &str, worker: &str) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "INSERT OR IGNORE INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![event_hash, event_id, worker, now],
    )?;
    Ok(())
}
```

### lib.rs changes:
Add wrapper methods on MemoryStore:
- `pub fn is_event_processed(&self, event_hash: &str, worker: &str) -> Result<bool, MemoryError>`
- `pub fn mark_event_processed(&self, event_hash: &str, event_id: &str, worker: &str) -> Result<(), MemoryError>`

### main.rs changes:
In the `ingest_event` tool handler, compute the event hash BEFORE processing:
```rust
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// Compute idempotency hash
let mut hasher = DefaultHasher::new();
conversation_id.hash(&mut hasher);
turn_id.hash(&mut hasher);
let event_hash = format!("{:x}", hasher.finish());

// Check if already processed
{
    let guard = self.store.lock().map_err(|e| format!("Lock error: {e}"))?;
    if guard.is_event_processed(&event_hash, "ingest")? {
        return Ok(CallToolResult::success(vec![Content::text(
            format!("Event already processed (hash: {})", event_hash)
        )]));
    }
}

// ... existing processing logic ...

// After successful processing, mark as processed
{
    let guard = self.store.lock().map_err(|e| format!("Lock error: {e}"))?;
    guard.mark_event_processed(&event_hash, &format!("{}:{}", conversation_id, turn_id), "ingest")?;
}
```

### pipeline.rs changes:
In `run_causal`, add dedup check at the start:
```rust
let event_hash = format!("{:x}", {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&event_id, &mut hasher);
    std::hash::Hasher::finish(&hasher)
});

match store.lock() {
    Ok(guard) => {
        if guard.is_event_processed(&event_hash, "causal").unwrap_or(false) {
            return; // Already processed
        }
    }
    Err(_) => return,
}

// ... existing logic ...

// At the end, mark as processed:
if let Ok(guard) = store.lock() {
    let _ = guard.mark_event_processed(&event_hash, &event_id, "causal");
}
```

---

## 2. Rule Lifecycle (规则生命周期)

### Problem
Distiller outputs rules with `state: DRAFT` in metadata but there's no mechanism to promote them to ACTIVE.

### Solution
A rule becomes ACTIVE when 3+ independent corrections support it. Add a `promote_rules` check after each distiller run.

### pipeline.rs changes (in `run_distiller`):
After saving distilled rules, add a promotion pass:

```rust
// After saving rules, promote DRAFT -> ACTIVE if enough support
// Lock store, list all memories at /behavior/global_rules path
// For each rule with metadata.state == "DRAFT":
//   Count how many corrections in derived_items contain keywords matching this rule
//   If count >= 3, update metadata.state to "ACTIVE"
```

Implementation:
```rust
/// Promote DRAFT rules to ACTIVE if they have sufficient supporting evidence.
async fn promote_draft_rules(store: &Arc<Mutex<MemoryStore>>) {
    let rules = match store.lock() {
        Ok(mut guard) => {
            let opts = SearchOptions {
                top_k: 100,
                path_prefix: Some("/behavior/global_rules".to_string()),
                include_archived: false,
                record_access: false,
                ..Default::default()
            };
            // Use list instead of search if available, or search with empty query
            match guard.search("", Some(opts)) {
                Ok(results) => results,
                Err(_) => return,
            }
        }
        Err(_) => return,
    };

    for result in rules {
        let entry = result.entry;
        let state = entry.metadata.get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("DRAFT");

        if state != "DRAFT" {
            continue;
        }

        // Count supporting corrections
        let support_count = match store.lock() {
            Ok(guard) => {
                guard.count_derived_by_source("causal", "/behavior/corrections")
                    .unwrap_or(0)
            }
            Err(_) => continue,
        };

        if support_count >= 3 {
            // Promote to ACTIVE
            let mut updated = entry.clone();
            if let Some(meta) = updated.metadata.as_object_mut() {
                meta.insert("state".to_string(), serde_json::json!("ACTIVE"));
            }
            if let Ok(mut guard) = store.lock() {
                let _ = guard.upsert(&updated);
            }
        }
    }
}
```

Call `promote_draft_rules(&store).await;` at the end of each distiller iteration (after saving new rules, before sleep).

---

## 3. L0 Context Loading (L0 上下文加载)

### Problem
When an agent searches memories, it doesn't automatically get global rules that should always be in context. The agent has to manually know to search `/behavior/global_rules`.

### Solution
Add a `get_pipeline_status` enhancement and a new tool `get_context` that auto-injects L0 rules.

### main.rs changes:
Add or update the `get_pipeline_status` tool to also return active global rules:

Actually, better approach: modify the `search_memory` tool to automatically append L0 rules:

In the `search_memory` tool handler, after computing search results, append any ACTIVE global rules that aren't already in the results:

```rust
// After normal search results, inject L0 rules if pipeline enabled
if self.pipeline_enabled {
    let guard = self.store.lock().map_err(|e| format!("Lock error: {e}"))?;
    let rules_opts = SearchOptions {
        top_k: 10,
        path_prefix: Some("/behavior/global_rules".to_string()),
        include_archived: false,
        record_access: false,
        ..Default::default()
    };
    if let Ok(rule_results) = guard.search("", Some(rules_opts)) {
        for rule in rule_results {
            // Only inject ACTIVE rules
            let state = rule.entry.metadata.get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("DRAFT");
            if state == "ACTIVE" {
                // Add to results with a marker
                // Append to the JSON output with "source": "L0_rule"
            }
        }
    }
}
```

The key is to append L0 rules to the search results JSON array with an extra field `"l0_rule": true` so the agent knows these are injected rules.

---

## 4. Optimistic Locking (乐观锁)

### Problem
If two consolidator instances run concurrently (unlikely with single instance, but possible with multiple IDEs), they could both read the same memory, merge it differently, and overwrite each other.

### Solution
Add a `revision` column to memories. Consolidator must check revision before writing.

### db.rs changes:
1. In `init_schema` migrations section, add:
```rust
ensure_column(conn, "memories", "revision", "INTEGER NOT NULL DEFAULT 1")?;
```

2. Add a function to update memory with revision check:
```rust
/// Update a memory only if its revision matches (optimistic lock).
/// Returns Ok(true) if updated, Ok(false) if revision mismatch.
pub fn update_with_revision(
    conn: &Connection,
    id: &str,
    new_text: &str,
    new_summary: &str,
    new_source: &str,
    new_metadata: &str,
    new_vec: Option<&[u8]>,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let new_revision = expected_revision + 1;
    conn.execute(
        "UPDATE memories SET text = ?1, summary = ?2, source = ?3, metadata = ?4, updated_at = ?5, revision = ?6
         WHERE id = ?7 AND revision = ?8",
        params![new_text, new_summary, new_source, new_metadata, now, new_revision, id, expected_revision],
    )?;
    let updated = conn.changes() > 0;

    // Update vector if provided and row was updated
    if updated {
        if let Some(vec_blob) = new_vec {
            let rowid: Option<i64> = conn.query_row(
                "SELECT rowid FROM memories WHERE id = ?1", params![id], |row| row.get(0)
            ).ok();
            if let Some(rowid) = rowid {
                conn.execute(
                    "INSERT OR REPLACE INTO memories_vec (rowid, embedding) VALUES (?1, ?2)",
                    params![rowid, vec_blob],
                )?;
            }
        }
        // Update FTS
        conn.execute(
            "INSERT OR REPLACE INTO memories_fts (rowid, text, summary, keywords)
             SELECT rowid, text, summary, keywords FROM memories WHERE id = ?1",
            params![id],
        )?;
    }

    Ok(updated)
}
```

### types.rs changes:
Add `revision: i64` field to `MemoryEntry`:
```rust
pub struct MemoryEntry {
    // ... existing fields ...
    pub revision: i64,
}
```
Make sure Default is set to 1.

### lib.rs changes:
Add wrapper method for `update_with_revision`.

### pipeline.rs changes (run_consolidator):
When performing a merge, use the revision-checked update instead of `upsert`:
- Read the target memory's current revision
- After merge, call `update_with_revision` with the expected revision
- If false (revision mismatch), log and skip (another instance already merged)

---

## Verification

```bash
cargo check -p memory-server     # must compile
cargo test --workspace           # all tests pass
```

Then test startup:
```bash
ENABLE_PIPELINE=true \
VOYAGE_API_KEY="your_voyage_key_here" \
SILICONFLOW_API_KEY="your_siliconflow_key_here" \
MEMORY_DB_PATH="/Users/kckylechen/.gemini/antigravity/memory.db" \
timeout 3 ./target/release/memory-server
```

Should show:
```
Pipeline workers: ENABLED
Database integrity: OK
```

DO NOT modify prompts.rs or llm.rs — they are unchanged in this iteration.
