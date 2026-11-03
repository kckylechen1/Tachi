# Agent credential surfaces

## Current contract

Vault is the credential source for explicitly named API and SSH connections.
Agents use `vault_connections` to discover scoped metadata and prepare a host
CLI command. The command resolves only its declared Vault references at launch.
See [Vault connections](vault-connections.md) for the schema, authorization
boundary and working examples.

| Need | Supported surface |
| --- | --- |
| Reuse an API key across tools | Named API connection with `ENV_NAME=vault:NAME` bindings |
| Inject a project's declared credentials | `.tachi/vault.env` plus `tachi env run` |
| Explicit shell/direnv integration | `tachi env export` for project-bound references only |
| Find the right SSH destination | Named SSH connection referring to an existing OpenSSH alias |
| Pass structured configuration as an environment variable | `config_content_env` credential adapter |
| Pass a temporary configuration file | `config_overlay` within a caller-authorized run directory |
| Manage credentials and pool health | Owner `tachi vault` commands |
| Inspect or clean prior generated files | `tachi vault doctor` and scoped `cleanup` |

The consumer profile allowlists and Vault ACL still apply. An identity-less
standalone CLI cannot impersonate a bound agent. API environment values override
the declared child variables; missing values fail before spawn. The rest of the
parent environment is inherited, so this facility is not an environment sandbox.

## Retired scope

The earlier inventory of harness login/config files was exploratory. It no
longer defines a consumer-registry expansion or a roadmap to own every harness
subscription, OAuth refresh, session blob, browser login, or personal payment
record. The broad auth-broker proposal is [retired](vault-auth-broker.md).

Generic `file_copy` and persistent `config_patch` writers are retired. Use env,
structured config env, or a temporary run-dir overlay. Existing managed records
retain their cleanup semantics; historical file metadata is not deleted during
this code change. See [adapter migration and cleanup](credential-adapters-cleanup.md).

## Encrypted portability

Live project memory databases remain local or project-scoped. The existing sync
commands carry encrypted Vault bundles; they do not silently replicate live
SQLite memory databases or synchronize SSH private keys.

Vault sync bundles are portable ciphertext, not offline-guessing-resistant
backups. The verifier/ciphertext can allow a holder to test password guesses.
Keep the default sync path local and require explicit `--allow-cloud` for
cloud-synced destinations. Prefer `--entries-only` when the import side already
has a matching Vault; entry ciphertext still permits offline guessing.

The existing #576 residual risk remains owner-accepted until recipient-key
encryption. A password-derived AEAD wrapper alone is not a fix because its tag
is itself an offline oracle. Recipient-key encryption remains separate work.
