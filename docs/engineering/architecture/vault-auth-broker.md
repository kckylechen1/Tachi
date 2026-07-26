# Vault Auth Broker For Agent Hosts

## Problem

The current agent stack has too many independent auth islands. A user may log in
through `grokcli`, Codex, OpenCode, Hermes, or OpenClaw, but each host keeps its
own token files, API keys, OAuth refresh material, config fields, and health
state. This creates three recurring problems:

- credentials are duplicated into plaintext host configs;
- every foreground or background agent has to be configured separately;
- auth health, spend, and capability usage are hard to audit across hosts.

The target model is login once, reuse everywhere with policy:

```text
grokcli / codex / opencode / hermes / openclaw
        -> import or register auth
Tachi Vault Auth Broker
        -> encrypted storage + policy + audit
agent host requests a capability
        -> broker resolves a bounded credential or proxy handle
provider plugin / CLI call executes
        -> result, usage, and auth health return to Tachi
```

This is not just "store API keys in Vault". It is a cross-agent auth substrate
that can represent API keys, OAuth refresh tokens, session files, cookies, and
provider-specific account profiles without forcing every host to implement the
same auth logic.

## Existing Surfaces

Tachi already has the primitives needed for the first version:

- `crates/memcore/src/vault.rs`
  - encrypted entries with `api_key`, `oauth_token`, `json_blob`, `cookie`, and
    `other` secret types;
  - `allowed_agents` on `VaultEntry`;
  - provider key rotation and health metadata.
- `crates/tachi-server/src/tools/vault_facade.rs`
  - MCP tools for `vault_set`, `vault_get`, `vault_list`,
    `vault_lease_api_key`, and key-result health recording.
- `crates/tachi-server/src/credential_profile/`
  - opt-in credential profiles;
  - env/config/file materialization;
  - redacted reports and cleanup metadata.
- `crates/tachi-server/src/dispatch_ops/dispatch/credentials.rs`
  - dispatch-time materialization into worker env/config before spawning.
- `crates/tachi-server/src/gh_ops/transport.rs`
  - a narrow example of "prefer Vault token, fall back to env" for `GH_TOKEN`.
- `integrations/openclaw/`
  - OpenClaw already exposes Tachi Vault passthrough tools;
  - the OpenClaw plugin is now a thin host adapter, so it should request auth
    from Tachi instead of storing or duplicating secrets.

Existing docs also point in this direction:

- `docs/engineering/architecture/agent-credential-surfaces.md`
- `docs/engineering/architecture/credentialed-dispatch-profiles.md`

### Credential precedence and observable custody

This is the canonical precedence contract for provider materialization:

- Durable daemon self-materialization is **Vault-wins**. If a configured or
  inherited value for the same key differs, the daemon warns with the key name
  and the `tachi vault set NAME` remediation only; it never logs either value.
  Identical values are not a conflict and do not warn.
- `tachi vault exec` and default/fill-missing legacy child injection are
  **caller-wins/fill-missing**. Existing caller values remain intact; Vault
  materialization supplies only absent names. The explicit legacy setting
  `TACHI_VAULT_CHILD_ENV=all` is an opt-in operator override that replaces
  inherited names; it is neither the default nor silent. Credential-profile
  materializers follow the precedence declared by their explicit profile
  contract.
- A persistent OpenCode `apiKey` must be an `{env:NAME}` reference. The
  decrypted credential belongs in a bounded runtime environment, not in an
  OpenCode config file.
- `tachi vault doctor --providers` is read-only. Without an explicit password
  source it remains metadata-only and reports value comparison as `UNKNOWN`.
  With `--stdin-password`, `--keychain`, or `--password-file`, it may
  decrypt only provider-referenced Vault entries through the report-only path,
  compare fixed-size hashes in memory, and emit only `MATCH`/`MISMATCH`;
  it never touches access counters/timestamps, refreshes provider caches, or
  prints values, digests, lengths, or sensitive alias targets. The CLI cannot
  observe external OAuth stores or the daemon's live last-known-good cache, so
  source completeness remains `INCOMPLETE` and an unobservable source remains
  `UNKNOWN`—never `CLEAN`, `dead`, or `orphaned`.
- `docs/engineering/architecture/credential-adapters-cleanup.md`
- `docs/engineering/architecture/agent-host-substrate.md`

## Design

### 1. Auth Identity Model

Add a logical auth account layer above raw Vault entries.

```rust
struct VaultAuthAccount {
    id: String,                 // xai.default, openai.codex-main
    provider: String,           // xai, openai, kimi, deepseek, github
    account_label: String,      // redacted user-facing label
    auth_kind: AuthKind,        // api_key | oauth | session_blob | cookie | cli_profile
    vault_entry: String,        // existing encrypted VaultEntry name
    source: AuthSource,         // imported_from_cli | manually_set | broker_generated
    source_host: Option<String>,// grokcli, codex, openclaw, opencode, hermes
    capabilities: Vec<String>,  // chat, coding, web_search, repo, messaging
    scopes: Vec<String>,        // provider-specific scopes, redacted by default
    status: AuthStatus,         // ok | expired | refresh_needed | revoked | unknown
    last_probe_at: Option<DateTime>,
    metadata_json: String,      // fingerprints, account ids, file origin, no secrets
}
```

The raw secret stays in existing Vault storage. `VaultAuthAccount` is metadata
and policy context around that secret.

### 2. Importers

Importers discover existing auth material and copy it into Vault under explicit
operator control.

Initial importer classes:

- `grokcli` / xAI
  - detect CLI auth files or API key env/config;
  - store OAuth/session JSON as `json_blob` or API key as `api_key`;
  - create `xai.default` / `grok.default` account metadata.
- `codex`
  - detect `~/.codex/auth.json` and relevant config;
  - store account/session material as encrypted `json_blob`;
  - keep restore explicit because Codex owns its first-party auth files.
- `opencode`
  - import env/provider API keys and config-backed keys;
  - prefer materialization into process env or config overlay.
- `openclaw`
  - import plaintext fields that doctor reports today:
    `gateway.auth.token`, `models.providers.*.apiKey`,
    `channels.telegram.accounts.*.botToken`,
    plugin provider keys.
- `hermes`
  - import existing credential pools and OAuth/session pools as logical account
    metadata plus Vault entries.

Importers must never log secret values. Reports show source path, provider,
account label, fingerprint, secret type, and intended account id.

### 3. SecretRef Resolver

Provide a stable SecretRef grammar for agent hosts:

```text
tachi-vault://auth/xai/default
tachi-vault://auth/openai/codex-main
tachi-vault://auth/kimi/coding
tachi-vault://secret/telegram/yaya-bot-token
```

Resolver output is intentionally host-specific:

- API-key providers can receive an env map or config value.
- OAuth/session providers should receive either:
  - a temporary restored auth file under a Tachi-managed run directory; or
  - a broker handle if the provider can be called through Tachi instead of
    exposing the token to the host.
- Messaging credentials such as Telegram bot tokens may be materialized into
  OpenClaw config or runtime env only for authorized channel hosts.

### 4. Policy

The broker authorizes credential use by actor and context:

```yaml
auth_policies:
  xai.default:
    allow_agents: [openclaw-main, opencode-builder, codex-worker]
    allow_hosts: [openclaw, opencode, hermes]
    allow_repos: ["kckylechen1/tachi", "Quant_Analyzer_2026"]
    deny_capabilities: [messaging_send]
    max_cost_tier: standard

  telegram.yaya:
    allow_hosts: [openclaw]
    allow_agents: [main, ops]
    allow_capabilities: [messaging_send, messaging_receive]
```

This should reuse `VaultEntry.allowed_agents` for the simple case, but needs a
separate policy model for host, repo, capability, lane, and cost constraints.

### 5. Host Integration

OpenClaw should be the first host adapter because it already has plaintext
secret warnings and a Tachi plugin.

Phase 1 OpenClaw integration:

- Add a Tachi MCP tool such as `vault_auth_resolve`.
- Add OpenClaw plugin helper code that resolves:
  - provider API keys for provider plugins;
  - Telegram bot tokens;
  - gateway/auth tokens if OpenClaw supports SecretRef/env indirection.
- Keep OpenClaw plugin thin:
  - no local token database;
  - no independent policy engine;
  - no plaintext logging.

OpenCode and Hermes can follow through existing credential profiles and
materializers. Codex CLI should be import/restore first, then brokered execution
only where the Codex surface supports it.

### 6. Audit And Health

Every resolve or lease should emit a redacted auth event:

```json
{
  "event_type": "auth.resolve",
  "actor": "openclaw-main",
  "host": "openclaw",
  "provider": "xai",
  "account_id": "xai.default",
  "capability": "chat",
  "repo": "kckylechen1/tachi",
  "status": "granted",
  "fingerprint": "sha256:...",
  "run_id": "..."
}
```

Provider calls should feed back:

- success/failure;
- 401/403 auth failures;
- 429 rate limits and cooldowns;
- usage and cost when available.

This can reuse the existing Vault key health path for API keys and add account
health for OAuth/session blobs.

### 7. Runtime Availability

Agent hosts must not have to know whether the in-process Vault key is currently
cached, expired, or recoverable from a local secure store. All agent-facing
credential reads should go through a resolver that has one consistent sequence:

1. check the cached unlocked Vault key;
2. if the key is missing or expired, attempt local secure-store auto-unlock once
   (macOS Keychain today);
3. retry the Vault read after successful auto-unlock;
4. fall back only to explicitly allowed provider/env caches;
5. return a machine-readable locked/unavailable error when no authorized secret
   can be materialized.

This matters for long-running agents. A daemon can keep provider keys cached
while `vault_status` reports `locked`, and a stdio adapter can forward memory
reads while direct Vault reads fail. Broker health should therefore report these
states separately:

- Vault storage initialized/uninitialized;
- session key cached/unlocked;
- secure-store auto-unlock configured and last attempt status;
- provider cache populated from Vault versus env fallback;
- last credential resolver failure by host and provider.

OpenClaw, Hermes, OpenCode, Codex, and background dispatch lanes should call
the same resolver path. Host adapters should not each implement their own
locked-Vault fallback semantics.

## CLI/API Shape

Proposed operator commands:

```bash
tachi vault intake discover --host openclaw
tachi vault intake plan --host openclaw --output plan.json
tachi vault intake apply --from plan.json
tachi vault intake probe --provider all
tachi vault intake consolidate --dry-run

tachi vault auth discover --host grokcli
tachi vault auth import --host grokcli --account default
tachi vault auth list
tachi vault auth probe xai.default
tachi vault auth resolve xai.default --consumer openclaw --capability chat
tachi vault auth materialize openclaw --profile local-main --apply
tachi vault auth cleanup --consumer openclaw --apply
```

Proposed MCP tools:

- `vault_auth_list`
- `vault_auth_discover`
- `vault_auth_import`
- `vault_auth_resolve`
- `vault_auth_record_result`
- `vault_auth_probe`
- `vault_intake_discover`
- `vault_intake_plan`
- `vault_intake_apply`
- `vault_intake_probe`
- `vault_intake_consolidate`

Responses must be machine-readable and redacted by default. Raw secret values
are returned only by explicit resolve/lease calls after policy approval, and
only to the requesting trusted adapter.

## Intake Workflow

The missing operational entrypoint is a Vault intake lane. Existing commands
cover only narrow cases:

- `tachi vault setup-keys` prompts through a fixed provider-key list;
- `tachi vault set` stores one value;
- `tachi vault set-pool` stores one rotation pool from stdin;
- credential profiles materialize known Vault entries but do not discover,
  import, probe, or consolidate them.

P0 intake must make Vault manageable when keys are duplicated, stale, or spread
across host configs.

### Intake Sources

Initial sources should be read-only discoverers:

- env files: `~/.secrets/master.env`, `~/.tachi/config.env`, project
  `.env`/`.tachi/vault.env`;
- OpenClaw: plaintext fields currently reported by `openclaw doctor`;
- OpenCode: provider `apiKey` config and MCP env maps;
- Hermes: `.env`, credential pools, OAuth/session pool metadata;
- Codex and grokcli: auth/session files, imported as `json_blob` only after an
  explicit apply step;
- current Tachi Vault metadata and key-health rows.

Discovery output must redact values and include only:

- source path;
- logical provider/key name;
- secret type;
- fingerprint;
- current Vault match status;
- health/probe status if known;
- suggested action.

### Intake Actions

The intake planner should classify each candidate:

- `skip_existing_same_fingerprint`
- `import_new`
- `replace_stale`
- `merge_alias`
- `promote_to_pool`
- `mark_auth_failed`
- `unverified_external_state` (never a removal recommendation merely because
  a supported discovery source did not observe the entry)

It should also detect semantic duplicates. Examples from the current Vault:

- `KIMI_API_KEY` and `MOONSHOT_API_KEY` may represent the same Moonshot/Kimi
  credential family but different downstream env names;
- `GOOGLE_API_KEY`, `GEMINI_API_KEY`, and `GOOGLE_SEARCH_API_KEY` may share an
  account but serve different APIs;
- `SUMMARY_*`, `EXTRACT_*`, `REASONING_*`, and `DISTILL_*` are lane configs,
  not all independent API keys;
- LongPort/LongBridge entries are broker/data credentials and should not be
  mixed with LLM provider pools.

The planner should never merge by name alone. It should use exact fingerprint
matches when the Vault is unlocked, and provider-specific alias rules when it is
locked.

### Probe And Health

Intake should make stale keys visible:

- API-key providers get lightweight provider probes when supported;
- OAuth/session blobs get expiry/refresh metadata extraction where possible;
- 401/403 should mark auth failed;
- 429 should mark only the concrete key member as cooling down;
- unknown probe surfaces remain `unprobed`, not `ok`.

Probe results should feed existing `vault_key_health` for API-key entries and
future auth-account health for OAuth/session entries.

### Apply Safety

`tachi vault intake apply` should be explicit and reversible:

- dry-run by default;
- requires unlock/password only when importing or comparing fingerprints;
- writes a redacted plan artifact;
- creates backups before patching host configs;
- stores raw secrets only in Vault;
- for replaced/archived entries, records old entry metadata and fingerprint, not
  plaintext values.

## Migration Plan

1. Add metadata schema for auth accounts and policies.
2. Add redacted list/discover output for existing local auth surfaces.
3. Implement API-key importers first: Kimi, DeepSeek, Tavily, Exa, Z.ai,
   Google Vertex, Telegram bot token.
4. Implement OpenClaw materialization/SecretRef resolution and remove
   plaintext OpenClaw config fields.
5. Implement OAuth/session importers: xAI/Grok, Codex, OpenAI, GitHub.
6. Add account health/probe and continuity/audit events.
7. Add cleanup and restore commands for generated auth files.

## Non-goals

- Do not silently take ownership of first-party OAuth files.
- Do not sync live memory databases as part of auth sync.
- Do not expose long-lived credentials to untrusted workers.
- Do not make every host link directly to Tachi internals; adapters should talk
  through MCP/ACP/CLI contracts.

## Acceptance Criteria

- `tachi vault auth discover` can report existing OpenClaw/Codex/OpenCode/Hermes
  auth surfaces without leaking values.
- A user can import a provider credential once and authorize at least two hosts
  to use it.
- OpenClaw can run with Tachi-backed SecretRefs for the fields that currently
  trigger plaintext secret warnings.
- Resolve/lease calls produce redacted audit events and update auth/key health.
- Denied access fails closed with a clear machine-readable error.
- Cleanup can remove or mark Tachi-managed materializations without touching
  unrelated host config.
