use super::coding::builtin_coding_skills;
use super::mcp::builtin_mcp_capabilities;
use super::superpowers::builtin_superpowers_skills;
use super::trading::builtin_trading_skills;
use super::waza::builtin_waza_skills;
use super::*;

fn builtin_capabilities() -> Result<Vec<HubCapability>, String> {
    let mut caps = builtin_coding_skills()?;
    caps.extend(builtin_trading_skills()?);
    caps.extend(builtin_superpowers_skills()?);
    caps.extend(builtin_waza_skills()?);
    caps.extend(builtin_mcp_capabilities()?);
    Ok(caps)
}

fn retire_removed_builtins(server: &MemoryServer) -> Result<(), String> {
    server.with_global_store(|store| {
        store
            .hub_set_review(RETIRED_TRAJECTORY_DISTILLER_ID, "rejected", Some(false))
            .map(|_| ())
            .map_err(|e| {
                format!(
                    "retire removed builtin capability {RETIRED_TRAJECTORY_DISTILLER_ID}: {e}"
                )
            })
    })?;
    server
        .unregister_skill_tool(RETIRED_TRAJECTORY_DISTILLER_ID)
        .map(|_| ())
        .map_err(|e| format!("unregister removed builtin skill tool: {e}"))
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

fn seed_builtin_sandbox_policy(server: &MemoryServer, capability_id: &str) -> Result<(), String> {
    server.with_global_store(|store| {
        let existing = store
            .get_sandbox_policy(capability_id)
            .map_err(|e| format!("lookup builtin sandbox policy {capability_id}: {e}"))?;
        if existing.is_none() {
            store
                .set_sandbox_policy(
                    capability_id,
                    "process",
                    "[]",
                    "[]",
                    "[]",
                    "[]",
                    10_000,
                    30_000,
                    2,
                    true,
                )
                .map_err(|e| format!("seed builtin sandbox policy {capability_id}: {e}"))?;
        }
        Ok(())
    })
}

pub(crate) fn seed_builtin_capabilities(server: &MemoryServer) -> Result<(), String> {
    retire_removed_builtins(server)?;
    let caps = builtin_capabilities()?;
    let inserted_or_updated = server.with_global_store(|store| {
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
        Ok(changed)
    })?;

    for cap in &caps {
        if cap.cap_type.eq_ignore_ascii_case("mcp") {
            seed_builtin_sandbox_policy(server, &cap.id)?;
        }
    }

    for cap in inserted_or_updated {
        if cap.cap_type.eq_ignore_ascii_case("skill") && should_expose_skill_tool(&cap) {
            server
                .register_skill_tool(&cap)
                .map_err(|e| format!("register builtin skill tool {}: {e}", cap.id))?;
        }
    }

    Ok(())
}
