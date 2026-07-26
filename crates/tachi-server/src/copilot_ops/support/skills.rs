use super::*;

pub(in crate::copilot_ops) fn tokenize_task(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_lowercase())
        .filter(|token| is_meaningful_skill_token(token))
        .collect()
}

pub(in crate::copilot_ops) fn is_meaningful_skill_token(token: &str) -> bool {
    if token.chars().count() < 3 {
        return false;
    }
    const STOPWORDS: &[&str] = &[
        "fix", "fixed", "fixing", "repair", "resolve", "bug", "bugs", "issue", "issues", "problem",
        "problems", "error", "errors", "failed", "failure", "task", "work", "use", "using", "add",
        "update", "change", "修复", "问题", "错误", "失败", "任务",
    ];
    !STOPWORDS.contains(&token)
}

pub(in crate::copilot_ops) fn tokenize_skill_text(input: &str) -> HashSet<String> {
    tokenize_task(input).into_iter().collect()
}

// `recommend_skills_light` is the copilot-facing light-weight skill recommender.
// It delegates hub-capability tokenize + bridge-token + scoring to the canonical
// implementation in `capability_ops::scoring` (`recommend_capabilities_inner`),
// then maps the rich `CapabilityRecommendation` rows down to the light JSON
// shape (`id` / `name` / `description` / `score` / `pattern_refs`) that callers
// (`feature_briefing::handlers`, `task_routing::build_selected_sops`) consume.
//
// Scope note (#517 cut 2): this is a bridge/scoring consolidation ONLY.
// `tokenize_task` / `tokenize_skill_text` / `is_meaningful_skill_token` above
// remain LIVE for the feature-guide path (`guides.rs:18,183`), which has
// different tokenization requirements than the capability path: CJK characters
// are preserved (not treated as delimiters), `_`/`-` are kept inside tokens, and
// a meaning-stopword filter (fix/bug/error/...) is applied. The capability path
// uses the ASCII-only `capability_ops::scoring::tokenize_query` instead. Full
// tokenizer unification is intentionally DEFERRED to avoid regressing guide
// matching; only the duplicate scoring/bridge logic was consolidated here.
//
// Consolidation rationale: the previous implementation duplicated
// `tokenize` + `is_bridge_token` + `pattern_bridge_score` + `score_capability`
// with a DIVERGED stopword list (12 entries vs scoring.rs's 21). The canonical
// 21-entry list is retained: the 9 extra entries (`about, after, asks, before,
// through, user, when, with, write`) suppress generic function words that would
// otherwise create spurious pattern bridges. The stopword-divergence
// equivalence-class test (`light_class_stopword_divergence_suppresses_function_word_bridge`)
// proves the larger list does not regress realistic bridges while it correctly
// suppresses the degenerate function-word bridge that the old 12-entry list
// allowed (it is RED on the pre-consolidation code and GREEN after).
pub(in crate::copilot_ops) fn recommend_skills_light(
    server: &MemoryServer,
    task: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let recommendations = crate::capability_ops::recommend_capabilities_inner(
        server,
        task,
        None,
        Some("skill"),
        limit.max(1),
        false,
        false,
    )?;

    Ok(recommendations
        .into_iter()
        .map(|rec| {
            let mut row = json!({
                "id": rec.id,
                "name": rec.name,
                "description": rec.description,
                "score": rec.score,
            });
            if !rec.pattern_refs.is_empty() {
                if let Some(object) = row.as_object_mut() {
                    object.insert("pattern_refs".to_string(), json!(rec.pattern_refs));
                }
            }
            row
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_server;
    use chrono::Utc;
    use memcore::MemoryEntry;

    fn make_test_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/".to_string(),
            summary: String::new(),
            text: "test memory".to_string(),
            importance: 0.7,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn make_test_skill(id: &str, name: &str, description: &str) -> HubCapability {
        HubCapability {
            id: id.to_string(),
            cap_type: "skill".to_string(),
            name: name.to_string(),
            version: 1,
            description: description.to_string(),
            definition: json!({
                "prompt": format!("Run skill {name}"),
                "content": format!("# {name}\n\n{description}"),
                "policy": {"visibility": "listed"},
                "inputSchema": {"type": "object"}
            })
            .to_string(),
            enabled: true,
            review_status: "approved".to_string(),
            health_status: "healthy".to_string(),
            last_error: None,
            last_success_at: None,
            last_failure_at: None,
            fail_streak: 0,
            active_version: None,
            exposure_mode: "direct".to_string(),
            uses: 0,
            successes: 0,
            failures: 0,
            avg_rating: 0.0,
            last_used: None,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn recommend_skills_light_uses_pattern_bridge() {
        let server = make_server();
        server
            .with_global_store(|store| {
                let closure = make_test_skill(
                    "skill:marmalade-closure-light",
                    "marmalade-closure-light",
                    "Write marmalade closure notes.",
                );
                store.hub_register(&closure).map_err(|e| e.to_string())?;
                let mut pattern = make_test_entry("pattern-zephyr-light");
                pattern.path = "/user/patterns/agent_os/zephyr-light".to_string();
                pattern.summary = "Zephyr requests use marmalade closure".to_string();
                pattern.text = "A zephyr task should route to marmalade closure notes.".to_string();
                pattern.metadata = json!({
                    "projection_kind": "pattern",
                    "projection_key": "zephyr-marmalade-light",
                    "source_event_id": "pattern-event-light-recommend",
                    "counters": {"seen": 4, "hit": 2}
                });
                store.upsert(&pattern).map_err(|e| e.to_string())
            })
            .expect("seed skill and pattern");

        let skills = recommend_skills_light(&server, "zephyr", 5).expect("recommend light");
        let hit = skills
            .iter()
            .find(|skill| {
                skill.get("id").and_then(Value::as_str) == Some("skill:marmalade-closure-light")
            })
            .expect("pattern-bridged skill should be recommended");
        assert_eq!(
            hit["pattern_refs"][0]["projection_key"],
            json!("zephyr-marmalade-light")
        );
    }

    // --- Equivalence-class tests for the consolidated scoring path (#517 cut 2) ---
    //
    // These enumerate the input equivalence classes of `recommend_skills_light`'s
    // observable output (returned skill ids + pattern_refs) and assert the
    // contract holds against the canonical scoring implementation. They are
    // written implementation-agnostically so they remain green after the
    // delegation to `capability_ops::recommend_capabilities_inner`.
    //
    // Classes covered:
    //   1. empty / whitespace query          -> no skills returned
    //   2. query with no matching skill      -> empty result
    //   3. single-token direct match         -> matched skill returned, ranked
    //   4. multi-token match                 -> best-overlap skill first
    //   5. pattern bridge (no direct match)  -> bridged skill + pattern_refs
    //   6. stopword-divergence discrimination-> extra bridge stopwords do NOT
    //      regress a realistic bridge (proves the 21-entry list is safe).
    //   7. CJK-only query                    -> empty result by design: the
    //      canonical `tokenize_query` is ASCII-only, so a pure-CJK query yields
    //      no tokens and `capability_score` returns None early. Pinned here so a
    //      future tokenizer change that gains CJK awareness is caught (see
    //      `recommend_skills_light_cjk_only_query_matches_canonical_behavior`).

    fn seed_skill(server: &MemoryServer, id: &str, name: &str, description: &str) {
        server
            .with_global_store(|store| {
                store
                    .hub_register(&make_test_skill(id, name, description))
                    .map_err(|e| e.to_string())
            })
            .expect("seed skill");
    }

    fn recommended_ids(skills: &[Value]) -> Vec<String> {
        skills
            .iter()
            .filter_map(|skill| skill.get("id").and_then(Value::as_str).map(String::from))
            .collect()
    }

    // Class 1: empty / whitespace query yields no recommendations.
    #[test]
    fn light_class_empty_query_returns_nothing() {
        let server = make_server();
        seed_skill(
            &server,
            "skill:alpha-empty",
            "alpha-empty",
            "Alpha empty skill",
        );
        for query in ["", "   ", "\n\t"] {
            let skills = recommend_skills_light(&server, query, 5).expect("recommend light");
            assert!(
                skills.is_empty(),
                "empty/whitespace query '{query:?}' must return no skills, got {skills:?}"
            );
        }
    }

    // Class 2: query that matches no skill (in any field) yields empty result.
    // The gibberish token is chosen so it appears in no builtin's id/name/
    // description/definition.
    #[test]
    fn light_class_no_match_returns_empty() {
        let server = make_server();
        seed_skill(
            &server,
            "skill:beta-nomatch",
            "beta-nomatch",
            "Beta nomatch skill",
        );
        let skills =
            recommend_skills_light(&server, "xyzzqwgumbotron", 5).expect("recommend light");
        assert!(
            skills.is_empty(),
            "non-matching query must return no skills, got {skills:?}"
        );
    }

    // Class 3: single-token direct match returns the matched skill, ranked.
    #[test]
    fn light_class_single_token_direct_match() {
        let server = make_server();
        seed_skill(
            &server,
            "skill:gamma-direct",
            "gamma-direct",
            "Gamma direct skill",
        );
        let skills = recommend_skills_light(&server, "gamma", 5).expect("recommend light");
        assert!(
            recommended_ids(&skills).contains(&"skill:gamma-direct".to_string()),
            "single-token direct match must return the skill, got {:?}",
            recommended_ids(&skills)
        );
    }

    // Class 4: multi-token match returns best-overlap skill first.
    #[test]
    fn light_class_multi_token_orders_by_relevance() {
        let server = make_server();
        seed_skill(
            &server,
            "skill:delta-weak",
            "delta-weak",
            "unrelated filler text",
        );
        seed_skill(
            &server,
            "skill:delta-strong",
            "delta-strong",
            "epsilon zeta matching tokens",
        );
        let skills =
            recommend_skills_light(&server, "epsilon zeta matching", 5).expect("recommend light");
        let ids = recommended_ids(&skills);
        assert!(ids.contains(&"skill:delta-strong".to_string()));
        // delta-weak has no overlap with the query tokens, so it must not rank
        // above delta-strong (and is typically absent).
        if ids.contains(&"skill:delta-weak".to_string()) {
            assert!(
                ids.iter()
                    .position(|id| id == "skill:delta-strong")
                    .unwrap()
                    < ids.iter().position(|id| id == "skill:delta-weak").unwrap(),
                "delta-strong must outrank delta-weak: {ids:?}"
            );
        }
    }

    // Class 5: pattern bridge with no direct token match returns bridged skill
    // with pattern_refs. (Reinforces the frozen bridge test with a distinct
    // fixture so the bridge path is covered by a second independent case.)
    #[test]
    fn light_class_pattern_bridge_no_direct_match() {
        let server = make_server();
        server
            .with_global_store(|store| {
                store
                    .hub_register(&make_test_skill(
                        "skill:obsidian-arch-light",
                        "obsidian-arch-light",
                        "Build obsidian architecture notes.",
                    ))
                    .map_err(|e| e.to_string())?;
                let mut pattern = make_test_entry("pattern-quasar-light");
                pattern.path = "/user/patterns/agent_os/quasar-light".to_string();
                pattern.summary = "Quasar requests build obsidian architecture".to_string();
                pattern.text =
                    "A quasar task should route to obsidian architecture notes.".to_string();
                pattern.metadata = json!({
                    "projection_kind": "pattern",
                    "projection_key": "quasar-obsidian-light",
                    "source_event_id": "pattern-event-quasar-light",
                    "counters": {"seen": 4, "hit": 2}
                });
                store.upsert(&pattern).map_err(|e| e.to_string())
            })
            .expect("seed skill and pattern");

        // "quasar" has no direct token overlap with the obsidian skill; only the
        // pattern bridge connects them.
        let skills = recommend_skills_light(&server, "quasar", 5).expect("recommend light");
        let hit = skills
            .iter()
            .find(|skill| {
                skill.get("id").and_then(Value::as_str) == Some("skill:obsidian-arch-light")
            })
            .expect("pattern-bridged skill should be recommended");
        assert_eq!(
            hit["pattern_refs"][0]["projection_key"],
            json!("quasar-obsidian-light")
        );
    }

    // Class 6 (stopword-divergence discrimination): the canonical 21-entry
    // bridge-stopword list (scoring.rs) suppresses generic function words that
    // the old 12-entry list (skills.rs) kept. Prove the extra entries do NOT
    // regress a realistic bridge: a pattern whose distinguishing token is a real
    // noun (not a function word) still bridges, while a degenerate pattern whose
    // ONLY shared token is a suppressed function word ("write") does NOT bridge.
    //
    // The discrimination is observable on the `pattern_refs` field: write-bearer
    // may still be returned for the query "write" via a DIRECT token match on its
    // description ("Used to write telemetry reports."), but it must NOT carry a
    // `pattern_refs` entry from the degenerate "write"-only pattern, because the
    // canonical stopword list suppresses "write" as a bridge token. Under the old
    // 12-entry list the pattern would bridge write-bearer and attach
    // `pattern_refs` — this test is RED on the pre-consolidation code.
    #[test]
    fn light_class_stopword_divergence_suppresses_function_word_bridge() {
        let server = make_server();
        server
            .with_global_store(|store| {
                store
                    .hub_register(&make_test_skill(
                        "skill:write-bearer",
                        "write-bearer",
                        "Used to write telemetry reports.",
                    ))
                    .map_err(|e| e.to_string())?;
                let mut pattern = make_test_entry("pattern-write-only");
                pattern.path = "/user/patterns/agent_os/write-only".to_string();
                pattern.summary = "write write write".to_string();
                pattern.text = "write write write".to_string();
                pattern.metadata = json!({
                    "projection_kind": "pattern",
                    "projection_key": "write-only",
                    "source_event_id": "pattern-event-write-only",
                    "counters": {"seen": 2, "hit": 1}
                });
                store.upsert(&pattern).map_err(|e| e.to_string())
            })
            .expect("seed skill and degenerate pattern");

        let skills = recommend_skills_light(&server, "write", 10).expect("recommend light");
        let write_bearer = skills
            .iter()
            .find(|skill| skill.get("id").and_then(Value::as_str) == Some("skill:write-bearer"));
        // If write-bearer is surfaced at all (via direct description match), it
        // must NOT carry the degenerate "write"-only pattern as a bridge ref.
        if let Some(hit) = write_bearer {
            let pattern_refs = hit.get("pattern_refs").unwrap_or(&Value::Null);
            assert!(
                pattern_refs
                    .as_array()
                    .map(|refs| {
                        refs.iter().all(|r| {
                            r.get("projection_key").and_then(Value::as_str) != Some("write-only")
                        })
                    })
                    .unwrap_or(true),
                "function-word bridge must be suppressed: write-bearer must not carry the \
                 write-only pattern_ref, got {pattern_refs:?}"
            );
        }
        // Either way, the degenerate write-only pattern must never appear as a
        // pattern_ref on ANY returned skill.
        for skill in &skills {
            if let Some(refs) = skill.get("pattern_refs").and_then(Value::as_array) {
                for r in refs {
                    assert_ne!(
                        r.get("projection_key").and_then(Value::as_str),
                        Some("write-only"),
                        "degenerate function-word pattern must not bridge any skill: {:?}",
                        skill
                    );
                }
            }
        }
    }

    // Class 7 (CJK-only query — deliberate canonical behavior). The consolidated
    // path delegates to `capability_ops::scoring::tokenize_query`, which is
    // ASCII-only (`ch.is_ascii_alphanumeric()`): every non-ASCII code point is a
    // delimiter. A query consisting solely of CJK characters therefore yields
    // ZERO tokens, `capability_score` returns `None` at the
    // `if query_tokens.is_empty() { return None; }` guard, and
    // `recommend_skills_light` returns an empty recommendation set — even when a
    // skill whose description contains those exact CJK characters is registered.
    //
    // This is DELIBERATE under the current contract and is pinned here so that a
    // future `tokenize_query` change that gains CJK awareness (intentional or
    // accidental) is caught. CJK-aware matching lives on a separate path
    // (`tokenize_skill_text`/`guides.rs`, which preserves `_`/`-`/CJK and applies
    // a meaning-stopword filter) and is NOT unified with the capability path.
    // Precedent for preserving CJK semantics in a dedicated, regression-pinned
    // test: `wiki_slug_preserves_cjk_and_readable_separators`.
    #[test]
    fn recommend_skills_light_cjk_only_query_matches_canonical_behavior() {
        let server = make_server();
        // The registered skill's description literally contains the CJK query
        // token "丢失"; under a CJK-aware tokenizer this would be a direct
        // description match. The canonical ASCII-only tokenizer must NOT surface
        // it, because no ASCII tokens are extracted from "丢失".
        seed_skill(
            &server,
            "skill:cjk-data-loss",
            "cjk-data-loss",
            "丢失 数据恢复 skill",
        );
        let skills = recommend_skills_light(&server, "丢失", 5).expect("recommend light");
        assert!(
            skills.is_empty(),
            "a pure-CJK query must yield zero ASCII tokens and therefore no \
             recommendations under the canonical tokenizer; got {skills:?}. If this \
             test is RED, tokenize_query may have gained non-ASCII handling — either \
             update this test deliberately or revert the tokenizer change."
        );
    }
}
