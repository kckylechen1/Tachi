use super::*;

fn now() -> String {
    Utc::now().to_rfc3339()
}

/// Content source for a builtin skill table entry.
///
/// `Vendored` corpora (superpowers / waza) live outside the git tree since
/// #895 (`skill/` is a host-absolute symlink into the central vendored-skills
/// library) — so their content is resolved at seed time, not compile time.
/// `Native` entries are Tachi-authored workflow-gate text baked directly into
/// the binary; they carry no filesystem dependency and need no resolution.
#[derive(Debug, Clone, Copy)]
pub(super) enum SkillContentSource {
    Vendored(&'static str),
    Native(&'static str),
}

/// Resolved content for one builtin skill table entry: `(content,
/// resolved_path, content_hash, source_path)`. `content` is `String::new()`
/// and `content_hash` is `None` for a stub (degradation policy b — see
/// `resolve_skill_source`).
pub(super) fn resolve_skill_content_source(
    name: &str,
    source: &SkillContentSource,
) -> (String, Option<String>, Option<String>, String) {
    match source {
        SkillContentSource::Vendored(rel_path) => {
            let (content, resolved_path, hash) = resolve_skill_source(rel_path);
            (
                content.unwrap_or_default(),
                resolved_path,
                hash,
                (*rel_path).to_string(),
            )
        }
        SkillContentSource::Native(text) => (
            (*text).to_string(),
            None,
            Some(crate::utils::stable_hash(text)),
            format!("native:{name}"),
        ),
    }
}

/// Resolve a vendored-skill source file at seed time via the same runtime
/// resolver the flow-stage injection path uses, so a seeded builtin skill
/// capability's content always matches what a live flow-stage injection would
/// load from the same `rel_path` — no compile-time `include_str!` against `skill/`,
/// no host-path lock-in (kckylechen1/tachi#895 made `skill/` a host-absolute
/// symlink, so compile-time embedding is neither hermetic nor portable).
///
/// Degradation policy (owner-ratified, option b — graceful, not hard-error):
/// when the central library is absent or `rel_path` can't be resolved, this
/// returns `(None, None, None)` instead of an `Err`. The caller seeds a stub
/// capability (`content_hash: null`) rather than failing the whole seed
/// pass — a library-less host must still boot.
pub(super) fn resolve_skill_source(
    rel_path: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let resolved = match crate::skill_source_resolver::resolve_vendored_skill_path(rel_path) {
        Some(p) => p,
        None => {
            tracing::warn!(
                rel_path = %rel_path,
                "builtin skill seed: vendored skill source not resolvable in any known root \
                 (repo root / cwd / cargo manifest dir / central vendored-skills library — is \
                 $TACHI_SKILLS_ROOT or ~/.agents/vendored-skills mounted?); seeding a stub \
                 capability instead of failing seed"
            );
            return (None, None, None);
        }
    };
    match std::fs::read_to_string(&resolved) {
        Ok(content) => {
            let hash = crate::utils::stable_hash(&content);
            (
                Some(content),
                Some(resolved.to_string_lossy().to_string()),
                Some(hash),
            )
        }
        Err(e) => {
            tracing::warn!(
                rel_path = %rel_path,
                resolved = %resolved.display(),
                error = %e,
                "builtin skill seed: resolved vendored skill path unreadable; seeding a stub \
                 capability instead of failing seed"
            );
            (None, Some(resolved.to_string_lossy().to_string()), None)
        }
    }
}

pub(super) fn make_skill_capability(
    id: &str,
    name: &str,
    description: &str,
    definition: Value,
) -> Result<HubCapability, String> {
    let timestamp = now();
    Ok(HubCapability {
        id: id.to_string(),
        cap_type: "skill".to_string(),
        name: name.to_string(),
        version: 1,
        description: description.to_string(),
        definition: serde_json::to_string(&definition)
            .map_err(|e| format!("serialize builtin skill {id}: {e}"))?,
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
        created_at: timestamp.clone(),
        updated_at: timestamp,
    })
}
