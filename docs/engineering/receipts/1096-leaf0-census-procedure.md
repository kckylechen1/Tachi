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
| `class` | preserved from historical Leaf-0 where `(file, fn_name)` matched | five-class schema |
| `evidence` | preserved from historical Leaf-0 (matched by file + function name, not nearest line) or `structural inspection of <fn>` | audit trail |
| `evidence_provenance` | `historical_leaf0 (committed_prior/archive_fallback)` or `regenerated_structural` | which source the evidence came from |
| `env_vars_touched` | **re-derived** from enclosing function body (not preserved from archive) | which vars the lock serializes |
| `env_role` | structural inspection of the constructor under test (this leaf) | migration discriminator |
| `env_role_evidence` | the constructor(s) detected in the enclosing function body | falsifiable basis for the role |
| `deletion_scope` | file-path match against #1319 physical-deletion targets | effort-triage flag |

> **Important:** `env_vars_touched` is always **re-derived** from the current
> source — it is NOT carried forward from the historical archive. The field
> `evidence` and `class` ARE preserved where `(file, test_or_fn_name)` matches.

## `env_role` classification method

The classifier reads the enclosing function body (bounded by brace-matching,
not a fixed line window) and pattern-matches for two constructor families:

- **Env-reading constructors** (`from_env()`, `LlmClient::new()`,
  `tachi_home()`, `model_lanes_json()`, `collect_api_key_status_from_sources()`,
  `collect_config_env_values()` …) → evidence for `behavior_under_test`.
- **Injection constructors** (`new_with_config()`, `new_with_home_for_test()`,
  `make_server_with_temp_home()`, `new_with_vault_db()`, …) → evidence for
  `incidental_delivery`.
- Both found → `mixed`. Neither found → `unknown`.

**`make_server()` is intentionally NOT an env-reading pattern.** It is a
generic test-server/temp-DB factory, not proof that env parsing is the behavior
under test. A test that calls `make_server()` for a server instance while
mutating env for unrelated setup must NOT be classified `behavior_under_test`
on that basis. Classifier golden tests in `scripts/test_lock_census.py`
(`ClassifierGoldens`) enforce this invariant.

`unknown` is an **honest** result, not a gap: the test may mutate env for a
helper or a production path the structural scan cannot trace. Per #1476
acceptance, `unknown` callsites **cannot be selected for automated migration**.

**Known limitation:** the classifier inspects the test's own constructor
calls, not the entire production call chain. A test may use an injection
constructor yet still require env for a deeper production path with no
injection overload. Such cases classify as `incidental_delivery` structurally
but are **blocked** for migration without production API widening. See the
candidate-list notes below.

## `deletion_scope` narrowing

Only source paths **directly authorized for physical deletion** by #1319 are
marked `deletion_scope=1319`:

- `tests/dispatch_tests/` — Task-dispatch facade tests (#1319 Leaf C)
- `arena_ops/` — Arena tests (#1319 Leaf D)
- `shell_ops/` — Shell tests (#1319 Leaf B)

The **surviving dispatch kernel** (`dispatch_ops/dispatch/`), dispatch prompt
(`dispatch_ops/prompt`), `bootstrap/serve`, and `cli_tool/tool_dispatch` are
NOT blanket-marked — their roots survive #1319.

## Regeneration procedure

```bash
# From the repo root, on the branch whose head you want to snapshot:
python3 scripts/lock_census.py regen
python3 scripts/lock_census.py validate   # must print OK

# Full test suite (from repo root):
python3 -m unittest scripts.test_lock_census
```

`regen` writes the fixture; `validate` re-derives the live callsite set and
diffs it against the fixture. The validator does **not** rewrite the fixture
during ordinary tests — it only reports drift. Commit both the refreshed
fixture and any classification changes in the same commit.

## Review checklist

1. Run `python3 scripts/lock_census.py validate` — must be green.
2. Run `python3 -m unittest scripts.test_lock_census` — all tests green.
3. Spot-check `incidental_delivery` entries: confirm the named injection
   constructor is real and the env mutation is plausibly removable.
4. Spot-check a sample of `unknown` entries: confirm no obvious injection
   constructor was missed.
5. Confirm `deletion_scope=1319` entries are all in Shell/Arena/Task-dispatch
   paths only.
6. Confirm `historical_mapping.archive_matched + archive_unmatched == 142`.

## Current snapshot (branch head)

| metric | value |
|---|---|
| total callsites | 256 |
| `behavior_under_test` | 59 |
| `incidental_delivery` | 6 |
| `mixed` | 0 |
| `unknown` | 191 |
| `deletion_scope=1319` | 10 |
| `deletion_scope=none` | 246 |
| archive rows matched (of 142) | 107 |
| archive rows unmatched | 35 |
| evidence from `archive_fallback` (hand-audited, beat placeholder) | 73 |
| evidence from `committed_prior` (manual or placeholder-only) | 183 |
| total non-placeholder evidence | 115 |
| total placeholder evidence | 141 |

## Migration candidate list — LOWER BOUND, not a safe migration list

> **These candidates are a structural lower bound requiring per-callsite
> review before any migration leaf proceeds.** The classifier identifies
> tests that call an injection constructor; it does NOT verify that every
> env mutation in the test is removable through that seam. Candidate #1
> below is a known false-positive for migration (blocked by a deeper
> env-only production path). Treat this list as a triage input, not a
> work order.

Candidates (`env_role=incidental_delivery`, `deletion_scope=none`): **6**

| # | file:line | function | injection ctor | env var(s) | prior-triage note |
|---|---|---|---|---|---|
| 1 | `tachi-llm/.../embedding_rerank.rs:142` | `voyage_request_shape_unchanged` | `new_with_config(` | `RERANK_VOYAGE_ENDPOINT_ENV` etc | **BLOCKED** (#1475): `voyage_rerank_url()` reads env with no injection seam |
| 2 | `tachi-llm/.../provider_key_persistence.rs:6` | `provider_key_health_blocking_persist_honors_test_disable_env` | `new_with_vault_db(` | `TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST` | unverified; migratable IFF persist flag injectable |
| 3 | `tachi-llm/.../provider_key_persistence.rs:34` | `provider_key_health_persists_off_async_runtime_thread` | `new_with_vault_db(` | same | same |
| 4 | `tachi-llm/.../provider_key_persistence.rs:92` | `provider_key_health_persist_errors_are_visible_in_status` | `new_with_vault_db(` | same | same |
| 5 | `tachi-llm/.../provider_key_persistence.rs:186` | `provider_key_health_reload_clears_local_cooldown_on_external_success` | `new_with_vault_db(` | same | same |
| 6 | `tachi-server/.../status_health/tests.rs:447` | `provider_probe_client_loads_target_db_key_health` | `new_with_vault_db(` | same | same |

**Recommended next migration leaf:** candidates #2–#6 (five callsites sharing
`new_with_vault_db` + `TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST`), IFF
the persist flag can be injected without API widening. Candidate #1 is blocked
pending a separate production-endpoint-injection leaf.

## Historical Leaf-0 preservation

The original Leaf-0 artifact (142 callsites, archived 2026-07-14 at
`~/.cache/sigil-eval-archive/`) is preserved and accounted for:

- **Evidence source preference:** for the same `(file, test_or_fn_name)`,
  non-placeholder evidence is preferred over placeholder evidence.
  - Committed-prior **non-placeholder** (manually edited) evidence beats
    archive evidence — it may contain review-round edits.
  - Archive **non-placeholder** (hand-audited) evidence beats committed-prior
    **placeholder** (`"structural inspection of <fn>"`) — this prevents a
    prior regen's auto-generated placeholder from shadowing the richer
    owner-audited archive row.
  - This logic recovered **73 callsites** that were previously shadowed by
    committed-prior placeholders and now carry hand-audited archive evidence.
- `evidence` and `class` fields are carried forward from the selected source
  wherever a callsite matched by `(file, test_or_fn_name)` — never by nearest
  line to a different function.
- 107 of 142 archive rows matched a live callsite by function name. **Not all
  142 archive rows are injected into the live fixture**: 35 are unmatched
  (function removed, renamed, or line-drifted to a different function) and are
  listed in `historical_mapping.unmatched_entries` for traceability. Of the 107
  matched archive rows, 73 contributed their hand-audited evidence (the other 34
  were superseded by committed-prior non-placeholder evidence from a prior
  review).
- `env_vars_touched` is **re-derived** from current source, not preserved.
- `secondary_classes_present` line references were refreshed: all historical
  references were stale against the current callsite set and dropped.
- Schema version is `"3"`. Provenance counts are in
  `historical_mapping.evidence_provenance_counts`.
