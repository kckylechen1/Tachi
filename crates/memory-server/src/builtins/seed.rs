use super::coding::builtin_coding_skills;
use super::mcp::builtin_mcp_capabilities;
use super::superpowers::builtin_superpowers_skills;
use super::trading::builtin_trading_skills;
use super::trajectory::builtin_trajectory_distiller;
use super::waza::builtin_waza_skills;
use super::*;

fn builtin_capabilities() -> Result<Vec<HubCapability>, String> {
    let mut caps = vec![builtin_trajectory_distiller()?];
    caps.extend(builtin_coding_skills()?);
    caps.extend(builtin_trading_skills()?);
    caps.extend(builtin_superpowers_skills()?);
    caps.extend(builtin_waza_skills()?);
    caps.extend(builtin_mcp_capabilities()?);
    Ok(caps)
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
    let caps = builtin_capabilities()?;
    let inserted_or_updated = server.with_global_store(|store| {
        let mut changed = Vec::new();
        for cap in &caps {
            let existing = store
                .hub_get(&cap.id)
                .map_err(|e| format!("lookup builtin capability {}: {e}", cap.id))?;
            let should_upsert = match existing {
                None => true,
                Some(ref prev) => {
                    prev.definition != cap.definition
                        || prev.description != cap.description
                        || prev.version != cap.version
                        || prev.enabled != cap.enabled
                        || prev.review_status != cap.review_status
                        || prev.health_status != cap.health_status
                }
            };
            if should_upsert {
                store
                    .hub_register(cap)
                    .map_err(|e| format!("register builtin capability {}: {e}", cap.id))?;
                changed.push(cap.clone());
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
