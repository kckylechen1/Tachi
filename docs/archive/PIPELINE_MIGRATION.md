# Pipeline Workers Migration: Python → Rust

## Goal
Port the 3 Python pipeline workers (`mcp/workers/causal.py`, `mcp/workers/distiller.py`, `mcp/workers/consolidator.py`) into the Rust `memory-server` binary so the Python server is no longer needed for pipeline functionality.

## Pre-work Already Done (Verify These First)

### 1. `crates/memory-server/src/prompts.rs`
Should contain these 4 NEW constants (in addition to existing EXTRACTION_PROMPT and SUMMARY_PROMPT):
- `CAUSAL_PROMPT` — causal extraction prompt (from `mcp/workers/causal.py`)
- `MERGE_PROMPT` — memory merge prompt (from `mcp/workers/consolidator.py`)
- `CONTRADICTION_PROMPT` — contradiction detection prompt (from `mcp/workers/consolidator.py`)
- `DISTILLER_PROMPT` — rule distillation prompt (from `mcp/workers/distiller.py`)

If missing, add them. Reference the Python files for the exact prompt text.

### 2. `crates/memory-server/src/llm.rs`
LlmClient should have these 2 NEW methods (in addition to existing ones):
- `call_llm_with_model(&self, system, user, model, temperature, max_tokens)` — explicit model override, delegates to call_llm
- `embed_batch(&self, texts: &[String], input_type: &str) -> Result<Vec<Vec<f32>>, String>` — batch embedding via Voyage-4 API (same endpoint, but `input` is an array of strings, response `data` is array of embeddings)

If missing, add them.

### 3. `crates/memory-core/src/db.rs`
Should have:
- `derived_items` table in `init_schema` (CREATE TABLE IF NOT EXISTS with columns: id, text, path, summary, importance, source, scope, metadata, created_at)
- `save_derived(conn, text, path, summary, importance, source, scope, metadata) -> Result<String>` — insert into derived_items
- `count_derived_by_source(conn, source, path_prefix) -> Result<u64>` — COUNT with source and path LIKE
- `list_derived_by_source(conn, source, path_prefix, limit) -> Result<Vec<Value>>` — SELECT with limit
- `archive_memory(conn, id) -> Result<bool>` — UPDATE memories SET archived = 1

If missing, add them.

### 4. `crates/memory-core/src/lib.rs`
MemoryStore should expose wrapper methods for the 4 new db functions:
- `save_derived(...)`, `count_derived_by_source(...)`, `list_derived_by_source(...)`, `archive_memory(...)`

If missing, add them.

---

## New Work Required

### 5. Create `crates/memory-server/src/pipeline.rs` (NEW FILE)

This is the core module implementing 3 async pipeline workers. Each worker is a public async function (not a struct/trait). All workers receive `Arc<Mutex<MemoryStore>>` + `Arc<LlmClient>`.

**Key patterns:**
- Lock the store mutex BRIEFLY, extract data, drop the guard BEFORE any async `.await` call
- All errors are logged via `eprintln!` — never panic, never propagate errors that would kill the task
- Parse LLM JSON responses defensively (strip markdown code fences, handle malformed JSON gracefully)

#### 5a. `run_causal` — Causal Extraction Worker

```rust
pub async fn run_causal(
    store: Arc<Mutex<MemoryStore>>,
    llm: Arc<LlmClient>,
    messages: Vec<serde_json::Value>,
    event_id: String,
)
```

Logic (ported from `mcp/workers/causal.py`):
1. Convert `messages` to conversation text: join each message as `"{role}: {content}\n"`
2. If < 2 messages or empty text, return early
3. Call `llm.call_llm_with_model(CAUSAL_PROMPT, &conversation_text, "Qwen/Qwen3.5-27B", 0.1, 1000)`
4. Parse the response as a JSON array of objects
5. For each item with `"type": "correction"`: extract context/wrong_action/correct_action, serialize as JSON, generate summary via `llm.generate_summary()`, then lock store and call `store.save_derived(text, "/behavior/corrections", &summary, 0.9, "causal", "general", &metadata)`
6. For each item with `"type": "causal"` and confidence >= 0.5: extract cause_text/effect_text/relation/confidence
   - Find matching memory IDs: lock store, call `store.search(cause_text, opts_json)` where opts_json uses FTS-only weights `{"top_k":1,"record_access":false,"weights":{"semantic":0.0,"fts":1.0,"symbolic":0.5,"decay":0.0}}`
   - If both cause and effect memory IDs found and differ, call `store.add_edge(edge_json)` where edge_json has source_id, target_id, relation, weight, metadata

#### 5b. `run_consolidator` — Memory Consolidation Worker

```rust
pub async fn run_consolidator(
    store: Arc<Mutex<MemoryStore>>,
    llm: Arc<LlmClient>,
    memory_id: String,
)
```

Logic (ported from `mcp/workers/consolidator.py`):
1. Lock store, get memory by ID (`store.get(memory_id)`), extract text, release lock
2. If no memory or empty text, return
3. Embed the text: `llm.embed_voyage(&text, "query").await`
4. Lock store, search by the embedded vector for similar memories (same path_prefix, top_k=10), release lock
5. For each candidate (skip self):
   - If score > 0.85 → **merge**: call LLM with MERGE_PROMPT on old+new texts, generate new summary via LLM, embed merged text, then lock store and update the target memory (use store.save() or direct SQL update), archive the source memory via `store.archive_memory(memory_id)`
   - If 0.5 < score <= 0.85 → **contradiction check**: call LLM with CONTRADICTION_PROMPT, parse JSON response, if `contradicts: true`, lock store and add a "contradicts" edge
   - Break after first merge or contradiction found
6. **Entity linking**: extract entities/persons from memory fields, for each entity search for other memories mentioning it, add "related_to" edges for shared entities

#### 5c. `run_distiller` — Rule Distillation Worker (Background Loop)

```rust
pub async fn run_distiller(
    store: Arc<Mutex<MemoryStore>>,
    llm: Arc<LlmClient>,
    poll_interval_secs: u64,
)
```

Logic (ported from `mcp/workers/distiller.py`):
1. Loop forever with `tokio::time::sleep(Duration::from_secs(poll_interval_secs))` between iterations
2. Lock store, call `store.count_derived_by_source("causal", "/behavior/corrections")`, release lock
3. If count < 5, skip this iteration
4. Lock store, call `store.list_derived_by_source("causal", "/behavior/corrections", 2000)`, release lock
5. Join their `text` fields into a numbered list: `[1] text1\n\n[2] text2\n\n...`
6. Call `llm.call_llm(DISTILLER_PROMPT, &sample_text, None, 0.1, 2000)`
7. Parse JSON array — each element is either a string rule or `{"rule": "...", "rationale": "..."}`
8. For each rule:
   - Embed via `llm.embed_voyage(&rule, "document")`
   - Generate summary via `llm.generate_summary(&rule)`
   - Lock store, save as memory: path="/behavior/global_rules", topic="global_rule", importance=0.95, scope="general"

#### Helper function:

```rust
fn strip_code_fence(text: &str) -> &str {
    let text = text.trim();
    let text = if text.starts_with("```json") {
        text[7..].trim()
    } else if text.starts_with("```") {
        text[3..].trim()
    } else {
        text
    };
    text.trim_end_matches("```").trim()
}

fn parse_json_array(content: &str) -> Vec<serde_json::Value> {
    let clean = strip_code_fence(content);
    match serde_json::from_str::<Vec<serde_json::Value>>(clean) {
        Ok(arr) => arr,
        Err(_) => Vec::new(),
    }
}
```

### 6. Modify `crates/memory-server/src/main.rs`

1. Add `mod pipeline;` near the top (next to `mod llm;` and `mod prompts;`)

2. Add `pipeline_enabled: bool` field to `MemoryServer` struct. Set it in `MemoryServer::new()`:
```rust
let pipeline_enabled = std::env::var("ENABLE_PIPELINE")
    .map(|v| v == "true" || v == "1")
    .unwrap_or(false);
```

3. After server creation in `main()`, if pipeline_enabled, spawn the distiller loop:
```rust
if server.pipeline_enabled {
    eprintln!("Pipeline workers: ENABLED");
    let distiller_store = server.store.clone();
    let distiller_llm = server.llm.clone();
    tokio::spawn(async move {
        pipeline::run_distiller(distiller_store, distiller_llm, 7200).await;
    });
} else {
    eprintln!("Pipeline workers: DISABLED (set ENABLE_PIPELINE=true to enable)");
}
```

4. In the `ingest_event` tool handler, after saving the event successfully, if pipeline_enabled, spawn causal worker:
```rust
if self.pipeline_enabled {
    let store = self.store.clone();
    let llm = self.llm.clone();
    let msgs = messages.clone();
    let eid = event_id.clone();
    tokio::spawn(async move {
        pipeline::run_causal(store, llm, msgs, eid).await;
    });
}
```

5. In the `save_memory` tool handler, after saving successfully, if pipeline_enabled, spawn consolidator:
```rust
if self.pipeline_enabled {
    let store = self.store.clone();
    let llm = self.llm.clone();
    let mid = id.clone();
    tokio::spawn(async move {
        pipeline::run_consolidator(store, llm, mid).await;
    });
}
```

### 7. Verification

After all changes:
```bash
cargo check -p memory-server    # must compile with no errors
cargo test --workspace           # all existing tests must pass
```

Fix any compilation errors. Warnings about unused code are acceptable for now.

---

## Important Notes

- The `MemoryStore` is behind `Arc<Mutex<MemoryStore>>` — always lock briefly, clone/extract data, then drop the guard before `.await`
- `LlmClient` is behind `Arc<LlmClient>` — it's `Clone` and all its methods take `&self`, so no mutex needed
- The `store.search()` method signature: `pub fn search(&self, query: &str, opts_json: &str) -> Result<String, MemoryError>` — returns a JSON string
- The `store.save()` method — check the exact signature in `lib.rs`, it takes text, vector, path, summary, etc.
- The `store.add_edge()` method: `pub fn add_edge(&self, edge_json: &str) -> Result<String, MemoryError>`
- For the consolidator's merge operation, you may need to add a simple `update_memory_text` function to db.rs/lib.rs if one doesn't exist
- Reference the Python source files (`mcp/workers/*.py`) for exact logic details
