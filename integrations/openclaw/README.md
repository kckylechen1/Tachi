# Legacy OpenClaw Tachi integration

The MCP memory adapter in this directory is retired. It depended on spawning Tachi with arbitrary process-owned global/project DB paths, the privileged `openclaw`/`operate` profile and legacy tools such as `runtime_info` and `recall_context`. Public Tachi HTTP clients cannot select that privileged surface. Connection attempts now fail before spawning a process or sending HTTP; there is no stdio fallback.

The plugin's static configuration validation, response conversion and local audit/todo helpers remain available in source. Its Tachi-backed memory tools, hooks and native memory capability cannot connect. A future integration requires a separate migration to the standard facade, registered workspace binding and explicit caller identity; there is no replacement plugin shipped here.

Published binary-only installation reference: the [version-pinned Tachi installer](https://raw.githubusercontent.com/kckylechen1/tachi/v2.0.0/scripts/install.sh), with `--skip-plugin`. The installer in this source tree no longer installs or wires the retired plugin.

For current Tachi MCP access, start the resident daemon and configure a supported native HTTP host:

```sh
tachi --daemon --no-project-db --port 6919 serve
python3 scripts/setup_agent_mcp.py --agent claude-code \
  --url http://127.0.0.1:6919/mcp --apply
```

Run the setup helper from the Tachi repository. Model credentials belong in the daemon configuration and must not be copied into HTTP client descriptors. Owner management uses supported `tachi vault`, `tachi hub` and `tachi mcp` CLI commands.

`npm run typecheck` and `npm run test:runtime` verify the retained configuration/response boundaries and refusal before process or network access. The SDK's generic HTTP compatibility test remains independent of the retired plugin connection.
