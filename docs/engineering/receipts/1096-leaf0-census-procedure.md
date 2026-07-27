# #1096 Leaf-0 Lock Census — Regeneration, Review, and Candidate List

> **Receipt artifact:** [`1096-leaf0-global-test-lock-baseline.json`](1096-leaf0-global-test-lock-baseline.json)
>
> **Refs:** #1096 #1476. **Related:** #1094 #1278 #1473 #1475.
>
> This is an **evidence leaf**: no locks were removed, no tests changed, no
> production code touched, no nextest profile modified. Its sole output is the
> checked-in census artifact plus the deterministic validator that keeps it
> honest.

## What the census is

Every live `global_test_lock().lock()` callsite across `tachi-llm` and
`tachi-server`, refreshed against the current branch head. Each entry carries:

| field | source | purpose |
|---|---|---|
| `file`, `line` | `rg` discovery | exact identity (validator catches line drift) |
| `test_or_fn_name` | enclosing `fn` via backwards scan | human-readable anchor |
| `class` | preserved from historical Leaf-0 | five-class schema (all `class2_runtime_config`) |
| `evidence` | preserved from historical Leaf-0 or structural note | audit trail |
| `env_vars_touched` | `EnvRestore::set/unset/remove` + `env::set_var/remove_var` scan | which vars the lock serializes |
| `env_role` | **structural inspection of the constructor under test** (this leaf) | migration discriminator |
| `env_role_evidence` | the constructor(s) detected in the enclosing function body | falsifiable basis for the role |
| `deletion_scope` | file-path match against #1319 targets | effort-triage flag |

## `env_role` classification method

The classifier reads the enclosing function body (bounded by brace-matching,
not a fixed line window) and pattern-matches for two constructor families:

- **Env-reading constructors** (`from_env()`, `LlmClient::new()`,
  `make_server()`, `tachi_home()`, `collect_api_key_status_from_sources()`,
  `model_lanes_json()`, …) → evidence for `behavior_under_test`.
- **Injection constructors** (`new_with_config()`, `new_with_home_for_test()`,
  `make_server_with_temp_home()`, `new_with_vault_db()`, …) → evidence for
  `incidental_delivery`.
- Both found → `mixed`. Neither found → `unknown`.

`unknown` is an **honest** result, not a gap: the test may mutate env for a
helper or a production path the structural scan cannot trace. Per #1476
acceptance, `unknown` callsites **cannot be selected for automated migration**.

**Known limitation:** the classifier inspects the test's own constructor
calls, not the entire production call chain. A test may use an injection
constructor yet still require env for a deeper production path with no
injection overload (e.g. `voyage_rerank_url()` reads `RERANK_VOYAGE_ENDPOINT_ENV`
with no injection seam). Such cases classify as `incidental_delivery`
structurally but are **blocked** for migration without production API widening.
See the candidate-list notes below.

## Regeneration procedure

```bash
# From the repo root, on the branch whose head you want to snapshot:
python3 scripts/lock_census.py regen
python3 scripts/lock_census.py validate   # must print OK
```

`regen` writes the fixture; `validate` re-derives the live callsite set and
diffs it against the fixture. The validator does **not** rewrite the fixture
during ordinary tests — it only reports drift. Commit both the refreshed
fixture and any classification changes in the same commit.

## Review checklist (for the human or cold-review session)

1. Run `python3 scripts/lock_census.py validate` — must be green.
2. Spot-check `incidental_delivery` entries: confirm the named injection
   constructor is real and the env mutation is plausibly removable.
3. Spot-check a sample of `unknown` entries: confirm no obvious injection
   constructor was missed (if one was, widen `INJECTION_PATTERNS` in
   `scripts/lock_census.py` and re-run `regen`).
4. Confirm `deletion_scope=1319` entries are all in Shell/Arena/Task-dispatch
   paths scheduled for deletion by #1319.

## Current snapshot (branch head)

| metric | value |
|---|---|
| total callsites | 256 |
| `behavior_under_test` | 114 |
| `incidental_delivery` | 6 |
| `mixed` | 0 |
| `unknown` | 136 |
| `deletion_scope=1319` | 82 |
| `deletion_scope=none` | 174 |

## Migration candidate list (`env_role=incidental_delivery`, `deletion_scope=none`)

These are the ONLY callsites the structural classifier flags as plausibly
migratable. **No migration is implemented in this leaf.** Each candidate
requires per-callsite verification before a migration leaf proceeds.

| # | file:line | function | injection constructor | env var(s) | prior-triage note |
|---|---|---|---|---|---|
| 1 | `tachi-llm/.../embedding_rerank.rs:142` | `voyage_request_shape_unchanged` | `new_with_config(` | `RERANK_VOYAGE_ENDPOINT_ENV`, `VOYAGE_API_KEY`, `VOYAGE_RERANK_API_KEY` | **BLOCKED** (#1475): provider IS injected, but `voyage_rerank_url()` reads `RERANK_VOYAGE_ENDPOINT_ENV` with no injection overload. Migration requires a production API widening (voyage-endpoint field on `RerankConfig`), which is a separate leaf. |
| 2 | `tachi-llm/.../provider_key_persistence.rs:6` | `provider_key_health_blocking_persist_honors_test_disable_env` | `new_with_vault_db(` | `TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST` | **Unverified**: migratable IFF `LlmClient` can accept the persist-disabled flag as a typed parameter instead of env. Investigate `new_with_vault_db` / `new_with_config` signatures. |
| 3 | `tachi-llm/.../provider_key_persistence.rs:34` | `provider_key_health_persists_off_async_runtime_thread` | `new_with_vault_db(` | `TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST` | Same as #2. |
| 4 | `tachi-llm/.../provider_key_persistence.rs:92` | `provider_key_health_persist_errors_are_visible_in_status` | `new_with_vault_db(` | `TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST` | Same as #2. |
| 5 | `tachi-llm/.../provider_key_persistence.rs:186` | `provider_key_health_reload_clears_local_cooldown_on_external_success` | `new_with_vault_db(` | `TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST` | Same as #2. |
| 6 | `tachi-server/.../status_health/tests.rs:447` | `provider_probe_client_loads_target_db_key_health` | `new_with_vault_db(` | `TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST` | Same as #2. |

**Recommended next migration leaf:** candidates #2–#6 form a coherent cluster
(five callsites sharing `new_with_vault_db` + `TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST`).
If the persist flag can be injected through the existing `LlmClient` constructor
without API widening, these five can migrate together. Candidate #1 is blocked
pending a separate production-endpoint-injection leaf.

## Historical Leaf-0 preservation

The original Leaf-0 artifact (142 callsites, archived 2026-07-14 at
`~/.cache/sigil-eval-archive/`) is preserved as follows:

- `class`, `evidence`, and `env_vars_touched` fields are carried forward from
  the archive wherever a callsite matched by file + nearest line (±5).
- `secondary_classes_present` in `stats` is carried forward verbatim.
- Callsites added since the archive (main drifted from 142 to 256) carry
  `evidence: "structural inspection of <fn>"` and the same five-class
  classification (`class2_runtime_config`) consistent with the historical
  finding that every callsite is env-serialization.
- The schema version is bumped to `"2"` to mark the `env_role` / `deletion_scope`
  additions; the original schema is implicitly `"1"`.
