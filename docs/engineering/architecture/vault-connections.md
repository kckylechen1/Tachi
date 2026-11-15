# Vault connections

## Purpose and boundary

Vault supplies stable credential references for agents and tools. A connection
adds the context needed to use those references: a name, purpose, API environment
bindings or an existing SSH host alias, and optional project/consumer/executor
constraints. Updating a Vault secret keeps the connection reference unchanged.

Connections support local CLI launches. MCP discovers and prepares a recipe;
the host's existing execution authorization controls the actual command.

## Retired Hub MCP credentials

The Hub upstream MCP proxy, connection pool, OAuth login and token refresh are
retired. Configure third-party MCP servers and their authentication directly in
the host. Hub retains only `skill` and `plugin` capabilities; it no longer
publishes `server__tool` or accepts MCP/virtual registrations. Existing database
schema and migrations remain unchanged for on-disk compatibility. Vault API/SSH
connections and explicit local CLI launches below remain supported.

## Register an API connection

First register the credential with the existing Vault setup/secret commands.
The connection JSON contains references only; never put a key or password in its
purpose, endpoint, model, or other metadata fields.

```json
{
  "name": "exa-search",
  "purpose": "Search the web through Exa",
  "target": {
    "kind": "api",
    "env": {"EXA_API_KEY": "vault:EXA_API_KEY"},
    "endpoint": "https://api.exa.ai"
  }
}
```

```sh
tachi vault connection put --file exa-search.json
tachi vault connection show exa-search
tachi vault connection run exa-search --keychain -- python search.py
```

`--keychain` uses the existing macOS Keychain unlock source. Other existing CLI
unlock sources are `--stdin-password` and an owner-protected `--password-file`.
An unlocked MCP process does not unlock a separate CLI process. Connection run
injects only the declared references into the child environment. Other parent
environment variables are inherited, as with `tachi env run`; this is not an
environment sandbox. A missing, denied, invalid, or empty declared secret stops
the launch rather than falling back to the parent's value for that binding.

`endpoint` and `model` are descriptive metadata. Add an explicit Vault binding
when an additional environment value is required. No provider configuration or
generated environment file is rewritten by connection run. The existing
`.tachi/vault.env` workflow remains available for project-wide environment sets.

Connection references use direct secrets, provider accounts, or key pools.
Internal lane slots such as `EXTRACT_API_KEY` are rejected: their encrypted
target pointers cannot be checked by read-only discovery. Reference the actual
account or pool instead. Existing lane-slot env/dispatch workflows retain their
current behavior.

## Register an SSH connection

Configure and trust the host through the system OpenSSH configuration first.

```json
{
  "name": "staging-shell",
  "purpose": "Run maintenance commands on staging",
  "target": {
    "kind": "ssh",
    "alias": "my-staging-host",
    "remote_cwd": "/srv/app"
  }
}
```

```sh
tachi vault connection put --file staging-shell.json
tachi vault connection run staging-shell -- uname -a
```

The CLI invokes system `ssh` with the registered alias. OpenSSH continues to own
HostName, User, IdentityFile, ProxyJump, host-key checks, and any other trusted
host configuration. Vault does not copy private keys or inspect the user's SSH
directory. Discovery never runs `ssh -G`, because OpenSSH configuration may
contain executable directives. Remote commands use POSIX shell quoting; shells
with incompatible syntax are outside this first version's execution contract.

## Agent discovery

Ordinary Lead and Worker profiles expose the read-only `vault_connections` tool:

```json
{"action":"list","kind":"api","query":"search","limit":20}
{"action":"describe","name":"exa-search"}
{"action":"prepare","name":"exa-search"}
```

`prepare` returns an argv prefix including the selected global DB and catalog
revision. Append the CLI unlock option for API connections, any local `--cwd`,
then `--` and the command. Execute through the existing host shell. MCP responses
contain `execution: "not_started"`, `credentials_resolved: false`, and
`connection_check: "not_performed"`; registered metadata is not proof that an
API key works or an SSH host is reachable. Actual resolution occurs during run.

Discovery reports missing credentials, invalid rotation metadata, disabled
connections, a different executor, and requirements for a bound consumer.
Disabled and wrong-executor records remain discoverable within their scope so
the agent can explain the blocker. Restricted records and credential metadata
are omitted when the current consumer is not allowed. Named denied/missing
connections return the same unavailable response.

## Scope and storage

- `allowed_agents`: optional list of bound MCP consumer identities. Empty means
  no additional connection-level constraint; the underlying Vault ACL still
  applies. Every possible pool member must be visible to describe that pool.
- `allowed_projects`: optional list of canonical project IDs. Run
  `tachi vault connection context --cwd /path/to/repository` to obtain the ID.
  CLI derives it from the actual canonical working directory, normalizing
  linked worktrees to the primary repository identity while keeping the child
  process in its requested worktree directory. MCP derives it
  from the current session's resolved repository DB; an unknown project does not
  match a project-restricted record.
- `executor`: optional local hostname constraint. `context` shows the local
  value. This is a routing guard, not authenticated remote machine identity.
- `enabled`: defaults to true. Disabled records cannot run.

MCP accepts no caller-supplied `agent_id`, project, command, or secret value.
Standalone CLI runs with local host authority and no bound agent identity.
Consequently, agent-restricted connections or credentials may be discoverable
to the bound MCP consumer but return `requires_bound_consumer` with no standalone
CLI recipe. This version does not add a consumer-impersonation flag or execution
ticket to bypass that restriction.

The catalog is `<global-db-filename>.connections.json` next to the selected
global DB. Separate DB files have separate catalogs. Writes use an owner-only
atomic replacement under a stable file lock; each successful mutation increments
the revision. `put --replace` is required to change an existing name. `remove`
deletes only the connection registration, retaining the referenced secret.
Catalog files reject unknown fields, duplicate names, unsupported versions,
literal environment values, and credential-bearing endpoint URL fields.

`run --expect-revision N` detects catalog changes made through `put/remove`
before unlock or execution. Hand-editing the catalog without incrementing its
revision is outside this check. The counter does not pin the secret's value:
the same reference resolves the latest Vault value on each run. A recipe is not
an authorization token; all execution constraints are checked again. CLI
`list`/`show` are owner management views of the selected local catalog, including
restricted records, while ordinary MCP discovery is filtered.

## Retired entrypoints and migration

Vault has two explicit local API launch inputs: a saved connection or project
bindings. Both feed the same environment resolver and child runner. There is no
implicit whole-Vault export or persistent generated-env writer.

| Retired entrypoint | Supported replacement |
| --- | --- |
| Bare `tachi env` | `tachi env export` with explicit project bindings |
| `tachi env sync` | `tachi env run` or a named connection; no generated file |
| `tachi vault exec` | `tachi vault connection run NAME -- COMMAND` or `tachi env run -- COMMAND` |
| Raw Vault MCP management and OpenClaw store/retrieve/list bridges | Owner `tachi vault` CLI for management; `vault_connections` for agent discovery |
| `file_copy`, persistent `config_patch` | `env`, `config_content_env`, or temporary run-dir `config_overlay` |
| `config_overlay` with an environment target | Change the adapter type to `config_content_env` |

The raw MCP routes are absent even in Admin. Session-sensitive owner operations
(status, lock, unlock, list, lease, key-result recording and existing-pool rotation) use a closed
`POST /internal/vault` endpoint on the same loopback daemon. It reuses the existing
owner capability, accepts fixed action shapes, and has no tool-name, agent,
consumer, profile, project or command override. Unlock forwards a protected
FIFO path, not an inline master-password JSON field. The client disables proxies,
redirects and retries. A matched daemon's authentication, compatibility or
operation error remains an error; it is not replayed locally. Restart an old
daemon if this endpoint is unavailable.

Local CLI paths without a matching daemon remain supported where their existing
operation allows it. `tachi vault setup-rotation PREFIX --total-keys N --strategy
round_robin` adjusts an existing pool without resubmitting secret values and
requires a matching running daemon unlocked through `tachi vault unlock`.
Without that daemon it refuses before opening a local database. Existing
`set-pool` retains its offline password-source flow.

Removing a code path does not remove user credentials, old generated files or
historical managed metadata. Use scoped [cleanup](credential-adapters-cleanup.md)
for managed materializations and remove obsolete generated env files after
migrating their callers. Provider runtime, legacy dispatch bindings, encrypted
sync and stored credential types retain their compatibility contracts. The
broader [auth-broker proposal](vault-auth-broker.md) is retired.
