//! Multi-evidence pack + conversation scoping for search hits (tachi#2124).
//!
//! Complements vector MMR in memcore with **path-leaf diversity** and optional
//! conversation/path scope so multi-hop QA gets complementary turns instead of
//! near-duplicate hits, without touching temporal/`as_of` ranking (#2125).

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Options for assembling a bounded multi-evidence pack.
#[derive(Debug, Clone)]
pub(crate) struct MultiEvidencePackOpts {
    pub top_k: usize,
    pub enabled: bool,
    pub auto_conversation_scope: bool,
    /// Explicit conversation scope and/or effective path prefix (already merged).
    pub explicit_scope: Option<String>,
}

/// Parent path used as a conversation/thread key (drop last segment).
pub(crate) fn conversation_key(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() || path == "/" {
        return "/".to_string();
    }
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(idx) => trimmed[..idx].to_string(),
        None => trimmed.to_string(),
    }
}

/// Last path segment (dialog/turn id for LoCoMo-style paths).
pub(crate) fn path_leaf(path: &str) -> String {
    let path = path.trim().trim_end_matches('/');
    match path.rfind('/') {
        Some(idx) => path[idx + 1..].to_string(),
        None => path.to_string(),
    }
}

fn row_path(row: &Value) -> &str {
    row.get("path").and_then(Value::as_str).unwrap_or("")
}

/// Infer dominant conversation key from the first `probe` ranked hits.
pub(crate) fn infer_conversation_scope(rows: &[Value], probe: usize) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let probe = probe.clamp(1, rows.len());
    let mut counts: HashMap<String, usize> = HashMap::new();
    for row in rows.iter().take(probe) {
        let key = conversation_key(row_path(row));
        if key == "/" {
            continue;
        }
        *counts.entry(key).or_insert(0) += 1;
    }
    let (key, count) = counts.into_iter().max_by_key(|(_, c)| *c)?;
    // ≥50% of probed hits share the key.
    if count * 2 >= probe {
        Some(key)
    } else {
        None
    }
}

fn in_scope(path: &str, scope: &str) -> bool {
    let path = path.trim();
    let scope = scope.trim().trim_end_matches('/');
    if scope.is_empty() {
        return true;
    }
    path == scope
        || path.starts_with(&(scope.to_string() + "/"))
        || path.trim_end_matches('/') == scope
}

/// Reorder/select a path-diverse multi-evidence pack in place.
///
/// When `enabled` is false, only truncates to `top_k` (no annotations).
pub(crate) fn apply_multi_evidence_pack(rows: &mut Vec<Value>, opts: &MultiEvidencePackOpts) {
    let top_k = opts.top_k.max(1);
    if rows.is_empty() {
        return;
    }
    if !opts.enabled {
        rows.truncate(top_k);
        return;
    }

    let inferred = if opts.auto_conversation_scope && opts.explicit_scope.is_none() {
        infer_conversation_scope(rows, 5.min(rows.len())).and_then(|scope| {
            // Soft scope must not demote a higher-ranked out-of-scope hit
            // (ops_audit cross-library #899: project decision at rank 1 with
            // `/wiki` majority in top-5 must stay rank 1).
            let top = rows.first().map(|r| row_path(r)).unwrap_or("");
            if in_scope(top, &scope) {
                Some(scope)
            } else {
                None
            }
        })
    } else {
        None
    };
    let scope_applied = opts.explicit_scope.clone().or_else(|| inferred.clone());
    let hard_scope = opts.explicit_scope.is_some();

    let mut selected: Vec<Value> = Vec::with_capacity(top_k);
    let mut selected_leaves: HashSet<String> = HashSet::new();
    let mut used: HashSet<usize> = HashSet::new();

    let prefer_scope = scope_applied.as_deref();

    // Pass 1: novel path leaves, preferring in-scope hits (rank order).
    while selected.len() < top_k {
        let mut picked: Option<usize> = None;
        for (idx, row) in rows.iter().enumerate() {
            if used.contains(&idx) {
                continue;
            }
            let path = row_path(row);
            if hard_scope {
                if let Some(scope) = prefer_scope {
                    if !in_scope(path, scope) {
                        continue;
                    }
                }
            }
            let leaf = path_leaf(path);
            if selected_leaves.contains(&leaf) && !leaf.is_empty() {
                continue;
            }
            if let Some(scope) = prefer_scope {
                // Soft: skip out-of-scope while in-scope novel leaves remain.
                if !in_scope(path, scope) {
                    let has_in_scope_novel = rows.iter().enumerate().any(|(j, r)| {
                        if used.contains(&j) {
                            return false;
                        }
                        let p = row_path(r);
                        in_scope(p, scope) && !selected_leaves.contains(&path_leaf(p))
                    });
                    if has_in_scope_novel {
                        continue;
                    }
                }
            }
            picked = Some(idx);
            break;
        }
        let Some(idx) = picked else {
            break;
        };
        used.insert(idx);
        let path = row_path(&rows[idx]).to_string();
        selected_leaves.insert(path_leaf(&path));
        selected.push(rows[idx].clone());
    }

    // Pass 2: fill remaining slots in rank order (allow leaf repeats / out-of-scope).
    for (idx, row) in rows.iter().enumerate() {
        if selected.len() >= top_k {
            break;
        }
        if used.contains(&idx) {
            continue;
        }
        if hard_scope {
            if let Some(scope) = prefer_scope {
                if !in_scope(row_path(row), scope) {
                    continue;
                }
            }
        }
        used.insert(idx);
        selected.push(row.clone());
    }

    // Annotate pack metadata.
    for (rank, row) in selected.iter_mut().enumerate() {
        if let Some(obj) = row.as_object_mut() {
            let path = obj
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            obj.insert("conversation_key".into(), json!(conversation_key(&path)));
            obj.insert("pack_rank".into(), json!(rank));
            if let Some(scope) = scope_applied.as_deref() {
                obj.insert("scope_applied".into(), json!(scope));
            }
        }
    }

    *rows = selected;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hit(path: &str, id: &str) -> Value {
        json!({
            "id": id,
            "path": path,
            "text": format!("body-{id}"),
            "excerpt": format!("ex-{id}"),
            "score": { "final_score": 0.9 }
        })
    }

    #[test]
    fn conversation_key_drops_leaf() {
        assert_eq!(
            conversation_key("/facts/locomo/conv-26/D1-3"),
            "/facts/locomo/conv-26"
        );
        assert_eq!(path_leaf("/facts/locomo/conv-26/D1-3"), "D1-3");
    }

    #[test]
    fn pack_keeps_complementary_path_leaves() {
        let mut rows = vec![
            hit("/facts/locomo/conv-26/D1-1", "a"),
            hit("/facts/locomo/conv-26/D1-1", "a-dup"), // same leaf
            hit("/facts/locomo/conv-26/D2-4", "b"),
            hit("/facts/locomo/conv-26/D3-2", "c"),
            hit("/facts/locomo/conv-26/D1-1", "a-dup2"),
        ];
        apply_multi_evidence_pack(
            &mut rows,
            &MultiEvidencePackOpts {
                top_k: 3,
                enabled: true,
                auto_conversation_scope: true,
                explicit_scope: None,
            },
        );
        assert_eq!(rows.len(), 3);
        let leaves: Vec<_> = rows.iter().map(|r| path_leaf(row_path(r))).collect();
        assert_eq!(leaves, vec!["D1-1", "D2-4", "D3-2"]);
        assert_eq!(rows[0]["pack_rank"], json!(0));
        assert_eq!(rows[0]["id"], json!("a"));
        assert!(rows.iter().all(|r| r.get("a-dup").is_none()));
        assert!(!rows.iter().any(|r| r["id"] == json!("a-dup")));
    }

    #[test]
    fn soft_auto_scope_prefers_dominant_conversation() {
        let mut rows = vec![
            hit("/facts/locomo/conv-26/D1-1", "26a"),
            hit("/facts/locomo/conv-26/D1-2", "26b"),
            hit("/facts/locomo/conv-26/D1-3", "26c"),
            hit("/facts/locomo/conv-48/D9-1", "48a"),
            hit("/facts/locomo/conv-48/D9-2", "48b"),
        ];
        apply_multi_evidence_pack(
            &mut rows,
            &MultiEvidencePackOpts {
                top_k: 3,
                enabled: true,
                auto_conversation_scope: true,
                explicit_scope: None,
            },
        );
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| row_path(r).contains("conv-26")));
        assert_eq!(rows[0]["scope_applied"], json!("/facts/locomo/conv-26"));
    }

    #[test]
    fn explicit_scope_hard_filters_pack() {
        let mut rows = vec![
            hit("/facts/locomo/conv-26/D1-1", "26a"),
            hit("/facts/locomo/conv-48/D9-1", "48a"),
            hit("/facts/locomo/conv-26/D1-2", "26b"),
        ];
        apply_multi_evidence_pack(
            &mut rows,
            &MultiEvidencePackOpts {
                top_k: 5,
                enabled: true,
                auto_conversation_scope: false,
                explicit_scope: Some("/facts/locomo/conv-26".into()),
            },
        );
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| row_path(r).contains("conv-26")));
        assert!(!rows.iter().any(|r| r["id"] == json!("48a")));
    }

    #[test]
    fn opt_out_preserves_rank_order() {
        let mut rows = vec![
            hit("/facts/locomo/conv-26/D1-1", "a"),
            hit("/facts/locomo/conv-26/D1-1", "a-dup"),
            hit("/facts/locomo/conv-48/D9-1", "b"),
        ];
        apply_multi_evidence_pack(
            &mut rows,
            &MultiEvidencePackOpts {
                top_k: 2,
                enabled: false,
                auto_conversation_scope: true,
                explicit_scope: None,
            },
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], json!("a"));
        assert_eq!(rows[1]["id"], json!("a-dup"));
        assert!(rows[0].get("pack_rank").is_none());
    }

    #[test]
    fn infer_scope_requires_majority() {
        let rows = vec![
            hit("/facts/locomo/conv-26/D1-1", "a"),
            hit("/facts/locomo/conv-48/D9-1", "b"),
            hit("/facts/locomo/conv-99/D2-1", "c"),
        ];
        assert!(infer_conversation_scope(&rows, 3).is_none());
    }

    #[test]
    fn soft_scope_does_not_demote_rank1_out_of_inferred_scope() {
        // Cross-library dilution (#899): project decision is rank-1; three /wiki
        // hits make /wiki a majority of top-5, but soft scope must not apply.
        let mut rows = vec![
            hit("/notes/project/decisions", "ops-xlib-project-decision"),
            hit("/wiki/memory-operating-system", "ops-xlib-global-roadmap"),
            hit("/wiki/phase2-precision", "ops-xlib-global-phase2"),
            hit("/wiki/quarterly-review", "ops-xlib-global-review"),
            hit("/guide/ranking-review", "ops-xlib-global-guide"),
        ];
        apply_multi_evidence_pack(
            &mut rows,
            &MultiEvidencePackOpts {
                top_k: 5,
                enabled: true,
                auto_conversation_scope: true,
                explicit_scope: None,
            },
        );
        assert_eq!(rows[0]["id"], json!("ops-xlib-project-decision"));
        assert!(rows[0].get("scope_applied").is_none());
    }
}
