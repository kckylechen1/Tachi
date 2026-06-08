# Credentialed Dispatch Profiles

## Goal

Dispatch profiles may declare credential profile ids so a leader can select one
profile and let Tachi materialize the required Vault-backed env/config/files
before spawning the worker.

## Rules

- Existing built-in profiles must stay credential-free unless they explicitly
  need credentials.
- Credentialed profiles must be opt-in so projects without `.tachi/credentials`
  do not break ordinary dispatch.
- Missing credential profile config should fail with a clear readiness error for
  the selected credentialed profile only.
- Dispatch responses and trajectory events must stay redacted.
- Credential profile allowlists may authorize either the backend agent name or
  the selected dispatch profile name.

## First Profile

`opencode_builder` is the first built-in credentialed profile. It uses the
existing custom/OpenCode command lane and requires the project credential profile
`opencode_shared`.

Projects can provide:

```json
{
  "credential_profiles": {
    "opencode_shared": {
      "provider": "opencode",
      "entries": {
        "api_key": "OPENCODE_ROUTER_SECRET"
      },
      "allowed_consumers": {
        "profiles": ["opencode_builder"]
      },
      "materializers": [
        {
          "type": "config_overlay",
          "source": "api_key",
          "target": "OPENCODE_CONFIG_CONTENT",
          "template": {
            "provider": "openai",
            "apiKey": "{{secret}}"
          }
        }
      ]
    }
  }
}
```

The profile remains safe for normal projects because it is selected only when
the caller passes `profile="opencode_builder"`.
