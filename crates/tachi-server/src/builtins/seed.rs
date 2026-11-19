use super::coding::builtin_coding_skills;
use super::superpowers::builtin_superpowers_skills;
use super::trading::builtin_trading_skills;
use super::waza::builtin_waza_skills;
use super::*;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, PartialEq, Eq)]
struct BuiltinCapabilitiesCacheKey {
    tachi_skills_root: Option<OsString>,
    cargo_manifest_dir: Option<OsString>,
    current_dir: Option<PathBuf>,
}

struct CachedBuiltinCapabilities {
    key: BuiltinCapabilitiesCacheKey,
    /// `Some(home)` when at least one skill lookup may have depended on `HOME`
    /// (explicit `TACHI_SKILLS_ROOT` set or a vendored skill fell back/stubbed),
    /// so cache hits also require `HOME` to match. `None` when all vendored
    /// skills resolved from process-invariant local roots (`cached_git_root`,
    /// `cwd`, or `CARGO_MANIFEST_DIR`).
    home_if_needed: Option<Option<OsString>>,
    caps: Vec<HubCapability>,
}

static BUILTIN_CAPABILITIES_CACHE: Mutex<Option<CachedBuiltinCapabilities>> = Mutex::new(None);

fn current_cache_inputs() -> (BuiltinCapabilitiesCacheKey, Option<OsString>) {
    let tachi_skills_root = std::env::var_os("TACHI_SKILLS_ROOT").filter(|v| !v.is_empty());
    let cargo_manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").filter(|v| !v.is_empty());
    let current_dir = std::env::current_dir().ok();
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty());
    (
        BuiltinCapabilitiesCacheKey {
            tachi_skills_root,
            cargo_manifest_dir,
            current_dir,
        },
        home,
    )
}

fn vendored_skills_all_resolved_locally(caps: &[HubCapability]) -> bool {
    let git_root = crate::path_utils::cached_git_root();
    let cwd = std::env::current_dir().ok();
    let manifest_workspace = std::env::var_os("CARGO_MANIFEST_DIR")
        .filter(|v| !v.is_empty())
        .map(|d| PathBuf::from(d).join("..").join(".."));
    caps.iter().all(|cap| {
        let Ok(def) = serde_json::from_str::<Value>(&cap.definition) else {
            return false;
        };
        let Some(source_path) = def.get("source_path").and_then(Value::as_str) else {
            return true;
        };
        if source_path.starts_with("native:") {
            return true;
        }
        if def.get("content_hash").and_then(Value::as_str).is_none() {
            return false;
        }
        git_root.is_some_and(|r| r.join(source_path).exists())
            || cwd.as_ref().is_some_and(|d| d.join(source_path).exists())
            || manifest_workspace
                .as_ref()
                .is_some_and(|w| w.join(source_path).exists())
    })
}

fn builtin_capabilities() -> Result<Vec<HubCapability>, String> {
    let (key, home) = current_cache_inputs();
    {
        let guard = BUILTIN_CAPABILITIES_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.as_ref() {
            let home_matches = match &entry.home_if_needed {
                Some(cached_home) => *cached_home == home,
                None => true,
            };
            if entry.key == key && home_matches {
                return Ok(entry.caps.clone());
            }
        }
    }

    let mut caps = builtin_coding_skills()?;
    caps.extend(builtin_trading_skills()?);
    caps.extend(builtin_superpowers_skills()?);
    caps.extend(builtin_waza_skills()?);

    // Only bypass `HOME` matching when `TACHI_SKILLS_ROOT` is unset and every
    // vendored skill resolved cleanly from local repo/workspace roots (so tests
    // that swap `HOME` via `TempHomeGuard` don't bypass caching on hosts with
    // the repo `skill/` corpus, while hosts without it still re-resolve per `HOME`).
    let home_if_needed =
        if key.tachi_skills_root.is_none() && vendored_skills_all_resolved_locally(&caps) {
            None
        } else {
            Some(home)
        };

    let mut guard = BUILTIN_CAPABILITIES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = Some(CachedBuiltinCapabilities {
        key,
        home_if_needed,
        caps: caps.clone(),
    });
    Ok(caps)
}

pub(crate) fn seed_builtin_capabilities(server: &MemoryServer) -> Result<(), String> {
    let caps = builtin_capabilities()?;
    let inserted_or_updated = server.with_global_store(|store| {
        let tx = store
            .connection()
            .unchecked_transaction()
            .map_err(|e| format!("begin builtin capability seed transaction: {e}"))?;
        store
            .hub_set_review(RETIRED_TRAJECTORY_DISTILLER_ID, "rejected", Some(false))
            .map_err(|e| {
                format!("retire removed builtin capability {RETIRED_TRAJECTORY_DISTILLER_ID}: {e}")
            })?;
        let mut changed = Vec::new();
        for cap in &caps {
            let existing = store
                .hub_get(&cap.id)
                .map_err(|e| format!("lookup builtin capability {}: {e}", cap.id))?;
            let replacement = match existing {
                None => Some(cap.clone()),
                Some(ref prev) => builtin_static_fields_differ(prev, cap)
                    .then(|| retain_operational_state(cap.clone(), prev)),
            };
            if let Some(replacement) = replacement {
                store
                    .hub_register(&replacement)
                    .map_err(|e| format!("register builtin capability {}: {e}", cap.id))?;
                changed.push(replacement);
            }
        }
        tx.commit()
            .map_err(|e| format!("commit builtin capability seed transaction: {e}"))?;
        Ok(changed)
    })?;

    server
        .unregister_skill_tool(RETIRED_TRAJECTORY_DISTILLER_ID)
        .map(|_| ())
        .map_err(|e| format!("unregister removed builtin skill tool: {e}"))?;

    for cap in inserted_or_updated {
        if cap.cap_type.eq_ignore_ascii_case("skill") && should_expose_skill_tool(&cap) {
            server
                .register_skill_tool(&cap)
                .map_err(|e| format!("register builtin skill tool {}: {e}", cap.id))?;
        }
    }

    Ok(())
}

/// A builtin definition is owned by the seed corpus, while these fields are
/// live operational state owned by the running Hub. Re-seeding a changed
/// definition must not erase ranking feedback or an operator's availability
/// choices (#1140).
fn retain_operational_state(mut seeded: HubCapability, existing: &HubCapability) -> HubCapability {
    seeded.enabled = existing.enabled;
    seeded.review_status = existing.review_status.clone();
    seeded.health_status = existing.health_status.clone();
    seeded.last_error = existing.last_error.clone();
    seeded.last_success_at = existing.last_success_at.clone();
    seeded.last_failure_at = existing.last_failure_at.clone();
    seeded.fail_streak = existing.fail_streak;
    seeded.active_version = existing.active_version.clone();
    seeded.exposure_mode = existing.exposure_mode.clone();
    seeded.uses = existing.uses;
    seeded.successes = existing.successes;
    seeded.failures = existing.failures;
    seeded.avg_rating = existing.avg_rating;
    seeded.last_used = existing.last_used.clone();
    seeded
}

fn builtin_static_fields_differ(existing: &HubCapability, seeded: &HubCapability) -> bool {
    existing.cap_type != seeded.cap_type
        || existing.name != seeded.name
        || existing.version != seeded.version
        || existing.description != seeded.description
        || existing.definition != seeded.definition
}
