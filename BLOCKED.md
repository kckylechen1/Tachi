# BLOCKED — #1680 PR-A: `filter_model_provider_pools` vs. `vault_lock_preserves_env_provider_fallback`

## Status
STOPPED per explicit coordinator instruction (do not self-decide; report and
wait). Everything else in this dispatch is done and committed. This is the
one open item.

## The frozen test in question
`crates/tachi-server/src/tests/vault_tests/env_injection/fallback.rs:1-50`,
`vault_lock_preserves_env_provider_fallback` (unmodified, must stay
unmodified per 冻言不移):

```rust
std::env::set_var("TACHI_ENV_FALLBACK_API_KEY", "env-secret");
...
server.vault_set(Parameters(VaultSetParams {
    name: "TACHI_ENV_FALLBACK_API_KEY".to_string(),
    value: "vault-secret".to_string(),
    ...
})).await.expect("vault_set should succeed");

assert_eq!(
    server.llm.provider_secret_for_tests(&["TACHI_ENV_FALLBACK_API_KEY"]).as_deref(),
    Some("vault-secret")   // <-- fails here (line 28): actual is Some("env-secret")
);

server.vault_lock().await.expect("vault_lock should succeed");

assert_eq!(
    server.llm.provider_secret_for_tests(&["TACHI_ENV_FALLBACK_API_KEY"]).as_deref(),
    Some("env-secret"),
    "locking vault must clear only vault overrides and preserve env fallback"
);
```

## Diagnosis (traced end to end, not guessed)

`TACHI_ENV_FALLBACK_API_KEY` **is not in `API_KEY_DEFS` at all** — grepped
`crates/tachi-server/src/status_ops/status_health/api_keys.rs`, zero hits.
It is a synthetic, test-only sentinel name that exists purely to exercise the
lock→env-fallback mechanism generically, independent of which specific
provider name is involved.

Trigger path for the failing first assertion:
`server.vault_set(...)`
→ `handle_vault_set` (`crates/tachi-server/src/vault_ops/handlers/secrets.rs:91`)
→ `attach_provider_refresh_warning` → `server.refresh_llm_provider_secrets_from_vault()`
→ `provider_config::materialize_for_server` → `materialize_for_server_inner`
(`crates/tachi-server/src/provider_config.rs:317-334`)
→ `resolve_vault_pools(...)` returns a pool map that **does** contain
`TACHI_ENV_FALLBACK_API_KEY → "vault-secret"` (Vault pool loading is
class/registry-blind, `vault_ops::access::load_unlocked_api_key_secret_pools`
admits any standalone `*_API_KEY` entry — this is exactly the seam #1680/D3
names)
→ **my new `filter_model_provider_pools`** (`provider_config.rs:50-58`)
drops it, because `provider_env_keys()` (== `model_provider_env_names()`)
only contains names present in `API_KEY_DEFS` with `class: ModelApi`, and
`TACHI_ENV_FALLBACK_API_KEY` is not in the registry under **any** class.

So `state.secrets["TACHI_ENV_FALLBACK_API_KEY"]` is never populated, and
`LlmClient::select_secret` (`crates/tachi-llm/src/llm/provider_health/selection.rs:4-73`)
falls straight through its vault-pool-cache lookup (empty) to its
process-env fallback branch, returning `"env-secret"` for **both**
assertions — hence the reported `Some("env-secret")` where the first
assertion expects `Some("vault-secret")`.

I confirmed the second assertion (post-lock) does **not** exercise the
filter at all: `vault_lock` → `handle_vault_lock` → `clear_cached_vault_state`
(`crates/tachi-server/src/vault_ops/session.rs:45-48`) calls
`llm.clear_provider_secrets_with_custody(...)`, which empties the whole
in-memory pool cache directly — no re-materialization, no filter involved.
The second assertion passing either way (env fallback) is not the disputed
behavior; the first assertion is.

## Root cause: a genuine design-vs-frozen-test conflict, not a bug in my filter

Before this PR, `materialize_for_server_inner`'s pool seeding was
`resolved_pools = vault_pools.clone()` — **unconditional**: every
Vault-stored `*_API_KEY` entry, registered or not, reached the LLM provider
cache. That is the literal discrimination-2 violation #1680/D3 names
(`EXA_API_KEY`/`TAVILY_API_KEY`/`GOOGLE_SEARCH_API_KEY` sneaking in), and the
frozen design text is explicit about the fix's shape:

> "只放行 ModelApi 类的 pool 名" — admit **only** ModelApi-class pool names.

That is allowlist semantics by construction: anything not found in the
registry — not just the three named SearchApi keys, but literally any
unregistered name, including this test's synthetic sentinel — is now
excluded. My implementation (`filter_model_provider_pools`) matches that
literal instruction exactly; the test I ran it against exposed that the
instruction's necessary side effect is broader than "exclude the three known
SearchApi names."

This is exactly the STOP condition the coordinator named ahead of time: the
failing key is genuinely absent from the registry, so this is a design
question, not an implementation bug I should resolve unilaterally.

## Options (not deciding — flagging for owner/design ruling)

**A. Narrow the filter to a deny-list on registered non-ModelApi names**
(admit anything *not found* in the registry at all; only exclude names the
registry explicitly classifies as SearchApi/Infra). Preserves this frozen
test's assumption unchanged. Risk: reopens the seam for any *future*
unregistered `*_API_KEY` Vault entry — including a legitimate new SearchApi
vendor's key stored before its `ApiKeyDef` lands — which is the "allow
known-good" vs. "deny known-bad" distinction the frozen design's own D3 text
argues *for* allowlist over ("the env-name admission surface stays
compile-time... a code-review-gated change, never a DB write" — a deny-list
reading weakens that property for anything not yet taught to the registry).

**B. Keep the strict allowlist (current implementation) and change what the
test exercises** — e.g. use an already-registered ModelApi name (there is
precedent: several fallback/rotation tests in this same directory already
use `VOYAGE_API_KEY`/`OPENAI_API_KEY`) instead of a synthetic sentinel. This
is a **test change**, which the coordinator explicitly forbade me from
making unilaterally (冻言不移) — flagging as an option for the coordinator to
authorize, not something I did.

**C. Escalate to #1680's design owner for a ruling** on whether "only
ModelApi pool names" was meant as a true allowlist (my reading, matches the
literal design text) or an allowlist-among-registered / deny-list-among-
unregistered hybrid (which the design text doesn't actually say, but which
this one frozen test's fixture implicitly assumed before PR-A existed).

## What I did NOT do
- Did not modify `fallback.rs` (frozen, untouched).
- Did not modify `filter_model_provider_pools`'s semantics to work around
  this (would be a unilateral design call).
- Did not add `TACHI_ENV_FALLBACK_API_KEY` to the registry (would be
  polluting `API_KEY_DEFS` with a name nothing real ever uses, purely to
  satisfy one test's synthetic sentinel — also a unilateral call).

## What I did do in this same pass (already committed, not blocked)
- Fixed my own test bug: `vault_intake_g1680_google_search_is_not_merged_with_google_family`
  wrongly asserted `GOOGLE_API_KEY`'s alias family is `None` — it legitimately
  keeps `Some("google/gemini")` (real, untouched `GEMINI_API_KEY` alias in the
  registry). Only `GOOGLE_SEARCH_API_KEY`'s membership in that family is what
  changed. Fixed the test's expectation, not the production `alias_family()`
  logic (which was already correct).
- Fixed the `ProviderSecret: !Debug` compile failure from the previous
  round (separate commit, already landed).

## Requested next step
Coordinator/design-owner picks A, B, or C (or another option I haven't
listed). I'll implement whichever is chosen in a follow-up pass on this same
branch.
