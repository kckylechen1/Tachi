#!/usr/bin/env python3
"""
Register shared MCP servers into Tachi Hub.

These MCPs will be proxied through Tachi's connection pool,
eliminating zombie processes across Amp / Gemini / OpenClaw.
"""

import subprocess
import json
import sys
import os
import copy
import select
import time
import argparse

SERVER_BIN = "tachi"
REQUEST_TIMEOUT_SECS = float(os.environ.get("TACHI_REQUEST_TIMEOUT_SECS", "90"))

# MCP servers to register — one connection, all agents share
MCP_SERVERS = [
    {
        "id": "mcp:context7",
        "name": "context7",
        "description": "Context7 — resolve library IDs and query up-to-date documentation for any programming library.",
        "definition": {
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "@upstash/context7-mcp@latest"],
            "tool_exposure": "gateway",
        },
        "policy": {"visibility": "discoverable", "scope": "core-shared"},
    },
    {
        "id": "mcp:exa",
        "name": "exa",
        "description": "Exa — fast intelligent web search and web crawling for AI agents.",
        "definition": {
            "transport": "sse",
            "url": "https://mcp.exa.ai/mcp",
            "tool_exposure": "gateway",
        },
        "policy": {"visibility": "discoverable", "scope": "core-shared"},
    },
    {
        "id": "mcp:longbridge",
        "name": "longbridge",
        "description": "Longbridge — Hong Kong / US stock market data, quotes, positions, and order management.",
        "definition": {
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "mcp-longbridge"],
            "tool_exposure": "gateway",
            "env": {
                "LONGPORT_APP_KEY": "${LONGPORT_APP_KEY}",
                "LONGPORT_APP_SECRET": "${LONGPORT_APP_SECRET}",
                "LONGPORT_ACCESS_TOKEN": "${LONGPORT_ACCESS_TOKEN}",
                "LONGPORT_LANGUAGE": "${LONGPORT_LANGUAGE}",
            },
            "tool_timeout_ms": 30000,
            "max_concurrency": 1,
        },
        "policy": {
            "visibility": "discoverable",
            "scope": "agent-local",
            "owner_agent": "stock",
        },
    },
]

REQUEST_ID = 0
AGENT_CHOICES = ["antigravity", "claude-desktop", "cursor", "all"]


def parse_args():
    parser = argparse.ArgumentParser(
        description="Register shared MCP servers into Tachi Hub with visibility policy"
    )
    parser.add_argument(
        "--scope",
        choices=["global", "project"],
        default="global",
        help="Hub scope to register into",
    )
    parser.add_argument(
        "--disable-hidden",
        action="store_true",
        help="Set enabled=false for policy.visibility=hidden capabilities",
    )
    parser.add_argument(
        "--sync-agent-config",
        dest="sync_agent_config",
        action="store_true",
        default=True,
        help="Auto-update local agent MCP config files after Hub registration (default: enabled)",
    )
    parser.add_argument(
        "--no-sync-agent-config",
        dest="sync_agent_config",
        action="store_false",
        help="Skip local agent MCP config auto-update",
    )
    parser.add_argument(
        "--agent",
        action="append",
        choices=AGENT_CHOICES,
        help="Agent target(s) for auto config sync. Repeatable. Defaults to all.",
    )
    return parser.parse_args()


def next_id():
    global REQUEST_ID
    REQUEST_ID += 1
    return REQUEST_ID


def send_request(proc, request):
    request_str = json.dumps(request) + "\n"
    if proc.stdin is None or proc.stdout is None:
        raise RuntimeError("tachi stdio is not available")
    proc.stdin.write(request_str.encode())
    proc.stdin.flush()

    deadline = time.time() + REQUEST_TIMEOUT_SECS
    while True:
        if proc.poll() is not None:
            raise RuntimeError(f"tachi exited before reply (exit={proc.returncode})")
        remaining = deadline - time.time()
        if remaining <= 0:
            raise TimeoutError(
                f"Timed out waiting for MCP reply after {REQUEST_TIMEOUT_SECS:.1f}s"
            )

        ready, _, _ = select.select([proc.stdout], [], [], remaining)
        if not ready:
            continue

        response_line = proc.stdout.readline().decode().strip()
        if response_line:
            return json.loads(response_line)

        if proc.poll() is not None:
            raise RuntimeError(
                f"tachi exited while reading reply (exit={proc.returncode})"
            )


def call_tool(proc, name, arguments):
    return send_request(
        proc,
        {
            "jsonrpc": "2.0",
            "id": next_id(),
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        },
    )


def parse_csv_list(raw_value):
    return [item.strip() for item in raw_value.split(",") if item.strip()]


def dedupe_items(items):
    seen = set()
    out = []
    for item in items:
        if item in seen:
            continue
        seen.add(item)
        out.append(item)
    return out


def resolve_selected_agents(agent_args):
    if not agent_args or "all" in agent_args:
        return ["all"]
    return dedupe_items(agent_args)


def list_hub_mcp_server_keys(proc):
    resp = call_tool(
        proc,
        "hub_discover",
        {
            "cap_type": "mcp",
            "enabled_only": False,
        },
    )

    if not resp or "error" in resp:
        return []

    try:
        result_text = resp["result"]["content"][0]["text"]
        items = json.loads(result_text)
    except (KeyError, TypeError, json.JSONDecodeError):
        return []

    if not isinstance(items, list):
        return []

    keys = []
    for item in items:
        if not isinstance(item, dict):
            continue
        if item.get("callable") is False:
            continue
        cap_id = item.get("id")
        if not isinstance(cap_id, str) or not cap_id.startswith("mcp:"):
            continue
        key = cap_id.split(":", 1)[1].strip()
        if not key or key == "tachi":
            continue
        keys.append(key)

    return dedupe_items(keys)


def sync_agent_configs(args, remove_server_keys):
    script_path = os.path.join(os.path.dirname(__file__), "setup_agent_mcp.py")
    cmd = [sys.executable, script_path, "--apply", "--command", SERVER_BIN]

    for agent_name in resolve_selected_agents(args.agent):
        cmd.extend(["--agent", agent_name])

    for key in dedupe_items(remove_server_keys):
        cmd.extend(["--remove-server", key])

    print("\nSyncing local agent MCP configs...")
    if remove_server_keys:
        print(f"Consolidating direct MCP keys under tachi: {', '.join(remove_server_keys)}")
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.stdout:
        print(result.stdout.strip())
    if result.returncode != 0:
        err_text = result.stderr.strip() if result.stderr else "unknown error"
        raise RuntimeError(f"setup_agent_mcp.py failed (exit={result.returncode}): {err_text}")


def apply_tool_filters(mcp):
    definition = copy.deepcopy(mcp["definition"])
    env_key = mcp["name"].upper().replace("-", "_")

    default_exposure = os.environ.get("TACHI_DEFAULT_TOOL_EXPOSURE", "").strip()
    per_server_exposure = os.environ.get(f"TACHI_{env_key}_TOOL_EXPOSURE", "").strip()
    exposure_mode = per_server_exposure or default_exposure
    if exposure_mode:
        definition["tool_exposure"] = exposure_mode

    allow_raw = os.environ.get(f"TACHI_{env_key}_ALLOW_TOOLS", "")
    deny_raw = os.environ.get(f"TACHI_{env_key}_DENY_TOOLS", "")
    allow_tools = parse_csv_list(allow_raw)
    deny_tools = parse_csv_list(deny_raw)

    if not allow_tools and not deny_tools:
        return definition, None

    permissions = definition.get("permissions") or {}
    if allow_tools:
        permissions["allow"] = allow_tools
    if deny_tools:
        permissions["deny"] = deny_tools
    definition["permissions"] = permissions

    parts = []
    if exposure_mode:
        parts.append(f"tool_exposure={exposure_mode}")
    if allow_tools:
        parts.append(f"allow={allow_tools}")
    if deny_tools:
        parts.append(f"deny={deny_tools}")
    return definition, "; ".join(parts)


def apply_cap_policy(mcp, definition):
    env_key = mcp["name"].upper().replace("-", "_")
    policy = copy.deepcopy(mcp.get("policy") or {})

    default_visibility = os.environ.get("TACHI_DEFAULT_CAP_VISIBILITY", "").strip()
    server_visibility = os.environ.get(f"TACHI_{env_key}_CAP_VISIBILITY", "").strip()
    visibility = server_visibility or default_visibility
    if visibility:
        policy["visibility"] = visibility

    default_scope = os.environ.get("TACHI_DEFAULT_CAP_SCOPE", "").strip()
    server_scope = os.environ.get(f"TACHI_{env_key}_CAP_SCOPE", "").strip()
    cap_scope = server_scope or default_scope
    if cap_scope:
        policy["scope"] = cap_scope

    owner_agent = os.environ.get(f"TACHI_{env_key}_OWNER_AGENT", "").strip()
    if owner_agent:
        policy["owner_agent"] = owner_agent

    if policy:
        definition["policy"] = policy

    summary_parts = []
    if "visibility" in policy:
        summary_parts.append(f"visibility={policy['visibility']}")
    if "scope" in policy:
        summary_parts.append(f"scope={policy['scope']}")
    if "owner_agent" in policy:
        summary_parts.append(f"owner_agent={policy['owner_agent']}")

    return definition, "; ".join(summary_parts) if summary_parts else None


def main():
    args = parse_args()
    print(f"Registering {len(MCP_SERVERS)} MCP servers into Tachi Hub...")

    proc = subprocess.Popen(
        [SERVER_BIN],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        # Initialize MCP handshake
        send_request(
            proc,
            {
                "jsonrpc": "2.0",
                "id": next_id(),
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "hub-mcp-registrar", "version": "1.0.0"},
                },
            },
        )
        proc.stdin.write(
            json.dumps(
                {"jsonrpc": "2.0", "method": "notifications/initialized"}
            ).encode()
            + b"\n"
        )
        proc.stdin.flush()

        registered = 0
        errors = 0

        for mcp in MCP_SERVERS:
            print(f"\n  Registering {mcp['id']}...")
            try:
                definition, filter_summary = apply_tool_filters(mcp)
                definition, policy_summary = apply_cap_policy(mcp, definition)
                if filter_summary:
                    print(f"    ℹ️  tool filter: {filter_summary}")
                if policy_summary:
                    print(f"    ℹ️  policy: {policy_summary}")

                resp = call_tool(
                    proc,
                    "hub_register",
                    {
                        "id": mcp["id"],
                        "cap_type": "mcp",
                        "name": mcp["name"],
                        "description": mcp["description"],
                        "definition": json.dumps(definition),
                        "scope": args.scope,
                        "version": 1,
                    },
                )

                if not resp:
                    raise RuntimeError("empty response")
                if "error" in resp:
                    raise RuntimeError(resp["error"])

                result_text = resp["result"]["content"][0]["text"]
                data = json.loads(result_text)
                discovery_error = data.get("discovery_error")
                enabled = data.get("enabled", True)
                if "error" in data:
                    raise RuntimeError(str(data["error"]))
                if discovery_error:
                    raise RuntimeError(str(discovery_error))
                if enabled is False:
                    warning = data.get("warning", "capability disabled")
                    raise RuntimeError(str(warning))

                tools = data.get("tools_discovered", "?")
                total_tools = data.get("tools_total", tools)
                filtered = data.get("tools_filtered_out", 0)
                exposure_mode = data.get("tool_exposure", "flatten")
                visibility = (definition.get("policy") or {}).get(
                    "visibility", "listed"
                )

                if args.disable_hidden and visibility == "hidden":
                    call_tool(
                        proc,
                        "hub_set_enabled",
                        {"id": mcp["id"], "enabled": False},
                    )
                    print(f"    ⚠ {mcp['id']}: disabled due to hidden visibility")

                if isinstance(filtered, int) and filtered > 0:
                    print(
                        f"    ✅ {mcp['id']}: {tools}/{total_tools} tools enabled ({filtered} filtered, exposure={exposure_mode}, visibility={visibility})"
                    )
                else:
                    print(
                        f"    ✅ {mcp['id']}: {tools} tools discovered (exposure={exposure_mode}, visibility={visibility})"
                    )
                registered += 1
            except (
                KeyError,
                TypeError,
                json.JSONDecodeError,
                RuntimeError,
                TimeoutError,
            ) as e:
                print(f"    ❌ {mcp['id']}: {e}")
                errors += 1

        # Show final stats
        print(f"\n{'=' * 50}")
        resp = call_tool(proc, "hub_stats", {})
        try:
            result_text = resp["result"]["content"][0]["text"]
            stats = json.loads(result_text)
            print(f"Hub total capabilities: {stats.get('total_capabilities', '?')}")
            print(f"By type: {json.dumps(stats.get('by_type', {}))}")
        except (KeyError, IndexError, TypeError, json.JSONDecodeError) as exc:
            print(f"  warning: could not parse hub_stats response: {exc}", file=sys.stderr)

        print(f"\nRegistered: {registered}")
        if args.sync_agent_config:
            try:
                remove_server_keys = list_hub_mcp_server_keys(proc)
                if not remove_server_keys:
                    remove_server_keys = dedupe_items(
                        [
                            mcp["id"].split(":", 1)[1]
                            for mcp in MCP_SERVERS
                            if mcp.get("id", "").startswith("mcp:")
                        ]
                    )
                sync_agent_configs(args, remove_server_keys)
            except RuntimeError as e:
                print(f"\n⚠ agent config sync failed: {e}")
                errors += 1

        print(f"Errors: {errors}")
        return errors == 0

    finally:
        proc.terminate()
        proc.wait()
        stderr_out = proc.stderr.read().decode()
        if stderr_out:
            # Only show last few lines of stderr
            lines = stderr_out.strip().split("\n")
            relevant = [line for line in lines[-8:] if not line.startswith("[gc]")]
            if relevant:
                print("\n--- Server log ---")
                print("\n".join(relevant))


if __name__ == "__main__":
    success = main()
    sys.exit(0 if success else 1)
