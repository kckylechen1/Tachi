# Wiki References Spec

**Issue:** [#149](https://github.com/kckylechen1/tachi/issues/149)  
**Status:** Draft → Ready for implementation  
**Date:** 2026-06-03  
**Scope:** `crates/tachi-server` — Wiki write tools  

---

## 1. Goal

Add a first-class `references` parameter to Wiki write tools so agents and users can attach external links, absolute file paths, and GitHub issue shorthands to wiki entries. References are validated at write time and rendered in Obsidian export.

---

## 2. Files to Touch

| File | Change |
|------|--------|
| `crates/tachi-params/src/memory/wiki.rs` | Add `references: Vec<String>` to `WikiWriteParams` |
| `crates/tachi-params/src/facade.rs` | Add `references: Vec<String>` to `TachiWikiParams` |
| `crates/tachi-server/src/tools.rs` | Forward `references` from facade to memory params |
| `crates/tachi-server/src/wiki_ops.rs` | Add `validate_reference_format`, wire into write path, render in `markdown_for_obsidian` |
| `crates/tachi-server/src/types.rs` | (No change — `metadata` already supports `source_refs`) |

---

## 3. Data Structure Changes

### 3.1 `WikiWriteParams`

```rust
// crates/tachi-params/src/memory/wiki.rs
pub struct WikiWriteParams {
    pub title: String,
    pub content: String,
    pub project: Option<String>,
    pub path: Option<String>,
    pub force: Option<bool>,
    pub references: Vec<String>,  // ← NEW
}
```

### 3.2 `TachiWikiParams`

```rust
// crates/tachi-params/src/facade.rs
pub struct TachiWikiParams {
    pub action: String,
    pub title: Option<String>,
    pub content: Option<String>,
    pub project: Option<String>,
    pub path: Option<String>,
    pub force: Option<bool>,
    pub references: Vec<String>,  // ← NEW
}
```

### 3.3 Serialization

Both use `#[serde(default)]` so existing callers (no `references` field) get `Vec::new()` instead of a deserialization error.

---

## 4. Validation Logic

### 4.1 `validate_reference_format`

Location: `crates/tachi-server/src/wiki_ops.rs` (private helper)

```rust
use std::sync::OnceLock;
use regex::Regex;

fn validate_reference_format(reference: &str) -> Result<(), String> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err("Reference cannot be empty".to_string());
    }

    // URLs
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("file://")
    {
        return Ok(());
    }

    // Absolute paths
    if trimmed.starts_with('/') {
        return Ok(());
    }
    static WIN_PATH_RE: OnceLock<Regex> = OnceLock::new();
    let win_re = WIN_PATH_RE.get_or_init(|| Regex::new(r"^[a-zA-Z]:[/\\]").unwrap());
    if win_re.is_match(trimmed) {
        return Ok(());
    }

    // GitHub shorthand: #69, repo#69, owner/repo#69
    static GH_SHORTHAND_RE: OnceLock<Regex> = OnceLock::new();
    let gh_re = GH_SHORTHAND_RE.get_or_init(|| {
        Regex::new(r"^(?:#\d+|[a-zA-Z0-9_.-]+#\d+|[a-zA-Z0-9_-]+/[a-zA-Z0-9_.-]+#\d+)$").unwrap()
    });
    if gh_re.is_match(trimmed) {
        return Ok(());
    }

    Err(format!(
        "Invalid reference format: '{}'. Expected URL (http/https/file), absolute path, or GitHub shorthand (#N, repo#N, owner/repo#N)",
        trimmed
    ))
}
```

### 4.2 Batch Validation

```rust
fn validate_references(references: &[String]) -> Result<(), String> {
    for (i, r) in references.iter().enumerate() {
        validate_reference_format(r).map_err(|e| {
            format!("references[{}]: {}", i, e)
        })?;
    }
    Ok(())
}
```

### 4.3 Integration Point

In `handle_tachi_wiki_write` (or the underlying write handler), validate **before** any DB operation:

```rust
validate_references(&params.references)?;
```

On validation failure, return `Err(...)` immediately — no partial write.

---

## 5. Metadata Mapping

In `handle_tachi_wiki_write`, after validation:

```rust
let mut wiki_metadata = json!({
    "wiki": true,
    "wiki_title": params.title,
    "user_force": params.force.unwrap_or(false),
    "allow_cross_project": true,
    "source_refs": params.references,  // ← NEW
});
```

This lands in `MemoryEntry.metadata` as `"source_refs": ["https://...", "#69", ...]`.

---

## 6. Obsidian Export Rendering

### 6.1 Location

`markdown_for_obsidian` in `crates/tachi-server/src/wiki_ops.rs` (around line 455).

### 6.2 Rendering Logic

After the main content body, append a `## References` section if `source_refs` exists and is non-empty:

```rust
// After pushing main content to `body`
if let Some(refs) = entry.metadata.get("source_refs").and_then(|v| v.as_array()) {
    if !refs.is_empty() {
        body.push_str("\n\n## References\n\n");
        for ref_val in refs {
            if let Some(ref_str) = ref_val.as_str() {
                // Render as Markdown link if it looks like a URL,
                // otherwise as a plain list item
                let line = if ref_str.starts_with("http://")
                    || ref_str.starts_with("https://")
                    || ref_str.starts_with("file://")
                {
                    format!("- [{}]({})\n", ref_str, ref_str)
                } else {
                    format!("- `{}`\n", ref_str)
                };
                body.push_str(&line);
            }
        }
    }
}
```

### 6.3 Output Example

```markdown
# KRONOS 800K Postmortem

## Summary
...

## References

- [https://github.com/kckylechen1/Hyperion-Quant-SRC/issues/69](https://github.com/kckylechen1/Hyperion-Quant-SRC/issues/69)
- `#69`
- `kckylechen1/Hyperion-Quant-SRC#69`
- `/Users/kckylechen/Desktop/Quant_Analyzer_2026/docs/InProgress/KRONOS_800K_POSTMORTEM_PROBE.md`
```

---

## 7. MCP Tool Schema Update

The `tachi_wiki_write` tool's JSON schema should expose `references` as an optional array of strings:

```json
{
  "name": "tachi_wiki_write",
  "inputSchema": {
    "type": "object",
    "properties": {
      "title": { "type": "string" },
      "content": { "type": "string" },
      "project": { "type": "string" },
      "path": { "type": "string" },
      "force": { "type": "boolean" },
      "references": {
        "type": "array",
        "items": { "type": "string" },
        "description": "External references: URLs, absolute paths, or GitHub shorthands (#N, repo#N, owner/repo#N)"
      }
    }
  }
}
```

---

## 8. Testing Plan

### 8.1 Unit Tests for `validate_reference_format`

```rust
#[test]
fn test_valid_references() {
    assert!(validate_reference_format("https://example.com").is_ok());
    assert!(validate_reference_format("file:///path/to/file.md").is_ok());
    assert!(validate_reference_format("/Users/foo/project/README.md").is_ok());
    assert!(validate_reference_format("C:\\Users\\foo\\file.txt").is_ok());
    assert!(validate_reference_format("#69").is_ok());
    assert!(validate_reference_format("repo#69").is_ok());
    assert!(validate_reference_format("owner/repo#69").is_ok());
}

#[test]
fn test_invalid_references() {
    assert!(validate_reference_format("").is_err());
    assert!(validate_reference_format("  ").is_err());
    assert!(validate_reference_format("relative/path.md").is_err());  // relative path
    assert!(validate_reference_format("ftp://example.com").is_err());  // unsupported protocol
    assert!(validate_reference_format("just some text").is_err());
}
```

### 8.2 Integration Tests

- Write wiki entry with valid references → success, `source_refs` present in DB
- Write wiki entry with one invalid reference → error, no DB write
- Obsidian export includes `## References` section when `source_refs` non-empty
- Obsidian export omits `## References` when `source_refs` empty or absent
- Backward compatibility: write without `references` field → `source_refs` is empty array

---

## 9. Implementation Steps

| Step | File | Action | Est |
|------|------|--------|-----|
| 1 | `memory/wiki.rs` | Add `references: Vec<String>` with `#[serde(default)]` | 5 min |
| 2 | `facade.rs` | Add `references: Vec<String>` with `#[serde(default)]` | 5 min |
| 3 | `tools.rs` | Forward `references` from `TachiWikiParams` → `WikiWriteParams` | 5 min |
| 4 | `wiki_ops.rs` | Add `validate_reference_format` + `validate_references` helpers | 15 min |
| 5 | `wiki_ops.rs` | Call `validate_references` in write handler before DB op | 5 min |
| 6 | `wiki_ops.rs` | Inject `source_refs` into `wiki_metadata` JSON | 5 min |
| 7 | `wiki_ops.rs` | Add `## References` rendering in `markdown_for_obsidian` | 10 min |
| 8 | — | Update MCP tool schema JSON | 5 min |
| 9 | — | Write unit tests for validation | 15 min |
| 10 | — | Write integration tests for write + export | 20 min |
| 11 | — | `cargo test --package memory-server` | 5 min |

**Total estimate:** ~1.5 hours

---

## 10. Auto-Extraction via Qwen Secretary (Phase 2)

### 10.1 Problem

Agents often mention issues, docs, and wiki paths in natural language (e.g. "see #149 for the spec" or "as discussed in docs/engineering/architecture/agent-router-spec.md"), but don't explicitly populate `references`. Manual copy-paste is error-prone.

### 10.2 Solution: `qwen_secretary::ref_extractor`

A lightweight background module (local ollama `qwen2.5:32b`, configurable) that scans agent output and auto-suggests references:

```rust
// crates/tachi-server/src/qwen_secretary/ref_extractor.rs
pub async fn extract_references_from_text(text: &str) -> Vec<String> {
    // Qwen prompt: "Extract all GitHub issues, file paths, and wiki paths from this text.
    // Return JSON array of strings."
}
```

**Integration points:**
- `tachi_complete` → auto-extract from `notes`/`diff` → append to eval entry
- `handle_tachi_wiki_write` → if `references` is empty, run extractor on `text` → suggest
- `dispatch_ops` → agent stdout/stderr → extractor → auto-update kanban card context

**Behavior:**
- Always **suggest**, never **force** — human/agent confirms before write
- Toggle: `TACHI_SECRETARY_AUTO_EXTRACT=true` (default: true)
- Fallback: if Qwen unavailable, skip silently (no error)

### 10.3 Prompt Template

```
You are a reference extractor. Read the text below and extract all mentions of:
- GitHub issues: #N, repo#N, owner/repo#N, or https://github.com/.../issues/N
- Documents: paths ending in .md, especially docs/... or wiki/...
- Wiki entries: /wiki/... or /scratch/... paths

Return ONLY a JSON array of strings. No explanation.

Text:
---
{text}
---
```

---

## 11. Open Questions

| Question | Default | Decision |
|----------|---------|----------|
| Relative paths (`docs/foo.md`) — reject or resolve against workspace root? | **Reject** for now (strict absolute only) | — |
| Duplicate references in same entry — allow or dedup? | **Allow** (simplest) | — |
| Max reference count per entry? | **None** (unlimited) | — |
| Reference description/label (e.g. `[label](url)`)? | **Out of scope** (just raw strings) | — |
| Should `save_memory` also get `references`? | **Yes** — reuse same validator, add to `TachiMemoryParams` save action | Done |
| Auto-extract references from agent output via Qwen secretary? | **Phase 2** — `qwen_secretary::ref_extractor` auto-fills `references[]` from raw text | #150 |
