# Vault scope: retirement of the broad auth-broker proposal

Status: retired proposal; current product contract is [Vault connections](vault-connections.md).

## Decision

Vault centralizes credentials that an agent or tool explicitly needs for an API
or an existing SSH connection. The supported flow is to register a secret once,
reference it by a stable name, discover a scoped connection through MCP, and run
an explicitly authorized command with that connection.

The earlier proposal to own harness subscriptions, first-party OAuth sessions,
cookies, auth-file restoration, and cross-host login refresh is retired. Its
proposed `tachi vault auth` commands and broad host importers are not an active
roadmap or a promise of implemented functionality. Personal passwords and
payment records are outside this module's product scope.

## Retained capabilities

- Encrypted Vault entries, key derivation, ACL enforcement, access accounting,
  pool rotation and provider key health remain in the existing core.
- Owner CLI commands manage initialization, unlock/lock, entries, pools and
  diagnostics. Session-sensitive commands use the existing local daemon through
  a fixed, authenticated owner endpoint. A matched daemon error is not replayed
  through a local or MCP fallback.
- `vault_connections` exposes scoped `list`, `describe` and `prepare` metadata.
  It neither returns credentials nor authorizes execution.
- Named API connections and project `.tachi/vault.env` bindings share the
  explicit environment resolver. A missing, denied or empty declared value
  prevents a child launch; the binding does not fall back to the parent's value.
- SSH connections reference system OpenSSH aliases. OpenSSH continues to own
  private keys, host trust and connection policy.
- Credential profiles support `env`, `config_content_env`, and temporary file
  `config_overlay` within a caller-authorized run directory. Historical managed
  materializations remain inspectable and cleanable.

The existing provider-account metadata, intake/reconcile checks, encrypted
sync format and credential enum values remain compatible. Their presence does
not make the retired OAuth broker a supported runtime.

## Migration

See [Vault connections](vault-connections.md#retired-entrypoints-and-migration)
for old CLI/MCP entrypoints and [Credential adapters and cleanup](credential-adapters-cleanup.md)
for retired file adapters. Existing user data is not erased by this retirement.
