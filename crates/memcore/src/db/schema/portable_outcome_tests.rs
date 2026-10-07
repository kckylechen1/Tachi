//! D7 T7: literal old/B outcomes and phase/side-effect receipts.
//! Shape corruption fixtures are deliberately synthetic; historical lineage is
//! tested separately by the classification fixtures.

use super::portable_version_tests::{backups, context, fixture, sentinels, stamp_and_mark};
use super::{inventory::query_rows, pre_b_39, test_hooks};
use crate::db::{DbOpenContext, StoreProfile};
use crate::{MemoryError, MemoryStore};
use rusqlite::Connection;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Variant {
    Valid,
    Role,
    Profile,
    Sentinel,
    Requirement,
    Shape,
    ShapeRole,
    IntegrityRole,
    Outbox,
}
const VARIANTS: &[Variant] = &[
    Variant::Valid,
    Variant::Role,
    Variant::Profile,
    Variant::Sentinel,
    Variant::Requirement,
    Variant::Shape,
    Variant::ShapeRole,
    Variant::IntegrityRole,
    Variant::Outbox,
];

fn code(error: &MemoryError) -> &'static str {
    match error {
        MemoryError::SchemaMigrationOptInRequired { .. } => "opt_in",
        MemoryError::StoreProfileMismatch { .. } | MemoryError::StoreProfileNotExact { .. } => {
            "requirement"
        }
        MemoryError::CurrentSchemaIncomplete { .. } => "shape",
        MemoryError::InvalidArg(message) if message.contains("newer than") => "newer",
        MemoryError::InvalidArg(message) if message.contains("sentinel") => "sentinel",
        MemoryError::InvalidArg(message)
            if message.contains("store identity stamp") || message.contains("store profile") =>
        {
            "identity"
        }
        MemoryError::InvalidArg(message) if message.contains("outbox") => "outbox",
        MemoryError::InvalidArg(message) if message.contains("delivery") => "delivery",
        error => panic!("unclassified outcome: {error:?}"),
    }
}

#[derive(Debug, PartialEq)]
struct Outcome {
    code: &'static str,
    transaction: bool,
    backups: usize,
    stamp: u32,
}

fn expected(
    profile: StoreProfile,
    stamp: u32,
    allow: bool,
    variant: Variant,
    old: bool,
) -> Outcome {
    use Variant::*;
    let pending = stamp != 0
        && stamp
            < if old || profile == StoreProfile::TachiFull || variant == Profile {
                39
            } else {
                36
            };
    let error = if stamp > 39 {
        "newer"
    } else if pending && !allow {
        "opt_in"
    } else if (stamp == 39 || (!old && profile == StoreProfile::PortableKernel && stamp >= 36))
        && variant == Sentinel
    {
        "sentinel"
    } else if (stamp == 39 || (!old && profile == StoreProfile::PortableKernel && stamp >= 36))
        && variant == IntegrityRole
    {
        "delivery"
    } else if (stamp == 39 || (!old && profile == StoreProfile::PortableKernel && stamp >= 36))
        && variant == Outbox
    {
        "outbox"
    } else if matches!(variant, Role | Profile | ShapeRole | IntegrityRole) {
        "identity"
    } else if variant == Requirement && profile == StoreProfile::PortableKernel {
        "requirement"
    } else if variant == Outbox {
        "outbox"
    } else if !old && stamp != 0 && profile == StoreProfile::PortableKernel && variant == Shape {
        "shape"
    } else {
        "ok"
    };
    let transaction = error == "ok"
        || error == "shape"
        || (error == "outbox"
            && stamp
                < if old {
                    39
                } else if profile == StoreProfile::PortableKernel {
                    36
                } else {
                    39
                });
    Outcome {
        code: error,
        transaction,
        backups: usize::from(transaction && pending),
        stamp: if error == "ok" && (old || profile == StoreProfile::TachiFull || stamp < 36) {
            39
        } else {
            stamp
        },
    }
}

#[test]
fn outcome_table_pins_pre_b_and_b_errors_phases_and_persistent_effects() {
    for (profile, stamp) in [
        (StoreProfile::PortableKernel, 40),
        (StoreProfile::TachiFull, 38),
        (StoreProfile::PortableKernel, 0),
        (StoreProfile::PortableKernel, 30),
        (StoreProfile::PortableKernel, 36),
        (StoreProfile::PortableKernel, 39),
    ] {
        for allow in [false, true] {
            for &variant in VARIANTS {
                for old in [true, false] {
                    let (_dir, path) = fixture(profile);
                    let conn = Connection::open(&path).unwrap();
                    crate::db::ensure_reserved_reference_write_guard(&conn).unwrap();
                    conn.execute_batch("INSERT INTO memories(id,text,path,source,timestamp,created_at,updated_at,valid_from) VALUES('d7-outcome-row','retained row','/fixture','wiki','2026-10-07T00:00:00Z','2026-10-07T00:00:00Z','2026-10-07T00:00:00Z','2026-10-07T00:00:00Z');").unwrap();
                    if matches!(
                        variant,
                        Variant::Role | Variant::ShapeRole | Variant::IntegrityRole
                    ) {
                        conn.execute_batch("INSERT OR REPLACE INTO hard_state(namespace,key,value_json) VALUES('store_identity','role','{\"value\":7}');").unwrap();
                    }
                    if variant == Variant::Profile {
                        conn.execute_batch("UPDATE hard_state SET value_json='{\"value\":7}' WHERE namespace='store_identity' AND key='profile';").unwrap();
                    }
                    if variant == Variant::Sentinel {
                        conn.execute_batch("DELETE FROM hard_state WHERE namespace='migrations' AND key='v3_handoff_path_standardize';").unwrap();
                    }
                    if matches!(variant, Variant::Shape | Variant::ShapeRole) {
                        conn.execute_batch("DROP INDEX idx_memories_idless_identity_active; CREATE INDEX idx_memories_idless_identity_active ON memories(idless_identity) WHERE idless_identity IS NOT NULL AND archived=0;").unwrap();
                    }
                    if variant == Variant::IntegrityRole {
                        conn.execute_batch("DROP INDEX idx_delivery_events_delivery;")
                            .unwrap();
                    }
                    if variant == Variant::Outbox {
                        conn.execute_batch("DROP INDEX idx_memory_outbox_events_state_created;")
                            .unwrap();
                    }
                    drop(conn);
                    stamp_and_mark(&path, stamp);
                    let conn = Connection::open(&path).unwrap();
                    let schema = query_rows(
                        &conn,
                        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
                    );
                    let before_sentinels = sentinels(&conn);
                    let rows = query_rows(&conn, "SELECT * FROM memories ORDER BY id");
                    let cookie = query_rows(&conn, "PRAGMA schema_version");
                    drop(conn);
                    let mut ctx = context(profile, allow);
                    if variant == Variant::Requirement {
                        ctx.required_profile = StoreProfile::TachiFull.into();
                    }
                    let entered = Rc::new(Cell::new(false));
                    let receipt = Rc::clone(&entered);
                    if !old {
                        test_hooks::arm_window_hook(
                            test_hooks::Window::BeforeSchemaTransaction,
                            move |_| receipt.set(true),
                        );
                    }
                    let open = || match MemoryStore::open_with_context(path.to_str().unwrap(), &ctx)
                    {
                        Ok(store) => {
                            drop(store);
                            "ok"
                        }
                        Err(error) => code(&error),
                    };
                    let result = if old {
                        pre_b_39::with_policy(39, open)
                    } else {
                        open()
                    };
                    let transaction = if old {
                        pre_b_39::transaction_entered()
                    } else {
                        entered.get()
                    };
                    test_hooks::disarm_window_hooks();
                    let conn = Connection::open(&path).unwrap();
                    let actual = Outcome {
                        code: result,
                        transaction,
                        backups: backups(&path).len(),
                        stamp: crate::db::migrations::read_schema_version(&conn).unwrap(),
                    };
                    assert_eq!(
                        actual,
                        expected(profile, stamp, allow, variant, old),
                        "{profile:?}@{stamp} allow={allow} {variant:?} old={old}"
                    );
                    assert_eq!(
                        query_rows(&conn, "SELECT * FROM memories ORDER BY id"),
                        rows,
                        "retained populated rows"
                    );
                    if result != "ok" {
                        assert_eq!(
                            sentinels(&conn),
                            before_sentinels,
                            "refusal cannot change sentinels"
                        );
                        assert_eq!(query_rows(&conn,"SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"),schema,"refusal must roll back schema");
                        assert_eq!(
                            query_rows(&conn, "PRAGMA schema_version"),
                            cookie,
                            "refusal must roll back DDL cookie"
                        );
                    } else if stamp >= 36 && !old && profile == StoreProfile::PortableKernel {
                        assert_eq!(
                            sentinels(&conn),
                            before_sentinels,
                            "band must not enter sentinel runner"
                        );
                    } else if variant == Variant::Sentinel {
                        assert_ne!(
                            sentinels(&conn),
                            before_sentinels,
                            "old/pending successful migration repairs missing v3"
                        );
                        assert_eq!(conn.query_row::<u32,_,_>("SELECT count(*) FROM hard_state WHERE namespace='migrations' AND key='v3_handoff_path_standardize'",[],|r|r.get(0)).unwrap(),1);
                    }
                }
            }
        }
    }
}

#[test]
fn converged_full_reopen_has_identical_raw_schema_and_cookie() {
    let (_dir, path) = fixture(StoreProfile::TachiFull);
    // Converge optional capability once, then sample equally fresh raw handles.
    drop(
        MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::TachiFull, false),
        )
        .unwrap(),
    );
    stamp_and_mark(&path, 39);
    let original_backups = backups(&path);
    let before = Connection::open(&path).unwrap();
    let schema = query_rows(
        &before,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
    );
    let cookie = query_rows(&before, "PRAGMA schema_version");
    drop(before);
    drop(
        MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &DbOpenContext::open_existing_deny(),
        )
        .unwrap(),
    );
    let after = Connection::open(&path).unwrap();
    assert_eq!(
        query_rows(
            &after,
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
        ),
        schema
    );
    assert_eq!(query_rows(&after, "PRAGMA schema_version"), cookie);
    assert_eq!(backups(&path), original_backups);
}
