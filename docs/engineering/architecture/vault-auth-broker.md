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

- `crates/memory-core/src/vault.rs`
  - encrypted entries with `api_key`, `oauth_token`, `json_blob`, `cookie`, and
    `other` secret types;
  - `allowed_agents` on `VaultEntry`;
  - provider key rotation and health metadata.
- `crates/memory-server/src/tools/vault_facade.rs`
  - MCP tools for `vault_set`, `vault_get`, `vault_list`,
    `vault_lease_api_key`, and key-result health recording.
- `crates/memory-server/src/credential_profile/`
  - opt-in credential profiles;
  - env/config/file materialization;
  - redacted reports and cleanup metadata.
- `crates/memory-server/src/dispatch_ops/dispatch/credentials.rs`
  - dispatch-time materialization into worker env/config before spawning.
- `crates/memory-server/src/gh_ops/transport.rs`
  - a narrow example of "prefer Vault token, fall back to env" for `GH_TOKEN`.
- `integrations/openclaw/`
  - OpenClaw already exposes Tachi Vault passthrough tools;
  - the OpenClaw plugin is now a thin host adapter, so it should request auth
    from Tachi instead of storing or duplicating secrets.

Existing docs also point in this direction:

- `docs/engineering/architecture/agent-credential-surfaces.md`
- `docs/engineering/architecture/credentialed-dispatch-profiles.md`
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

## CLI/API Shape

Proposed operator commands:

```bash
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

Responses must be machine-readable and redacted by default. Raw secret values
are returned only by explicit resolve/lease calls after policy approval, and
only to the requesting trusted adapter.

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
