use super::{Exemption, ExemptionBasis, MachineProof, Site};

/// Keep sorted by path.
pub(super) const EXEMPTIONS: &[Exemption<'static>] = &[
    Exemption {
        path: "crates/memcore/src/db/memory_crud.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "private_authorization_scopes_reset_after_database_errors",
                trigger: "AUTHORIZATION_LEAK",
                ddl: "3fc0f106e730ea0a",
                occurrences: 1,
            },
            Site {
                symbol: "private_authorization_scopes_reset_after_database_errors",
                trigger: "TYPED_SCOPE_ERROR",
                ddl: "798f6231f938e88f",
                occurrences: 1,
            },
            Site {
                symbol: "raw_auxiliary_trigger_cannot_chain_into_typed_search_write",
                trigger: "MALICIOUS_ACCESS_HISTORY_INSERT",
                ddl: "fee8cf3c70f389a2",
                occurrences: 1,
            },
            Site {
                symbol: "raw_connection_cannot_disable_reserved_reference_guards",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "2aec808021c3d3f0",
                occurrences: 1,
            },
            Site {
                symbol: "raw_connection_cannot_disable_reserved_reference_guards",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "47b624650f017587",
                occurrences: 2,
            },
            Site {
                symbol: "raw_connection_cannot_disable_reserved_reference_guards",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 2,
            },
            Site {
                symbol: "raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers",
                trigger: "MIXEDCASEAUXTRIGGER",
                ddl: "1935d8f3df73ed82",
                occurrences: 1,
            },
            Site {
                symbol: "raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers",
                trigger: "MIXEDCASEAUXTRIGGER",
                ddl: "ddbacc979109044d",
                occurrences: 1,
            },
            Site {
                symbol: "raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers",
                trigger: "TEMPAUXTRIGGER",
                ddl: "b3fb7a4857f54094",
                occurrences: 1,
            },
            Site {
                symbol: "raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers",
                trigger: "TEMPAUXTRIGGER",
                ddl: "fa414be6d72ac150",
                occurrences: 1,
            },
            Site {
                symbol: "raw_triggers_cannot_launder_typed_write_authority",
                trigger: "MALICIOUS_MEMORY_UPDATE",
                ddl: "07820a364a9417ef",
                occurrences: 1,
            },
            Site {
                symbol: "raw_triggers_cannot_launder_typed_write_authority",
                trigger: "MALICIOUS_MEMORY_UPDATE",
                ddl: "8507aa5b586fc0b9",
                occurrences: 1,
            },
            Site {
                symbol: "schema_migration_scope_allows_only_canonical_trigger_ddl",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 1,
            },
            Site {
                symbol: "schema_migration_scope_allows_only_canonical_trigger_ddl",
                trigger: "MIGRATION_DDL_BYPASS",
                ddl: "5bef73c316bf372f",
                occurrences: 1,
            },
            Site {
                symbol: "schema_migration_scope_allows_only_canonical_trigger_ddl",
                trigger: "TYPED_DML_DDL_BYPASS",
                ddl: "ecb413ba204a18cd",
                occurrences: 1,
            },
        ],
        reason: "memcore's own authorizer tests, bodies read 2026-07-26. Each \
                 name is either pushed at a store connection and asserted to be \
                 REFUSED (raw_connection_cannot_disable_reserved_reference_guards, \
                 raw_triggers_cannot_launder_typed_write_authority, \
                 raw_auxiliary_trigger_cannot_chain_into_typed_search_write, \
                 schema_migration_scope_allows_only_canonical_trigger_ddl, \
                 private_authorization_scopes_reset_after_database_errors), \
                 planted on a second connection before the authorizer is \
                 installed (raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers), \
                 or the canonical search-generation trigger dropped under an \
                 armed migration token. These tests are the wall's proof, not \
                 users of it.",
    },
    Exemption {
        path: "crates/memcore/src/db/migrations.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Amp implementation lane, affected bodies read 2026-09-20",
        },
        sites: &[
            Site {
                symbol: "stamped_current_v37_missing_append_only_trigger_fails_closed_without_repair",
                trigger: "IDENTITY_VERIFICATION_RECEIPTS_NO_UPDATE",
                ddl: "bf78f824b898c946",
                occurrences: 1,
            },
            Site {
                symbol:
                    "stamped_current_with_missing_search_generation_trigger_is_refused_without_repair",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 1,
            },
            Site {
                symbol: "stamped_v23_with_missing_guards_is_refused_even_with_migration_authority",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "stamped_v23_with_missing_guards_is_refused_even_with_migration_authority",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
            Site {
                symbol: "stamped_v36_db_adds_verified_receipts_without_rewriting_admission_history",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_DELETE",
                ddl: "9205a371ccee164f",
                occurrences: 1,
            },
            Site {
                symbol: "stamped_v36_db_adds_verified_receipts_without_rewriting_admission_history",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_REPLACE",
                ddl: "d2d977d8db3c4414",
                occurrences: 1,
            },
            Site {
                symbol: "stamped_v36_db_adds_verified_receipts_without_rewriting_admission_history",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_UPDATE",
                ddl: "67bc9c164078a9a9",
                occurrences: 1,
            },
            Site {
                symbol: "v22_to_current_installs_reserved_reference_guards_and_scored_count",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "v22_to_current_installs_reserved_reference_guards_and_scored_count",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
            Site {
                symbol: "v23_guard_install_failure_rolls_back_triggers_sentinel_and_stamp",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "v23_guard_install_failure_rolls_back_triggers_sentinel_and_stamp",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
        ],
        reason: "migration tests; the pinned digests are canonical DDL or deliberate \
                 DROP statements used to build or damage a migration fixture. Affected \
                 bodies read 2026-09-20. Enclosing symbols include \
                 stamped_current_v37_missing_append_only_trigger_fails_closed_without_repair, \
                 stamped_v36_db_adds_verified_receipts_without_rewriting_admission_history, \
                 v22_to_current_installs_reserved_reference_guards_and_scored_count, \
                 v23_guard_install_failure_rolls_back_triggers_sentinel_and_stamp, \
                 stamped_v23_with_missing_guards_is_refused_even_with_migration_authority, \
                 stamped_current_with_missing_search_generation_trigger_is_refused_without_repair. \
                 This file names a store doorway and does not arm the migration token, \
                 so nothing here is machine-provable — it is a declaration.",
    },
    Exemption {
        path: "crates/memcore/src/db/migrations/classification_history.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "FROZEN_SEARCH_MAINTENANCE",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "340dc41ab0ac4309",
                occurrences: 1,
            },
            Site {
                symbol: "FROZEN_SEARCH_MAINTENANCE",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "f679f34f65adb809",
                occurrences: 1,
            },
            Site {
                symbol: "FROZEN_SEARCH_MAINTENANCE",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "247319cd75ecf49d",
                occurrences: 1,
            },
            Site {
                symbol: "FROZEN_SEARCH_MAINTENANCE",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "3a8f9bb2587a6ee1",
                occurrences: 1,
            },
            Site {
                symbol: "FROZEN_SEARCH_MAINTENANCE",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "5ffa4851db0ced95",
                occurrences: 1,
            },
            Site {
                symbol: "FROZEN_SEARCH_MAINTENANCE",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "56321119342ca80d",
                occurrences: 1,
            },
            Site {
                symbol: "FROZEN_SEARCH_MAINTENANCE",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "149dc807d99e4b9e",
                occurrences: 1,
            },
            Site {
                symbol: "FROZEN_SEARCH_MAINTENANCE",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "41639d22f6c14602",
                occurrences: 1,
            },
            Site {
                symbol: "FROZEN_SEARCH_MAINTENANCE",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "45058f250231c66a",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_134",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_135",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_136",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "0afb6ce32fb11979",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_137",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "d7422dea485ae00e",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_148",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "41639d22f6c14602",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_149",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "45058f250231c66a",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_150",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "149dc807d99e4b9e",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_151",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "5ffa4851db0ced95",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_152",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "56321119342ca80d",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_153",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "3a8f9bb2587a6ee1",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_154",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "f679f34f65adb809",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_155",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "247319cd75ecf49d",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_156",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "340dc41ab0ac4309",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_184",
                trigger: "IDENTITY_VERIFICATION_RECEIPTS_NO_REPLACE",
                ddl: "812ea55c97bb1ced",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_185",
                trigger: "IDENTITY_VERIFICATION_RECEIPTS_NO_UPDATE",
                ddl: "c065c950eb5f5f32",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_186",
                trigger: "IDENTITY_VERIFICATION_RECEIPTS_NO_DELETE",
                ddl: "b48f870336c476d6",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_187",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_DELETE",
                ddl: "08c29535632d16a0",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_188",
                trigger: "IDENTITY_VERIFICATION_REVOCATIONS_NO_REPLACE",
                ddl: "efca1cdf81a8d334",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_189",
                trigger: "IDENTITY_VERIFICATION_REVOCATIONS_NO_UPDATE",
                ddl: "593490431c8af36e",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_190",
                trigger: "IDENTITY_VERIFICATION_REVOCATIONS_NO_DELETE",
                ddl: "339ef62dcdcff852",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_191",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_REPLACE",
                ddl: "2cf9979cb514295e",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_192",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_UPDATE",
                ddl: "6d1ff7fef1a3e304",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_195",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_REPLACE",
                ddl: "f06d5bf1424e6a1f",
                occurrences: 1,
            },
            Site {
                symbol: "FULL_SQL_196",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_UPDATE",
                ddl: "eec4fdbfa521851b",
                occurrences: 1,
            },
            Site {
                symbol: "P36_GUARDS",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "0afb6ce32fb11979",
                occurrences: 1,
            },
            Site {
                symbol: "P36_GUARDS",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "P36_GUARDS",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
            Site {
                symbol: "P36_GUARDS",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "d7422dea485ae00e",
                occurrences: 1,
            },
            Site {
                symbol: "SQL_47",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "0afb6ce32fb11979",
                occurrences: 1,
            },
            Site {
                symbol: "SQL_47",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "SQL_47",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
            Site {
                symbol: "SQL_47",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "d7422dea485ae00e",
                occurrences: 1,
            },
        ],
        reason: "D7 historical source reconstructions: install/install_full execute fixed \
                 predecessor DDL on the raw connections constructed by classification_tests \
                 and the historical P36 test. The caller paths and enclosing helpers were \
                 read 2026-10-07. This file contains no MemoryStore doorway; that limited \
                 absence is re-derived by the census, not a call-graph proof. Every \
                 historical CREATE/DROP site remains pinned by symbol, digest and count.",
    },
    Exemption {
        path: "crates/memcore/src/db/open.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "FAULT_TRIGGER_PERSISTENT",
                trigger: "WF1443_FAULT_PROBE",
                ddl: "423086a5aa70e980",
                occurrences: 1,
            },
            Site {
                symbol: "FAULT_TRIGGER_TEMP",
                trigger: "WF1443_FAULT_PROBE",
                ddl: "537e5a3b68ddb80c",
                occurrences: 1,
            },
            Site {
                symbol: "an_unguarded_second_connection_can_fail_a_real_store_write",
                trigger: "WF1443_FAIL_STATE_WRITE",
                ddl: "2dbc54ad46e9188b",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_AUTHORITY_HARD_STATE_DELETE_GUARD",
                ddl: "1879e764c59490e1",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_AUTHORITY_HARD_STATE_INSERT_GUARD",
                ddl: "bd0252ac11399838",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_AUTHORITY_HARD_STATE_UPDATE_GUARD",
                ddl: "0341ff8316c0eb7a",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_CANONICAL_SUPERSESSION_EDGE_DELETE_GUARD",
                ddl: "46f26d0fed72be9c",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_CANONICAL_SUPERSESSION_EDGE_INSERT_GUARD",
                ddl: "8c95df2eab0d1c3f",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_CANONICAL_SUPERSESSION_EDGE_UPDATE_GUARD",
                ddl: "7d37af0a8da366de",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_SUPERSESSION_EVENT_DELETE_GUARD",
                ddl: "9808dc1dc3f318ec",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_SUPERSESSION_EVENT_INSERT_GUARD",
                ddl: "0ddbc17b53178011",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_SUPERSESSION_EVENT_UPDATE_GUARD",
                ddl: "100f286dd78f4ed9",
                occurrences: 1,
            },
            Site {
                symbol: "install_ingest_stable_owner_fence",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "85cc694adcebac24",
                occurrences: 1,
            },
            Site {
                symbol: "install_ingest_stable_owner_fence",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "a9f625fe7f6cb0ea",
                occurrences: 1,
            },
            Site {
                symbol: "remove_ingest_stable_owner_fence",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "85cc694adcebac24",
                occurrences: 1,
            },
            Site {
                symbol: "scoped_owner_fence_cleans_and_relocks_after_action_error",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "80cdae411ed81731",
                occurrences: 1,
            },
            Site {
                symbol: "scoped_owner_fence_keeps_temp_ddl_private_and_cleans_after_success",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "76fdc7829814afb4",
                occurrences: 1,
            },
        ],
        reason: "the wall's own module. The nine install_authority_row_guards \
                 sites are the byte-exact TEMP triggers admitted only while \
                 the internal typed-DML or canonical-edge token is armed; bodies \
                 read 2026-08-17. \
                 INGEST_STABLE_OWNER_FENCE is the \
                 byte-exact temp-trigger shape the authorizer admits under the \
                 owner-fence token (install_ingest_stable_owner_fence, \
                 remove_ingest_stable_owner_fence, and the two scoped_owner_fence \
                 tests). WF1443_FAULT_PROBE is asserted DENIED on a store \
                 connection and WF1443_FAIL_STATE_WRITE is installed on an \
                 unguarded second connection and asserted to fail a real store \
                 write — those two tests are tachi#1443's executable pins.",
    },
    Exemption {
        path: "crates/memcore/src/db/schema/ddl.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "RESERVED_REFERENCE_GUARD_SQL",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "0afb6ce32fb11979",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_GUARD_SQL",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_GUARD_SQL",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_GUARD_SQL",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "d7422dea485ae00e",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_INSERT_TRIGGER_SQL",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "0afb81e32fb13d28",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_UPDATE_TRIGGER_SQL",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "d74214ea485ab593",
                occurrences: 1,
            },
        ],
        reason: "the canonical reserved-reference guard triggers as const DDL \
                 text; this file executes nothing. The weaker of the two proofs, \
                 on purpose — it holds only while this file names no store \
                 doorway, and voids itself the day one appears.",
    },
    Exemption {
        path: "crates/memcore/src/db/schema/migration_backup_tests.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body NOT read 2026-07-26",
        },
        sites: &[
            Site {
                symbol: "seed_v22_fixture",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "90d32439353dd888",
                occurrences: 1,
            },
            Site {
                symbol: "seed_v22_fixture",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
        ],
        reason: "seed_v22_fixture drops the canonical guards on a fixture \
                 database to manufacture a v22 shape. Enclosing symbol \
                 extracted mechanically; body NOT read. The file names a store \
                 doorway, so the no-doorway proof does not hold and this is a \
                 declaration.",
    },
    Exemption {
        path: "crates/memcore/src/db/schema/open_race_tests.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[Site {
            symbol: "TRIGGER",
            trigger: "",
            ddl: "6f0bc4e1dfbe5fa6",
            occurrences: 4,
        }],
        reason: "trigger_dropped_in_window_is_refused_like_a_sequential_open and \
                 trigger_dropped_after_commit_is_refused_before_a_handle_is_returned \
                 each drop the canonical memory_edge_search_generation_after_update \
                 trigger (named by a local const TRIGGER, hence runtime-assembled) \
                 to damage a store's trigger inventory: once on a second store \
                 before a sequential open, once inside a cfg(test) window hook \
                 (before BEGIN IMMEDIATE, and after the schema COMMIT). All four \
                 DROPs run on a fresh rusqlite::Connection::open(path), never on a \
                 MemoryStore doorway; each test asserts the open REFUSES the \
                 damaged inventory. Body read 2026-09-26. The proof remains valid \
                 only while this file names no MemoryStore doorway.",
    },
    Exemption {
        path: "crates/memcore/src/db/schema/portable_version_tests.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex coordinator, enclosing body read 2026-10-07",
        },
        sites: &[
            Site {
                symbol: "band_missing_objects_refuse_preflight_but_present_bad_definitions_refuse_in_transaction",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "f0bd5d1e4be39287",
                occurrences: 1,
            },
        ],
        reason: "band_missing_objects_refuse_preflight_but_present_bad_definitions_refuse_in_transaction \
                 drops the canonical trigger on a second raw Connection::open(&path), \
                 then opens the damaged fixture through MemoryStore and asserts the \
                 preflight refusal, unchanged schema/sentinels/stamp and zero backups. \
                 Body read 2026-10-07. Other tests name MemoryStore doorways, so the \
                 file-wide absence proof does not hold; this is an explicit declaration.",
    },
    Exemption {
        path: "crates/memcore/src/db/search_generation.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "340dc41ab0ac4309",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "f679f34f65adb809",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "247319cd75ecf49d",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "3a8f9bb2587a6ee1",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "5ffa4851db0ced95",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "56321119342ca80d",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "149dc807d99e4b9e",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "41639d22f6c14602",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "45058f250231c66a",
                occurrences: 1,
            },
            Site {
                symbol: "migrate_previous_memory_update_trigger",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 1,
            },
            Site {
                symbol: "missing_or_drifted_trigger_refuses_generation_read",
                trigger: "",
                ddl: "c5435d9d26bb4068",
                occurrences: 1,
            },
            Site {
                symbol: "missing_or_drifted_trigger_refuses_generation_read",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 1,
            },
            Site {
                symbol: "trigger_sql",
                trigger: "",
                ddl: "95545aefb7bebb4e",
                occurrences: 1,
            },
        ],
        reason: "the canonical search-generation triggers themselves, and the \
                 migration that replaces a drifted one, all under an armed \
                 migration token. The two name-less digests are trigger_sql's \
                 dynamically named trigger builder and the literal \
                 drop-operation expect message in \
                 missing_or_drifted_trigger_refuses_generation_read, which \
                 uppercases into a keyword with no name after it. Both read \
                 2026-07-26.",
    },
    Exemption {
        path: "crates/memcore/src/db/tests/model_catalog_ops.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[Site {
            symbol: "break_the_event_append",
            trigger: "REFUSE_EVENT_APPEND",
            ddl: "0aba31b541e6e02d",
            occurrences: 1,
        }],
        reason: "break_the_event_append installs its RAISE(ABORT) trigger on the \
                 bare in-memory rusqlite Connection returned by catalog_conn and \
                 the append-failure rollback and connection-state tests execute \
                 the store door on that same unguarded connection — #1443's \
                 sanctioned memcore pattern. Body read 2026-08-13. The proof \
                 remains valid only while this file names no MemoryStore doorway.",
    },
    Exemption {
        path: "crates/memcore/src/db/tests/search_generation.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "known_previous_memory_update_trigger_migrates_to_all_column_coverage",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "dcadfb203310d3df",
                occurrences: 1,
            },
            Site {
                symbol: "known_previous_memory_update_trigger_migrates_to_all_column_coverage",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "fd31dd35ddd478a9",
                occurrences: 1,
            },
            Site {
                symbol: "missing_trigger_refuses_search_generation",
                trigger: "",
                ddl: "c5435d9d26bb4068",
                occurrences: 1,
            },
            Site {
                symbol: "missing_trigger_refuses_search_generation",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "836f7494890a3bf8",
                occurrences: 1,
            },
        ],
        reason: "drops and recreates the canonical generation triggers under an \
                 armed migration token to exercise the migration path \
                 (missing_trigger_refuses_search_generation, \
                 known_previous_memory_update_trigger_migrates_to_all_column_coverage). \
                 The name-less digest is the same drop-operation expect \
                 message shape; read 2026-07-26.",
    },
    Exemption {
        path: "crates/memcore/src/db/tests/write_ops.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex 5.5 local agent, body read 2026-07-30",
        },
        sites: &[
            Site {
                symbol: "enrichment_sql_failure_rolls_back_field_and_receipt_together",
                trigger: "FAIL_ENRICHMENT_FIELD_RECEIPT_CAS",
                ddl: "b134e245f67c7163",
                occurrences: 1,
            },
            Site {
                symbol: "typed_noncanonical_transition_archives_without_hard_delete",
                trigger: "NO_HARD_DELETE",
                ddl: "d4b5b7c7036c68da",
                occurrences: 1,
            },
        ],
        reason: "enrichment_sql_failure_rolls_back_field_and_receipt_together \
                 installs its trigger on the direct rusqlite Connection \
                 returned by make_conn, then drives update_enrichment_fields \
                 on that same unguarded connection. Body read 2026-07-30. \
                 typed_noncanonical_transition_archives_without_hard_delete \
                 installs its trigger on the direct rusqlite Connection \
                 returned by make_conn. Body read 2026-08-01. \
                 This file also contains unrelated MemoryStore doorway tests, \
                 so NoStoreDoorwayInFile cannot prove the exemption.",
    },
    Exemption {
        path: "crates/memcore/src/db/typed_records.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "KIND_DELETE",
                trigger: "TYPED_RECORD_KINDS_NO_DELETE",
                ddl: "4dc187473bc3f9b9",
                occurrences: 1,
            },
            Site {
                symbol: "KIND_INSERT",
                trigger: "TYPED_RECORD_KINDS_INSERT_GUARD",
                ddl: "cb59c55c28637af0",
                occurrences: 1,
            },
            Site {
                symbol: "KIND_UPDATE",
                trigger: "TYPED_RECORD_KINDS_NO_UPDATE",
                ddl: "dec671564ad712ed",
                occurrences: 1,
            },
            Site {
                symbol: "LINEAGE_DELETE",
                trigger: "TYPED_RECORD_SUPERSESSIONS_NO_DELETE",
                ddl: "727b135d09e2ffec",
                occurrences: 1,
            },
            Site {
                symbol: "LINEAGE_INSERT",
                trigger: "TYPED_RECORD_SUPERSESSIONS_INSERT_GUARD",
                ddl: "abe561cc625fbbdb",
                occurrences: 1,
            },
            Site {
                symbol: "LINEAGE_UPDATE",
                trigger: "TYPED_RECORD_SUPERSESSIONS_NO_UPDATE",
                ddl: "62b5e70687389c80",
                occurrences: 1,
            },
            Site {
                symbol: "RECORD_DELETE",
                trigger: "TYPED_RECORDS_NO_DELETE",
                ddl: "645f3bedd51ecbdd",
                occurrences: 1,
            },
            Site {
                symbol: "RECORD_INSERT",
                trigger: "TYPED_RECORDS_INSERT_GUARD",
                ddl: "1a5f9a103ddda6fb",
                occurrences: 1,
            },
            Site {
                symbol: "RECORD_UPDATE",
                trigger: "TYPED_RECORDS_NO_UPDATE",
                ddl: "1c3bd67b31b31941",
                occurrences: 1,
            },
        ],
        reason: "the nine constants are canonical v41 typed-record, lineage and \
                 kind-registry guards. install executes them on the migration \
                 connection supplied by the catalogue; this file names no \
                 MemoryStore connection doorway. Constants, installer and v41 \
                 catalogue call read by the Codex census audit 2026-11-03. The \
                 machine basis establishes only the documented file-local absence.",
    },
    Exemption {
        path: "crates/memcore/src/db/verified_admissions.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Amp implementation lane, bodies read 2026-09-20",
        },
        sites: &[
            Site {
                symbol: "ADMISSION_NO_DELETE_SQL",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_DELETE",
                ddl: "08c2b035632d4481",
                occurrences: 1,
            },
            Site {
                symbol: "ADMISSION_NO_REPLACE_SQL",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_REPLACE",
                ddl: "f06d74f1424e949a",
                occurrences: 1,
            },
            Site {
                symbol: "ADMISSION_NO_UPDATE_SQL",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_UPDATE",
                ddl: "eec4e6bfa5215e06",
                occurrences: 1,
            },
            Site {
                symbol: "RECEIPT_NO_DELETE_SQL",
                trigger: "IDENTITY_VERIFICATION_RECEIPTS_NO_DELETE",
                ddl: "b48f7e0336c4678b",
                occurrences: 1,
            },
            Site {
                symbol: "RECEIPT_NO_REPLACE_SQL",
                trigger: "IDENTITY_VERIFICATION_RECEIPTS_NO_REPLACE",
                ddl: "812e8a5c97baef0c",
                occurrences: 1,
            },
            Site {
                symbol: "RECEIPT_NO_UPDATE_SQL",
                trigger: "IDENTITY_VERIFICATION_RECEIPTS_NO_UPDATE",
                ddl: "c065e050eb5f8647",
                occurrences: 1,
            },
            Site {
                symbol: "REVOCATION_NO_DELETE_SQL",
                trigger: "IDENTITY_VERIFICATION_REVOCATIONS_NO_DELETE",
                ddl: "339f0d2dcdd01f67",
                occurrences: 1,
            },
            Site {
                symbol: "REVOCATION_NO_REPLACE_SQL",
                trigger: "IDENTITY_VERIFICATION_REVOCATIONS_NO_REPLACE",
                ddl: "efca07df81a8af85",
                occurrences: 1,
            },
            Site {
                symbol: "REVOCATION_NO_UPDATE_SQL",
                trigger: "IDENTITY_VERIFICATION_REVOCATIONS_NO_UPDATE",
                ddl: "593477431c8ac8f3",
                occurrences: 1,
            },
            Site {
                symbol: "schema_validation_rejects_same_name_trigger_drift",
                trigger: "IDENTITY_VERIFICATION_RECEIPTS_NO_UPDATE",
                ddl: "023a0b505415c3a9",
                occurrences: 1,
            },
            Site {
                symbol: "schema_validation_rejects_same_name_trigger_drift",
                trigger: "IDENTITY_VERIFICATION_RECEIPTS_NO_UPDATE",
                ddl: "f3572365ab9e5f67",
                occurrences: 1,
            },
        ],
        reason: "the nine module-scope constants are v37's canonical production \
                 append-only trigger definitions. \
                 schema_validation_rejects_same_name_trigger_drift deliberately drops \
                 and replaces one trigger on an unguarded in-memory connection to prove \
                 byte-canonical validation fails closed. Bodies read 2026-09-20. The \
                 test module also names raw connection doorways, so this is declared \
                 rather than assigned an inapplicable machine proof.",
    },
    Exemption {
        path: "crates/memcore/src/private_partition/tests.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex downstream census audit (agent), affected body read 2026-11-03",
        },
        sites: &[Site {
            symbol: "sealed_current_image_presence_and_previous_trigger_controls",
            trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
            ddl: "d232a9fb5db0971e",
            occurrences: 1,
        }],
        reason: "sealed_current_image_presence_and_previous_trigger_controls \
                 deserializes an owned snapshot into a separate raw in-memory \
                 connection, drops the trigger there, and encrypts the fixture \
                 before the real private open. Missing triggers refuse; the \
                 recognized previous definition normalizes. The moved body and \
                 connection source were read 2026-11-03. Other bodies name store \
                 doorways, so this is a declaration rather than a machine proof.",
    },
    Exemption {
        path: "crates/memcore/src/store/current_store_admission_tests/runtime.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex Sol, bodies read 2026-10-05",
        },
        sites: &[Site {
            symbol: "read_only_maintenance_and_public_initializer_refuse_before_returning_a_handle",
            trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
            ddl: "1f453a249e6e3163",
            occurrences: 1,
        }],
        reason: "the entry-point test drops a persistent trigger through a second raw Connection::open on an owned fixture before read-only and maintenance opens, then asserts the original trigger refusal and unchanged logical state/artifacts. Other tests use store doorways only to read outcomes; body read 2026-10-05. No MachineProof applies to this file, so this is a declaration.",
    },
    Exemption {
        path: "crates/memcore/src/store/linking.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Kernel cleanup implementation lane, affected rollback body read 2026-11-08",
        },
        sites: &[
            Site {
                symbol: "confirmed_contradiction_transaction_rolls_back_at_every_side_effect_boundary",
                trigger: "FAIL_CONFIRMED_FIRST",
                ddl: "a9eb74177952b834",
                occurrences: 1,
            },
            Site {
                symbol: "confirmed_contradiction_transaction_rolls_back_at_every_side_effect_boundary",
                trigger: "FAIL_CONFIRMED_LIFECYCLE",
                ddl: "70c23cd1ba89425e",
                occurrences: 1,
            },
            Site {
                symbol: "confirmed_contradiction_transaction_rolls_back_at_every_side_effect_boundary",
                trigger: "FAIL_CONFIRMED_SECOND",
                ddl: "264d76731ecfe769",
                occurrences: 1,
            },
        ],
        reason: "The rollback fixture opens a file-backed MemoryStore, then installs \
                 each persistent failure trigger through a separate rusqlite Connection. \
                 The live commit_confirmed_contradiction doorway retains its authorizer \
                 and owns the transaction; graph rows, lifecycle state, observations and \
                 canonical receipts are checked after injected failures.",
    },
    Exemption {
        path: "crates/memcore/src/store/memory_lifecycle.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body NOT read 2026-07-26",
        },
        sites: &[Site {
            symbol: "apply_rolls_back_memory_when_proposal_stamp_aborts",
            trigger: "ABORT_LIFECYCLE_PROPOSAL_STAMP",
            ddl: "38fc27464aa57215",
            occurrences: 1,
        }],
        reason: "apply_rolls_back_memory_when_proposal_stamp_aborts injects a \
                 rollback fault on memcore's raw second connection. Enclosing \
                 symbol extracted mechanically; body NOT read. The file also \
                 names a store doorway, so no proof in the enum holds and this \
                 is a declaration.",
    },
    Exemption {
        path: "crates/memcore/src/store/open/exact_dedupe_open_tests.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "authorized_v22_migration_installs_missing_reference_guards",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "90d32439353dd888",
                occurrences: 1,
            },
            Site {
                symbol: "authorized_v22_migration_installs_missing_reference_guards",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
            Site {
                symbol: "existing_opens_refuse_missing_reserved_reference_guards_before_repair",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "90d32439353dd888",
                occurrences: 1,
            },
            Site {
                symbol: "existing_opens_refuse_missing_reserved_reference_guards_before_repair",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
            Site {
                symbol: "open_refuses_unknown_or_spoofed_persistent_triggers",
                trigger: "MALICIOUS_ACCESS_CHAIN",
                ddl: "43ad5ed527d39e80",
                occurrences: 1,
            },
            Site {
                symbol: "open_refuses_unknown_or_spoofed_persistent_triggers",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "36f015c4c2878ed5",
                occurrences: 1,
            },
            Site {
                symbol: "open_refuses_unknown_or_spoofed_persistent_triggers",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
            Site {
                symbol:
                    "read_only_existing_schema_compat_opens_stamped_older_without_write_authority",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "90d32439353dd888",
                occurrences: 1,
            },
            Site {
                symbol:
                    "read_only_existing_schema_compat_opens_stamped_older_without_write_authority",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
            Site {
                symbol: "stamp_v22_without_v23_guards",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "stamp_v22_without_v23_guards",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
        ],
        reason: "the listed open-path fixtures execute their trigger DDL on \
                 second raw Connection::open handles; every caller of \
                 stamp_v22_without_v23_guards passes such an offline handle. \
                 The fixture bodies and helper callers were read by the Codex \
                 census audit 2026-11-03. The moved file itself still arms the \
                 migration token in existing_read_write_does_not_recreate_missing_memories_vec, \
                 so the file-level machine basis remains valid; it does not \
                 claim that the offline trigger fixtures execute under that token.",
    },
    Exemption {
        path: "crates/memcore/src/store/profile_identity_tests.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Amp implementation lane, body read 2026-09-20",
        },
        sites: &[
            Site {
                symbol: "production_open_authorizer_migrates_v36_verified_admission_inventory",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_DELETE",
                ddl: "9205a371ccee164f",
                occurrences: 1,
            },
            Site {
                symbol: "production_open_authorizer_migrates_v36_verified_admission_inventory",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_REPLACE",
                ddl: "d2d977d8db3c4414",
                occurrences: 1,
            },
            Site {
                symbol: "production_open_authorizer_migrates_v36_verified_admission_inventory",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_UPDATE",
                ddl: "67bc9c164078a9a9",
                occurrences: 1,
            },
            Site {
                symbol: "production_reopen_rejects_altered_identity_admission_conflict_target",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_DELETE",
                ddl: "9205a371ccee164f",
                occurrences: 1,
            },
            Site {
                symbol: "production_reopen_rejects_altered_identity_admission_conflict_target",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_REPLACE",
                ddl: "d2d977d8db3c4414",
                occurrences: 1,
            },
            Site {
                symbol: "production_reopen_rejects_altered_identity_admission_conflict_target",
                trigger: "IDENTITY_VERIFIED_ADMISSIONS_NO_UPDATE",
                ddl: "67bc9c164078a9a9",
                occurrences: 1,
            },
        ],
        reason: "production_open_authorizer_migrates_v36_verified_admission_inventory \
                 drops three v37 triggers on an unguarded fixture connection to model a \
                 v36 database, then exercises the production MemoryStore open/migrate path \
                 and canonical inventory. \
                 production_reopen_rejects_altered_identity_admission_conflict_target \
                 removes the same triggers while rebuilding an offline fixture, restores \
                 canonical v37 objects, and proves production reopen rejects the altered \
                 historical conflict target. Bodies read 2026-09-20. The file names store \
                 doorways, so these fixtures are declared rather than machine-proven.",
    },
    Exemption {
        path: "crates/memcore/src/store/snapshot_import.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex coordinator, fixture bodies read 2026-11-02",
        },
        sites: &[Site {
            symbol: "an_fts_trigger_installed_after_open_keeps_synchronization",
            trigger: "SNAPSHOT_FIXTURE_FTS",
            ddl: "b435635c353f6b7e",
            occurrences: 1,
        }],
        reason: "an_fts_trigger_installed_after_open_keeps_synchronization installs \
                 the fixture on an actual second rusqlite::Connection::open to the \
                 owned file after MemoryStore admission, then exercises the guarded \
                 public importer. Only the symbolic trigram projection is injected, \
                 so this pre-lock fixture never invokes the simple tokenizer. \
                 Fixture and invocation bodies read; this is a \
                 declaration, not a machine proof. The file is discarded after the test.",
    },
    Exemption {
        path: "crates/memcore/src/store/vault/tests/vault_failure_injection.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[Site {
            symbol: "vault_replace_api_key_pool_rolls_back_when_rotation_write_fails",
            trigger: "FAIL_POOL_ROTATION",
            ddl: "9ec39925504e73bc",
            occurrences: 1,
        }],
        reason: "vault_replace_api_key_pool_rolls_back_when_rotation_write_fails \
                 — isolated raw second-connection failure fixture. Re-derived: \
                 this dedicated file names no store doorway, so the trigger \
                 cannot reach a guarded MemoryStore connection from here.",
    },
    Exemption {
        path: "crates/memcore/src/typed_records/tests.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex downstream census audit (agent), affected body read 2026-11-03",
        },
        sites: &[Site {
            symbol: "kind_registry_rejects_shadowed_rowid_and_missing_guards_on_reopen",
            trigger: "TYPED_RECORD_KINDS_NO_UPDATE",
            ddl: "385f00fd046115b6",
            occurrences: 1,
        }],
        reason: "kind_registry_rejects_shadowed_rowid_and_missing_guards_on_reopen \
                 closes its owned MemoryStore, opens a separate raw Connection \
                 on that fixture file, drops the guard, closes the raw handle, \
                 and asserts the real guarded reopen refuses. The complete body \
                 and open helper were read 2026-11-03. Other tests name store \
                 doorways, and this file does not arm a schema-migration token; \
                 this is a declaration, not a machine proof.",
    },
    Exemption {
        path: "crates/memcore/tests/legacy_row_probe.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "fixture",
                trigger: "CREATED_OBSERVER",
                ddl: "aa13b594a1139d02",
                occurrences: 1,
            },
            Site {
                symbol: "fixture",
                trigger: "REVISION_OBSERVER",
                ddl: "6a035801cb360890",
                occurrences: 1,
            },
            Site {
                symbol: "fixture",
                trigger: "UPDATED_OBSERVER",
                ddl: "f3b53595318302f8",
                occurrences: 1,
            },
        ],
        reason: "the legacy-row parity fixture (#2143) opens an isolated raw \
                 in-memory rusqlite::Connection and installs three observer \
                 triggers recording field-update order, comparing the guarded \
                 probe against the original statement prefix. Re-derived: this \
                 dedicated file names no store doorway, so the triggers cannot \
                 reach a guarded MemoryStore connection from here.",
    },
    Exemption {
        path: "crates/tachi-params/src/current_truth/tests/projection_compat.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[Site {
            symbol: "mixed_case_projection_insert_failure_rolls_back_entire_rewrite",
            trigger: "FAIL_PROJECTION_INSERT",
            ddl: "88c1c19f866d3357",
            occurrences: 1,
        }],
        reason: "the projection fixture opens a separate direct SQLite connection on its private database, installs an insertion abort, then calls the real CurrentTruth write API and verifies both legacy projections and all immutable assertion columns survived. No guarded store doorway is present in this file.",
    },
    Exemption {
        path: "crates/tachi-server/src/bootstrap/wiki_corpus/apply.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex Luna, bodies read 2026-08-01",
        },
        sites: &[
            Site {
                symbol: "maybe_inject_copy_after_receipt_prepared",
                trigger: "WIKI_CORPUS_NO_HARD_DELETE",
                ddl: "ffd56ed2b7788f70",
                occurrences: 1,
            },
        ],
        reason: "maybe_inject_copy_after_receipt_prepared installs the hard-delete \
                 sentinel through a second direct rusqlite::Connection::open(target_path). \
                 These test-fixture sites use an unrestricted/file connection to install \
                 or remove the sentinel; they are not #1443 false-failure injection \
                 through guarded MemoryStore::connection. Bodies read 2026-08-01.",
    },
    Exemption {
        path: "crates/tachi-server/src/bootstrap/wiki_corpus/tests.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex Luna, bodies read 2026-08-01",
        },
        sites: &[
            Site {
                symbol: "remove_memory_hard_delete_guard",
                trigger: "WIKI_CORPUS_NO_HARD_DELETE",
                ddl: "b59c6c444efa524c",
                occurrences: 1,
            },
        ],
        reason: "remove_memory_hard_delete_guard removes the hard-delete sentinel \
                 through a second direct rusqlite::Connection::open(path). These \
                 test-fixture sites use an unrestricted/file connection to install \
                 or remove the sentinel; they are not #1443 false-failure injection \
                 through guarded MemoryStore::connection. Bodies read 2026-08-01.",
    },
    Exemption {
        path: "crates/tachi-server/src/exec_env_ops.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex Sol, body read 2026-08-27",
        },
        sites: &[
            Site {
                symbol: "abandoned_dispatch_with_a_failed_fence_stays_exclusively_fail_closed",
                trigger: "FAIL_ABANDONED_DISPATCH_FENCE",
                ddl: "efee4311eb6310e3",
                occurrences: 1,
            },
            Site {
                symbol: "contains_worktree_marker",
                trigger: "FAIL_BUILD_PRIVATE_PUBLICATION",
                ddl: "47c83951777b49af",
                occurrences: 1,
            },
            Site {
                symbol: "contains_worktree_marker",
                trigger: "FAIL_BUILD_PRIVATE_PUBLICATION",
                ddl: "73c18ff187218f56",
                occurrences: 1,
            },
            Site {
                symbol: "late_publication_failure_rolls_back_every_intermediate_row",
                trigger: "FAIL_PRIVATE_RESERVATION",
                ddl: "701012d43ab1f566",
                occurrences: 1,
            },
        ],
        reason: "the tests open a second direct rusqlite::Connection on a file-backed DB and install failure triggers there, so the real quarantine or atomic publication transaction fails. The BuildPrivate test drops its persistent trigger through the post-failure hook, rolls back only the exact Tachi-generated Cargo config, then exercises certified worktree cleanup; bodies read 2026-08-27. The file also contains MemoryStore doorways, so these sites are declared rather than assigned an inapplicable machine proof.",
    },
    Exemption {
        path: "crates/tachi-server/src/exec_env_postflight/tests.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex Sol, body read 2026-08-27",
        },
        sites: &[Site {
            symbol: "daemon_quarantine_sink_propagates_resource_failure_before_receipt",
            trigger: "FAIL_POSTFLIGHT_RESOURCE_QUARANTINE",
            ddl: "6f65485c4aa9fc08",
            occurrences: 1,
        }],
        reason: "the test opens a second direct rusqlite::Connection on the file-backed server DB and installs the trigger before exercising DaemonQuarantineSink; body read 2026-08-27. The file also uses guarded store connections for setup/assertions, so no machine proof applies.",
    },
    Exemption {
        path: "crates/tachi-server/src/mcp_pool/proxy.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), bodies read 2026-07-26",
        },
        sites: &[
            Site {
                symbol: "failed_auto_ingest_does_not_replace_successful_mcp_result",
                trigger: "FAIL_PROXY_AUTO_INGEST_ROW",
                ddl: "943337696f6b7b43",
                occurrences: 1,
            },
            Site {
                symbol: "failed_auto_ingest_staging_is_visible_in_pipeline_status",
                trigger: "FAIL_PROXY_AUTO_INGEST_STAGE",
                ddl: "6c121fc727d3c6ed",
                occurrences: 1,
            },
        ],
        reason: "both installed inside with_unrestricted_fixture_connection on \
                 the server's global DB path; bodies read 2026-07-26 \
                 (failed_auto_ingest_does_not_replace_successful_mcp_result, \
                 failed_auto_ingest_staging_is_visible_in_pipeline_status). This \
                 is the sanctioned shape — but the file also names a store \
                 doorway, so nothing machine-checkable establishes it and this \
                 is a declaration.",
    },
    Exemption {
        path: "crates/tachi-server/src/pipeline_ops/audit.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body read 2026-07-26",
        },
        sites: &[
            Site {
                symbol: "lost_heartbeat_fences_stale_graph_observation_after_takeover",
                trigger: "FAIL_INGEST_HEARTBEAT",
                ddl: "087fb80573a0e8da",
                occurrences: 1,
            },
            Site {
                symbol: "lost_heartbeat_fences_stale_graph_observation_after_takeover",
                trigger: "FAIL_INGEST_HEARTBEAT",
                ddl: "5567a2a03d22ca97",
                occurrences: 1,
            },
            Site {
                symbol: "lost_heartbeat_fences_stale_graph_observation_after_takeover",
                trigger: "MARK_HEARTBEAT_AFTER_JOIN",
                ddl: "8c1bd387cd98c4f6",
                occurrences: 1,
            },
        ],
        reason: "both installed through with_offline_global_fixture_connection \
                 (a local wrapper over the unguarded route) inside \
                 lost_heartbeat_fences_stale_graph_observation_after_takeover, \
                 and dropped again in the same test; body read 2026-07-26. The \
                 file also names a store doorway, so this is a declaration.",
    },
    Exemption {
        path: "crates/tachi-server/src/repair/tests/plan_c_restore.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "arm_memories_fts_delete_failure",
                trigger: "MEMORIES_FTS_FAIL_DELETE",
                ddl: "99f4dc6641aad4d7",
                occurrences: 1,
            },
            Site {
                symbol: "arm_memories_fts_update_failure",
                trigger: "MEMORIES_FTS_FAIL_UPDATE",
                ddl: "e91f547436ca6aea",
                occurrences: 1,
            },
            Site {
                symbol: "arm_symbolic_fts_delete_failure",
                trigger: "MEMORIES_SYMBOLIC_FTS_FAIL_DELETE",
                ddl: "93b4b5bf33a9b407",
                occurrences: 1,
            },
        ],
        reason: "FTS restore fault injection on an unguarded fixture connection. \
                 Re-derived: this file names no store doorway, so no site in it \
                 can reach a guarded connection from here. Enclosing symbols \
                 extracted mechanically; bodies NOT read.",
    },
    Exemption {
        path: "crates/tachi-server/src/repair/tests/quarantine_jobs_integrity.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[Site {
            symbol: "quarantine_purge_uses_default_deny_v23_connection",
            trigger: "QUARANTINE_PURGE_REQUIRES_DEFAULT_DENY_GUARD",
            ddl: "9ecc2dee2bc20b47",
            occurrences: 1,
        }],
        reason: "installs the quarantine default-deny guard on an unguarded \
                 fixture connection. Re-derived: this file names no store \
                 doorway. Body NOT read.",
    },
    Exemption {
        path: "crates/tachi-server/src/staffing_ops/mod.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex Sol, body read 2026-08-27",
        },
        sites: &[
            Site {
                symbol: "find_worktree",
                trigger: "FAIL_STAFF_EXEC_ENV_PUBLICATION",
                ddl: "08b602143fb001c8",
                occurrences: 1,
            },
            Site {
                symbol: "find_worktree",
                trigger: "FAIL_STAFF_EXEC_ENV_PUBLICATION",
                ddl: "7e17e51fe7d94067",
                occurrences: 1,
            },
        ],
        reason: "staff_publication_failure_retains_worktree_for_certified_cleanup opens an unrestricted second connection to inject a real atomic publication failure, then a test-only post-failure hook drops that persistent trigger before the production certified worktree cleanup reopens the DB. Both CREATE and DROP sites are pinned; body read 2026-08-28. The file also contains MemoryStore doorways, so this is declared rather than assigned an inapplicable machine proof.",
    },
    Exemption {
        path: "crates/tachi-server/src/staffing_ops/tests/native_lifecycle.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "OpenAI Codex, body read 2026-11-03",
        },
        sites: &[
            Site { symbol: "assert_staff_lifecycle", trigger: "FAIL_NATIVE_FENCE", ddl: "362446e77a4b3204", occurrences: 1 },
            Site { symbol: "assert_staff_lifecycle", trigger: "FAIL_NATIVE_FENCE", ddl: "60b430cea7d660f6", occurrences: 1 },
        ],
        reason: "assert_staff_lifecycle opens a separate rusqlite connection to the fixture global DB, installs a resource-fence failure trigger, verifies canonical failure outcomes, then drops the trigger on that same connection before teardown. This file also reads outcomes through MemoryStore, so no machine proof applies; both CREATE and DROP sites are declared explicitly.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/memory_tests/save_policy/recall_cache_invalidation.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body read 2026-07-26",
        },
        sites: &[Site {
            symbol:
                "missing_generation_trigger_bypasses_a_warm_cache_instead_of_serving_stale_rows",
            trigger: "MEMORY_SEARCH_GENERATION_AFTER_INSERT",
            ddl: "7866d9740159f9e6",
            occurrences: 1,
        }],
        reason: "missing_generation_trigger_bypasses_a_warm_cache_instead_of_serving_stale_rows \
                 drops the canonical insert trigger through \
                 with_unrestricted_fixture_connection to simulate drift; body \
                 read 2026-07-26. The file also names a store doorway, so this \
                 is a declaration.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/mod.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body NOT read 2026-07-26",
        },
        sites: &[
            Site {
                symbol: "seed_pre_v23_wiki_reference_metadata",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "seed_pre_v23_wiki_reference_metadata",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
        ],
        reason: "seed_pre_v23_wiki_reference_metadata drops the canonical guards \
                 on a fixture database to manufacture a pre-v23 shape. Enclosing \
                 symbol extracted mechanically; body NOT read. This is the \
                 fixture hub — it names a store doorway many times over, so it \
                 gets no proof and every site in it is a declaration.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/skill_tests/builtin_ingest/auto_ingest.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1756 contract repair (Codex), bodies read 2026-08-14",
        },
        sites: &[
            Site {
                symbol: "admitted_auto_ingest_chunk_write_failure_is_partial_and_replayable",
                trigger: "FAIL_ADMITTED_CHUNK_WRITE",
                ddl: "83faca4f9eb4b9f1",
                occurrences: 1,
            },
            Site {
                symbol: "admitted_auto_ingest_chunk_write_failure_is_partial_and_replayable",
                trigger: "FAIL_ADMITTED_CHUNK_WRITE",
                ddl: "fdb0ef03dadcb089",
                occurrences: 1,
            },
            Site {
                symbol:
                    "admitted_auto_ingest_completion_receipt_failure_is_partial_and_replayable",
                trigger: "FAIL_ADMITTED_AUTO_INGEST_COMPLETION",
                ddl: "b92e6ecff09fdedc",
                occurrences: 1,
            },
            Site {
                symbol:
                    "admitted_auto_ingest_completion_receipt_failure_is_partial_and_replayable",
                trigger: "FAIL_ADMITTED_AUTO_INGEST_COMPLETION",
                ddl: "e8d2cf7827ea37f7",
                occurrences: 1,
            },
            Site {
                symbol: "admitted_auto_ingest_edge_failure_is_partial_without_duplicate_chunks",
                trigger: "FAIL_ADMITTED_EDGE_WRITE",
                ddl: "18e035a5d422f961",
                occurrences: 1,
            },
            Site {
                symbol: "admitted_auto_ingest_edge_failure_is_partial_without_duplicate_chunks",
                trigger: "FAIL_ADMITTED_EDGE_WRITE",
                ddl: "73a542736e95f78b",
                occurrences: 1,
            },
        ],
        reason: "the chunk-write, completion-receipt, and edge-write recovery tests each \
                 install and then drop their named fault trigger through \
                 with_unrestricted_fixture_connection, assert the typed partial stage, \
                 and prove replay completes without duplicate chunks. Bodies read \
                 2026-08-14. The file also names store doorways, so this sanctioned \
                 fault-injection shape is declared rather than machine-proven.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/wiki_tests/write/facade_routing/guide_metadata.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "v22_named_project_write_migrates_before_guide_pattern_and_reference_reads",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "v22_named_project_write_migrates_before_guide_pattern_and_reference_reads",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
        ],
        reason: "pre-v23 wiki fixture seeding on an unguarded connection. \
                 Re-derived: this file names no store doorway. Enclosing symbol \
                 extracted mechanically; body NOT read.",
    },
    Exemption {
        path: "crates/tachi-vault-tests/tests/vault_tests/env_injection/lane_config_overlay.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex Sol, bodies read 2026-08-30",
        },
        sites: &[
            Site {
                symbol: "drop_catalog_failure_trigger",
                trigger: "PR1862_FAIL_CATALOG_REFRESH",
                ddl: "0241301fe84ca149",
                occurrences: 1,
            },
            Site {
                symbol: "install_catalog_failure_trigger",
                trigger: "PR1862_FAIL_CATALOG_REFRESH",
                ddl: "7204ba4a646bf264",
                occurrences: 1,
            },
        ],
        reason: "install_catalog_failure_trigger and drop_catalog_failure_trigger both use \
                 with_unrestricted_fixture_connection on the file-backed server DB; the test \
                 installs RAISE(ABORT) before exercising catalog refresh through MemoryServer, \
                 asserts rollback across catalog/provider publication, and drops the trigger \
                 before the next reopen. Bodies read 2026-08-30. The file also names guarded \
                 store doorways, so this sanctioned fixture is declared rather than assigned \
                 an inapplicable machine proof.",
    },
];
