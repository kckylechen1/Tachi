use super::*;

// ─── Stage / meta-skill mapping ──────────────────────────────────────────────

/// MVP hardcoded stage → meta skill SOP file path (relative to repo root).
pub(super) fn meta_skill_for_stage(stage: &str) -> Option<&'static str> {
    match stage {
        "brainstorm" => Some("skill/superpowers/skills/brainstorming/SKILL.md"),
        "plan" => Some("skill/superpowers/skills/writing-plans/SKILL.md"),
        "dispatch" => Some("skill/superpowers/skills/executing-plans/SKILL.md"),
        "review" => Some("skill/superpowers/skills/requesting-code-review/SKILL.md"),
        "ship" => Some("skill/superpowers/skills/finishing-a-development-branch/SKILL.md"),
        _ => None,
    }
}

/// Stages that bear a skill gate and create/advance a flow run.
pub(super) const STAGE_ACTIONS: &[&str] = &["brainstorm", "plan", "dispatch", "review", "ship"];

// ─── Meta skill injection ────────────────────────────────────────────────────

/// Resolve the meta skill SOP file, falling back through several roots.
pub(super) fn resolve_meta_skill(rel_path: &str) -> Option<PathBuf> {
    // 1. repo root (git toplevel)
    if let Some(root) = cached_git_root() {
        let p = root.join(rel_path);
        if p.exists() {
            return Some(p);
        }
    }
    // 2. cwd
    let p = PathBuf::from(rel_path);
    if p.exists() {
        return Some(p);
    }
    // 3. cargo manifest dir (for tests)
    if let Ok(d) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(d).join("..").join("..").join(rel_path);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[derive(Debug)]
pub(super) struct InjectionResult {
    pub(super) required: bool,
    pub(super) rel_path: Option<String>,
    pub(super) source_path: Option<String>,
    pub(super) injected_path: Option<String>,
    pub(super) content_hash: Option<String>,
    pub(super) loaded: bool,
    pub(super) warning: Option<String>,
}

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
        };
    }
    let resolved = match resolve_meta_skill(rel) {
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
    }
}
