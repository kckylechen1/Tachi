# Legacy OpenClaw Tachi integration

The OpenClaw Tachi plugin is retired and its executable source, plugin manifest, npm package and dedicated build/package jobs have been removed. This directory retains only retirement guidance and historical release notes. No memory provider, tools or lifecycle hooks are shipped here.

The former plugin depended on process-owned database paths, the privileged `openclaw`/`operate` profile and legacy runtime tools. Its local todo, audit and usage helpers were plugin-only features and are retired with it. The independent `crates/memory-node` native binding and shared Rust memory/runtime APIs remain available.

Before upgrading a source checkout used as an installed OpenClaw extension, disable the legacy Tachi extension and its memory-provider selection in OpenClaw. Existing installed copies and generated JavaScript are not removed automatically; updating the Tachi binary alone does not uninstall them. Preserve existing memory databases, todo files, audit/usage logs and configuration. There is no automatic data migration or replacement OpenClaw plugin.

Published binary-only installation reference: the [version-pinned Tachi installer](https://raw.githubusercontent.com/kckylechen1/tachi/v2.0.0/scripts/install.sh), with `--skip-plugin`. The installer in this source tree installs only the Tachi HTTP service. The old `scripts/install_openclaw_ext.sh` entry point refuses without installing, starting a service or changing configuration.

For current Tachi MCP access, start the resident daemon and configure a supported native HTTP host:

```sh
tachi --daemon --no-project-db --port 6919 serve
python3 scripts/setup_agent_mcp.py --agent claude-code \
  --url http://127.0.0.1:6919/mcp --apply
```

Run the setup helper from the Tachi repository. Model credentials belong in the daemon configuration and must not be copied into HTTP client descriptors. Owner management uses supported `tachi vault`, `tachi hub` and `tachi mcp` CLI commands.

The HTTP setup helper supports the hosts named by its `--help`; this example does not provide an OpenClaw adapter or grant public HTTP callers an owner-management profile.
