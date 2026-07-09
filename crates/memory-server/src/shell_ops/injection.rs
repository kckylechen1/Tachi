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

/// Central vendored-skills library root.
///
/// Order:
/// 1. `$TACHI_SKILLS_ROOT`
/// 2. `$HOME/.agents/vendored-skills`
///
/// The vendored skill corpora (superpowers / waza / …) are mounted from this
/// central library so they no longer have to live in every git worktree /
/// clone. A `rel_path` here is `<central>/skill/<corpus>/…`, i.e. the central
/// root already contains the `skill/` prefix the rel_path carries. This is a
/// robustness fallback: on this host the in-repo `skill/` entry is an absolute
/// symlink into the same central library, so paths 1–3 normally already win.
fn central_skills_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TACHI_SKILLS_ROOT") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home).join(".agents").join("vendored-skills"));
        }
    }
    None
}

/// A meta-skill `rel_path` must be repo-relative. Reject absolute paths and any
/// `..` traversal component so neither a caller nor a future stage mapping can
/// coax the resolver into reading outside a known skills root — defence in
/// depth, independent of the calling convention.
fn is_safe_meta_skill_rel_path(rel_path: &str) -> bool {
    let p = Path::new(rel_path);
    if p.is_absolute() {
        return false;
    }
    !p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Resolve a vendored-skill file by its repo-relative path (e.g.
/// `skill/superpowers/skills/writing-plans/SKILL.md`), falling back through
/// several roots.
///
/// The central vendored-skills library is checked **last** so an in-repo copy
/// (or the in-repo symlink into that same library) keeps taking priority; a
/// fresh worktree / clone with no `skill/` on disk falls through to the central
/// library.
pub(super) fn resolve_meta_skill(rel_path: &str) -> Option<PathBuf> {
    // Defence in depth: only ever resolve repo-relative paths.
    if !is_safe_meta_skill_rel_path(rel_path) {
        return None;
    }
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
    // 4. central vendored-skills library (mounted, not tracked in-repo)
    if let Some(central) = central_skills_root() {
        let p = central.join(rel_path);
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
