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
typed OpenCode backend and requires the project credential profile
`opencode_shared`.

Projects can provide:

```json
{
  "credential_profiles": {
    "opencode_shared": {
      "provider": "opencode",
      "entries": {
        "api_key": "ZHIPUAI_API_KEY"
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
            "providers": {
              "zhipuai-coding-plan": {
                "settings": {
                  "apiKey": "{{secret}}"
                }
              }
            }
          }
        }
      ]
    }
  }
}
```

The profile remains safe for normal projects because it is selected only when
the caller passes `profile="opencode_builder"`.

## OpenCode CLI compatibility

The CLI profile requires OpenCode v2 (verified against 2.2.5). Tachi starts
`opencode --standalone run --model <resolved-model> <prompt>` in the execution
grant's working directory. `--standalone` owns a private service for the CLI
lifetime; it does not attach the dispatch to the operator's background service.
The removed v1 `--pure` flag is not accepted by v2.

The credential overlay above uses the v2 `providers.<id>.settings.apiKey` schema
and matches the built-in `zhipuai-coding-plan/glm-5.2` model. Provision the named
Vault entry and restrict the profile allowlist to the intended consumers. Change
both provider and model together when configuring another provider.

Standalone service lifetime is not a filesystem sandbox or configuration
isolation guarantee. OpenCode still discovers host configuration by default.
For an isolated canary, use a disposable home/config/data/state/cache directory,
disable project and Claude configuration discovery, and provision only the
required credentials. Do not enable `--auto` to bypass permission prompts;
Tachi continues to reject unsupported sandbox and permission-profile requests.

The explicit `opencode_serve` transport still uses the legacy `--attach`/`--dir`
protocol and is outside this v2 CLI support. Leave it unselected for v2; an
unreachable legacy server falls back to the v2 CLI command.
