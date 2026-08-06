//! Build-target resource registration for the broker seat (#894 S2a/S2c).
//!
//! Moved out of `tachi-server::exec_env_ops` in #1702 carve 4 — the broker is
//! the one caller allowed to see a quarantined build-target row.

use memcore::{NewExecEnvResource, ResourceKind, ResourceState};

/// Resolve (registering if new) the ledger row for a build target dir the
/// **broker** is about to touch — the one caller that is allowed to see a
/// `quarantined` row, because clearing that quarantine is its job
/// (`build_broker::clear_target_for_reuse` -> `memcore::release_quarantine`).
///
/// Still fail-closed on the states nobody can safely proceed from: a target
/// mid-reclaim (`reclaiming`/`reclaim_failed`) is not something to quietly
/// build into — those bytes are being (or have been) freed under someone
/// else's transaction.
///
/// ## `reclaimed` ⇒ re-registrable (was an open S2a round-2 dependency; now
/// resolved)
///
/// A seat target holds no lease binding (that is the round-2 fix: only the
/// broker books it), so a *stale, unheld* seat target is legitimately
/// reclaimable by the orphan reaper. When that happens the row goes
/// `reclaimed` — and `memcore::insert_resource`'s revive semantics (#894 S2a)
/// now cover exactly this case: registering over a `reclaimed` `(path, kind)`
/// resurrects that row in place under a fresh `resource_id`, `state` back to
/// `active`. So a seat whose target got swept is not wedged: its next ticket
/// calls this function, sees `Reclaimed`, and re-registers the same path as a
/// virgin dir — same as the `None` (never-seen) arm below, just through the
/// revive path instead of a plain insert. Reclaiming an idle target dir costs
/// a cold rebuild, never a wedged seat.
pub fn ensure_resource_allow_quarantined(
    conn: &mut rusqlite::Connection,
    target_path: &str,
) -> Result<String, String> {
    let existing = memcore::find_resource_by_path(conn, target_path, ResourceKind::BuildTarget)
        .map_err(|e| e.to_string())?;
    match existing {
        Some(res)
            if matches!(
                res.state,
                ResourceState::Active | ResourceState::Quarantined
            ) =>
        {
            Ok(res.resource_id)
        }
        Some(res) if res.state == ResourceState::Reclaimed => {
            // Revived, not a plain insert: `insert_resource` resurrects the
            // `(path, kind)` row under a fresh id rather than erroring, so the
            // seat's next ticket gets a virgin-looking target instead of
            // wedging on the swept row.
            let resource_id = uuid::Uuid::new_v4().to_string();
            memcore::insert_resource(
                conn,
                &NewExecEnvResource {
                    resource_id: resource_id.clone(),
                    kind: ResourceKind::BuildTarget,
                    path: target_path.to_string(),
                    bytes: None,
                    created_at: String::new(),
                },
            )
            .map_err(|e| e.to_string())?;
            Ok(resource_id)
        }
        Some(res) => Err(format!(
            "build target '{target_path}' is '{}': a target mid-reclaim must not be built into \
             until that resolves (#894 S2a/S2c)",
            res.state.as_str()
        )),
        None => {
            let resource_id = uuid::Uuid::new_v4().to_string();
            memcore::insert_resource(
                conn,
                &NewExecEnvResource {
                    resource_id: resource_id.clone(),
                    kind: ResourceKind::BuildTarget,
                    path: target_path.to_string(),
                    bytes: None,
                    created_at: String::new(),
                },
            )
            .map_err(|e| e.to_string())?;
            Ok(resource_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::ResourceState;

    /// A seat's build target holds no lease binding (round-2 fix: only the
    /// broker books it), so an idle seat target is legitimately reclaimable by
    /// the orphan reaper — the row can go `reclaimed` out from under a seat
    /// that still thinks it owns that path. `insert_resource`'s revive
    /// semantics (#894 S2a) are what keep the seat from wedging on that: the
    /// next ticket's `ensure_resource_allow_quarantined` call on the same path
    /// must come back `Ok` with a fresh, `active` resource_id — not an error
    /// that leaves the seat stuck on the swept row (the open dependency this
    /// module used to carry against S2a round-2).
    #[test]
    fn a_reclaimed_seat_target_is_revived_not_wedged() {
        let mut store = memcore::MemoryStore::open_in_memory().expect("in-memory store");
        let conn = store.connection_mut();

        let first_id =
            ensure_resource_allow_quarantined(conn, "/seat/target").expect("first registration");

        // The orphan reaper sweeps the idle target: nobody held a binding on
        // it, so the reclaim goes through clean.
        let outcome =
            memcore::reclaim_resource(conn, &first_id, Some("orphan sweep"), |_res| Ok(0))
                .expect("reclaim");
        assert!(matches!(
            outcome,
            memcore::ResourceReclaimOutcome::Reclaimed { .. }
        ));
        assert_eq!(
            memcore::get_resource(conn, &first_id)
                .unwrap()
                .unwrap()
                .state,
            ResourceState::Reclaimed
        );

        // The seat's next ticket asks for the same path again — it must NOT
        // error or wedge; it must come back as a fresh, active resource, and
        // the build proceeds instead of stalling.
        let second_id = ensure_resource_allow_quarantined(conn, "/seat/target")
            .expect("a swept seat target must revive, not wedge the seat (#894 S2a/S2c)");
        assert_ne!(
            second_id, first_id,
            "revive mints a fresh resource_id (S2a's RegisterOutcome::Revived contract)"
        );
        let revived = memcore::get_resource(conn, &second_id).unwrap().unwrap();
        assert_eq!(revived.state, ResourceState::Active);
        assert_eq!(revived.path, "/seat/target");
    }
}
