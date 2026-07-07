# Agent Credential Surfaces

This note records the local credential surfaces observed while designing Tachi
Vault as the shared source of truth for agent API keys and auth material. It is
intentionally value-free: paths, field names, and env names are documented, but
no secret values are included.

## Decision

Tachi should centralize credential storage, rotation, health checks, and cloud
sync, but it should not require every agent runtime to read Tachi directly.
Different harnesses already expect different auth shapes, so Tachi needs a
consumer registry plus materializers that render Vault entries into each
consumer's native format.

This splits the system into five layers:

1. Vault source of truth: encrypted logical secrets and key pools.
2. Key-pool runtime: round-robin selection, rate-limit cooldown, provider probe
   state, and explicit precedence over env/config values.
3. Encrypted sync: portable Vault ciphertext bundles for multi-machine setup.
4. Consumer registry: one record per agent/tool describing where credentials
   are read and which Vault aliases should satisfy them.
5. Materializers: safe renderers for env injection, config overlays, config
   patches, auth-file blobs, and command helpers.

The important boundary is that live project memory databases stay local or
project-scoped. Cloud sync may carry encrypted Vault material, but it should not
silently replicate live SQLite memory DBs.

Vault sync bundles are portable ciphertext, not offline-guessing-resistant
backups. The bundle carries enough verifier/ciphertext material for anyone who
obtains the file to test Vault password guesses offline. Keep the default sync
path local, require explicit `--allow-cloud` for cloud-synced destinations, and
do not describe the signed bundle as safe merely because rows are encrypted.

## Observed Consumers

### OpenCode

Observed files:

- `~/.config/opencode/opencode.json`
- `~/.config/opencode/oh-my-openagent.jsonc`
- `~/.config/opencode/.opencode/opencode.json`

Observed surfaces:

- MCP environment map:
  - `mcp.tachi.environment.SILICONFLOW_API_KEY`
  - `mcp.tachi.environment.VOYAGE_API_KEY`
  - `mcp.tachi.environment.TACHI_PROFILE`
- Provider API key fields:
  - `provider.anthropic.options.apiKey`
  - `provider.google.options.apiKey`
  - `provider.moonshot.options.apiKey`
  - `provider.openai.options.apiKey`
  - `provider.zhipuai-coding-plan.options.apiKey`
- Remote MCP URL:
  - `mcp.exa.url`

Recommended materializer:

- Prefer env placeholders in OpenCode config and inject the selected Vault value
  into the launched process.
- Avoid literal `apiKey` values in JSON configs when the provider supports env
  references.
- For remote MCPs such as Exa, materialize the required API key into the exact
  launcher env used by that MCP instead of duplicating it in multiple configs.

### Hermes

Observed files:

- `~/.hermes/.env`
- `~/.hermes/config.yaml`
- `~/.hermes/auth.json`
- `~/Desktop/Quant_Analyzer_2026/.hermes/.env`
- `~/Desktop/Quant_Analyzer_2026/.hermes/config.yaml`
- `~/Desktop/Quant_Analyzer_2026/.hermes/auth.json`

Observed env names include:

- `ANTHROPIC_API_KEY`
- `BROWSERBASE_API_KEY`
- `EXA_API_KEY`
- `FAL_KEY`
- `FIRECRAWL_API_KEY`
- `GEMINI_API_KEY`
- `GITHUB_TOKEN`
- `GLM_API_KEY`
- `GOOGLE_API_KEY`
- `GROQ_API_KEY`
- `HF_TOKEN`
- `HONCHO_API_KEY`
- `KIMI_API_KEY`
- `KIMI_CN_API_KEY`
- `MINIMAX_API_KEY`
- `MINIMAX_CN_API_KEY`
- `OPENROUTER_API_KEY`
- `PARALLEL_API_KEY`
- `TAVILY_API_KEY`
- `TINKER_API_KEY`
- `VOICE_TOOLS_OPENAI_KEY`

Observed config/auth surfaces:

- `model.provider`
- `fallback_providers`
- `credential_pool_strategies`
- repeated `api_key` and `base_url` fields
- `credential_pool.<provider>.<index>.source`
- `credential_pool.<provider>.<index>.auth_type`
- `credential_pool.<provider>.<index>.secret_fingerprint`
- OAuth/session pools such as `openai-codex`, `xai-oauth`, `qwen-oauth`, and
  `nous`

Recommended materializer:

- Hermes is closest to the target model because it already has credential pools
  and pool strategies. Tachi should populate env keys or generate a Hermes pool
  fragment rather than replacing Hermes auth logic.
- API-key providers can be rendered as env entries or pool sources such as
  `env:OPENROUTER_API_KEY`.
- OAuth/session providers should be handled as encrypted file blobs or explicit
  auth-file copies. They should require a narrower consent boundary than simple
  API-key env injection.

### OpenClaw

Observed files:

- `~/.openclaw/openclaw.json`
- `~/.openclaw/service-env/ai.openclaw.gateway.env`
- `~/.openclaw/service-env/ai.openclaw.gateway-env-wrapper.sh`
- `~/.openclaw/identity/device-auth.json`
- `~/.openclaw/memory/main.sqlite.bak.*`

Observed surfaces:

- `plugins.entries.exa.config.webSearch.apiKey`
- `plugins.entries.tavily.config.webSearch.apiKey`
- `models.providers.google-vertex.apiKey`
- `gateway.auth.token`
- `channels.telegram.accounts.*.botToken`
- `auth.profiles.*`
- device/session auth under `~/.openclaw/identity/device-auth.json`

Recommended materializer:

- Use JSON patch style updates for known config paths, preserving unrelated
  OpenClaw config. Do not rewrite the whole file.
- Prefer env-backed plugin configuration if OpenClaw adds support for it.
- Treat device/session auth files as encrypted blobs with explicit import/export
  commands, not as routine env keys.
- Keep OpenClaw's existing config write safety: a suspiciously large config
  shrink should fail rather than overwriting user state.

### Codex And Claude Code

Observed surfaces:

- Codex:
  - `~/.codex/auth.json`
  - `~/.codex/config.toml`
- Claude Code:
  - `~/.claude/settings.json`
  - `~/.claude/config.json`
  - `~/.claude/.mcp.json`
  - `~/.claude/settings.local.json`

Recommended materializer:

- Keep first-party OAuth/session files owned by the first-party tool whenever
  possible.
- Tachi may store encrypted snapshots for machine migration, but restore should
  be explicit and reversible.
- Provider SDK keys and MCP keys are better handled through env injection or
  command-scoped launch wrappers.

## Registry Shape

The next feature should add a registry shaped roughly like this:

```yaml
consumers:
  opencode:
    materializer: env_overlay
    env:
      VOYAGE_API_KEY: voyage.embed
      SILICONFLOW_API_KEY: siliconflow.chat
      EXA_API_KEY: exa.search

  hermes:
    materializer: env_file
    env:
      OPENROUTER_API_KEY: openrouter.chat
      GLM_API_KEY: zhipu.chat
      KIMI_API_KEY: moonshot.chat
      TAVILY_API_KEY: tavily.search

  openclaw:
    materializer: json_patch
    paths:
      plugins.entries.exa.config.webSearch.apiKey: exa.search
      plugins.entries.tavily.config.webSearch.apiKey: tavily.search
      models.providers.google-vertex.apiKey: google.vertex
```

The registry should be redacted by default. Diagnostics should show aliases,
fingerprints, source type, freshness, and last probe state, never raw secret
values.

## Doctor Checks

The doctor surface should detect:

- literal API keys in known config paths where env placeholders are preferred;
- env names referenced by a consumer but missing from Vault;
- Vault aliases that exist but have no consumer mapping;
- stale key-pool members with repeated 401 or 429 failures;
- config drift where the materialized consumer value no longer matches the
  selected Vault alias fingerprint;
- auth files with unsafe permissions;
- cloud sync bundles that are newer than local Vault state but not imported.

For 429 handling, the provider runtime should mark only the concrete key member
as cooling down and retry the next available member under the same logical
alias. For 401 handling, the member should be marked unhealthy until an explicit
probe or replacement succeeds.

## Practical Conclusion

Centralizing in Tachi is the right direction. The important refinement is that
Tachi should centralize authority, not homogenize every downstream tool's auth
format. The user-facing workflow should be:

1. Put keys and session blobs into Tachi Vault once.
2. Sync encrypted Vault ciphertext across machines when desired.
3. Run `tachi vault materialize <consumer>` to render only the needed auth shape.
4. Launch agents through wrappers that inject short-lived env/config overlays.
5. Let doctor/probe report drift, 401, 429, and missing mappings.
