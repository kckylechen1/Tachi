#!/usr/bin/env python3
"""
Plan legacy shared MCP registration; write mode is retired.

Hub registration and standard client setup are separate from direct MCP routing.
Existing direct MCP client entries are preserved.
"""

import subprocess
import json
import sys
import os
import copy
import argparse

if __package__:
    from . import setup_agent_mcp as agent_setup
else:
    import setup_agent_mcp as agent_setup

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

AGENT_CHOICES = sorted([*agent_setup.AGENT_TARGETS, "all"])


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
        help="Require hidden registrations to be confirmed disabled (no unscoped enable/disable writes)",
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
    parser.add_argument(
        "--agent-transport",
        choices=["http", "stdio"],
        default="http",
        help="Client MCP transport (default: native Streamable HTTP)",
    )
    parser.add_argument(
        "--daemon-url",
        help="Fixed resident daemon /mcp endpoint, required for HTTP client sync",
    )
    parser.add_argument(
        "--agent-config-scope",
        choices=["project", "user"],
        default="project",
        help="Client config location (default: project, bound to the calling workspace)",
    )
    parser.add_argument(
        "--workspace-root",
        help="Workspace directory on the daemon host (project scope defaults to current directory)",
    )
    parser.add_argument(
        "--sync-only",
        action="store_true",
        help="Verify approved, enabled Hub entries and sync without re-registering them",
    )
    parser.add_argument("--dry-run", action="store_true", help="Print the registration plan without starting Tachi or writing config")
    args = parser.parse_args()
    if args.sync_only and not args.sync_agent_config:
        parser.error("--sync-only requires agent config sync")
    return args


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


def tool_result(resp):
    """Reject transport, MCP tool, and payload errors before using evidence."""
    try:
        if not isinstance(resp, dict) or "error" in resp:
            raise ValueError("MCP error response")
        result = resp["result"]
        if result.get("isError", False) is not False:
            raise ValueError("MCP tool returned isError")
        content = result["content"]
        if len(content) != 1 or content[0].get("type") != "text":
            raise ValueError("expected one text result")
        data = json.loads(content[0]["text"])
        if isinstance(data, dict) and "error" in data:
            raise ValueError("tool payload contains error")
        return data
    except (KeyError, IndexError, TypeError, AttributeError, ValueError) as exc:
        raise RuntimeError(f"Invalid MCP tool evidence: {exc}") from exc


def confirm_hub_mcp_servers(items, scope):
    if not isinstance(items, list) or not items:
        raise RuntimeError("Hub discovery is empty or invalid; preserving direct MCP entries")
    for mcp in MCP_SERVERS:
        matches = [item for item in items if isinstance(item, dict)
                   and item.get("id") == mcp["id"]]
        if len(matches) != 1:
            raise RuntimeError(f"Missing or ambiguous Hub evidence for {mcp['id']}")
        item = matches[0]
        if (item.get("callable") is not True or item.get("enabled") is not True
                or item.get("review_status") != "approved"
                or item.get("cap_type") != "mcp" or item.get("db") != scope):
            raise RuntimeError(
                f"{mcp['id']} is not confirmed approved, enabled and callable in {scope}; "
                "review/enable it explicitly, then use --sync-only. Direct entries preserved."
            )



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
    if args.dry_run:
        for mcp in MCP_SERVERS:
            definition, _ = apply_tool_filters(mcp)
            definition, _ = apply_cap_policy(mcp, definition)
            print(json.dumps({"id": mcp["id"], "scope": args.scope, "definition": definition}))
        return True
    print("Tachi MCP stdio has been retired; this legacy bootstrap script cannot register Hub entries or sync client configs. No subprocess was started or config written. Migrate supported operations with the owner CLI (tachi hub / tachi mcp), and configure HTTP clients with setup_agent_mcp.py. The owner CLI does not reproduce every batch scope/policy option; migrate those manually using supported capabilities. Use --dry-run to inspect the plan.", file=sys.stderr)
    return False


if __name__ == "__main__":
    sys.exit(0 if main() else 1)
