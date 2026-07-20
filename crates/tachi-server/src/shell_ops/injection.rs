use super::*;

// ─── Stage / meta-skill mapping ──────────────────────────────────────────────

/// MVP hardcoded stage → meta skill SOP file path (relative to repo root).
pub(super) fn meta_skill_for_stage(stage: &str) -> Option<&'static str> {
    match stage {
        "dispatch" => Some("skill/superpowers/skills/executing-plans/SKILL.md"),
        _ => None,
    }
}

// ─── Meta skill injection ────────────────────────────────────────────────────

#[derive(Debug)]
pub(super) struct InjectionResult {
    pub(super) required: bool,
    pub(super) rel_path: Option<String>,
    pub(super) source_path: Option<String>,
    pub(super) injected_path: Option<String>,
    pub(super) content_hash: Option<String>,
    pub(super) loaded: bool,
    pub(super) warning: Option<String>,
    /// Machine-readable classification of *why* injection did not `loaded`.
    /// `None` when injection succeeded (or wasn't required for this stage).
    /// Callers (e.g. the poke shell probe, kckylechen1/tachi#1058) use this to
    /// tell "host skill roots aren't mounted on this runner" — the one
    /// tolerated failure mode — apart from real I/O failures that should
    /// still fail loudly.
    pub(super) failure_class: Option<&'static str>,
}

/// `failure_class` value for: the meta-skill file could not be resolved in
/// any known root (repo root / cwd / `CARGO_MANIFEST_DIR` / central vendored
/// library). This is the *only* failure mode CI runners without host skill
/// roots are expected to hit, and the only one callers should tolerate.
pub(super) const FAILURE_CLASS_MISSING_SOURCE_ROOTS: &str = "missing_source_roots";

pub(super) async fn inject_meta_skill(stage: &str, run_dir: &Path) -> InjectionResult {
    let rel = match meta_skill_for_stage(stage) {
        Some(r) => r,
        None => {
            return InjectionResult {
                required: false,
                rel_path: None,
                source_path: None,
                injected_path: None,
                content_hash: None,
                loaded: false,
                warning: None,
                failure_class: None,
            };
        }
    };
    let injected_dir = run_dir.join("injected");
    if let Err(e) = tokio::fs::create_dir_all(&injected_dir).await {
        return InjectionResult {
            required: true,
            rel_path: Some(rel.to_string()),
            source_path: None,
            injected_path: None,
            content_hash: None,
            loaded: false,
            warning: Some(format!("create injected dir failed: {e}")),
            failure_class: Some("create_dir_failed"),
        };
    }
    let resolved = match crate::skill_source_resolver::resolve_vendored_skill_path(rel) {
        Some(p) => p,
        None => {
            return InjectionResult {
                required: true,
                rel_path: Some(rel.to_string()),
                source_path: None,
                injected_path: None,
                content_hash: None,
                loaded: false,
                warning: Some(format!(
                    "meta skill file '{}' not found in any known root",
                    rel
                )),
                failure_class: Some(FAILURE_CLASS_MISSING_SOURCE_ROOTS),
            };
        }
    };
    let bytes = match tokio::fs::read(&resolved).await {
        Ok(b) => b,
        Err(e) => {
            return InjectionResult {
                required: true,
                rel_path: Some(rel.to_string()),
                source_path: Some(resolved.to_string_lossy().to_string()),
                injected_path: None,
                content_hash: None,
                loaded: false,
                warning: Some(format!("read meta skill failed: {e}")),
                failure_class: Some("read_failed"),
            };
        }
    };
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let digest = format!("{:016x}", hasher.finish());
    // Flatten path: `superpowers-<stage>.md`
    let basename = format!("superpowers-{}.md", stage);
    let target = injected_dir.join(&basename);
    if let Err(e) = tokio::fs::write(&target, &bytes).await {
        return InjectionResult {
            required: true,
            rel_path: Some(rel.to_string()),
            source_path: Some(resolved.to_string_lossy().to_string()),
            injected_path: None,
            content_hash: Some(digest),
            loaded: false,
            warning: Some(format!("write injected meta skill failed: {e}")),
            failure_class: Some("write_failed"),
        };
    }
    InjectionResult {
        required: true,
        rel_path: Some(rel.to_string()),
        source_path: Some(resolved.to_string_lossy().to_string()),
        injected_path: Some(target.to_string_lossy().to_string()),
        content_hash: Some(digest),
        loaded: true,
        warning: None,
        failure_class: None,
    }
}
