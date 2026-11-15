#!/usr/bin/env python3
"""
Plan legacy shared MCP registration; write mode is retired.

Hub registration and standard client setup are separate from direct MCP routing.
Existing direct MCP client entries are preserved.
"""

import json
import sys
import os
import copy
import argparse

if __package__:
    from . import setup_agent_mcp as agent_setup
else:
    import setup_agent_mcp as agent_setup

# Legacy Hub definitions retained for dry-run planning.
MCP_SERVERS = [
    {
        "id": "mcp:context7",
        "name": "context7",
        "description": "Context7 — resolve library IDs and query up-to-date documentation for any programming library.",
        "definition": {
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "@upstash/context7-mcp@latest"],
        },
        "policy": {"visibility": "listed", "scope": "core-shared"},
    },
    {
        "id": "mcp:exa",
        "name": "exa",
        "description": "Exa — fast intelligent web search and web crawling for AI agents.",
        "definition": {
            "transport": "sse",
            "url": "https://mcp.exa.ai/mcp",
        },
        "policy": {"visibility": "listed", "scope": "core-shared"},
    },
    {
        "id": "mcp:longbridge",
        "name": "longbridge",
        "description": "Longbridge — Hong Kong / US stock market data, quotes, positions, and order management.",
        "definition": {
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "mcp-longbridge"],
            "env": {
                "LONGPORT_APP_KEY": "${vault:LONGPORT_APP_KEY}",
                "LONGPORT_APP_SECRET": "${vault:LONGPORT_APP_SECRET}",
                "LONGPORT_ACCESS_TOKEN": "${vault:LONGPORT_ACCESS_TOKEN}",
                "LONGPORT_LANGUAGE": "${LONGPORT_LANGUAGE}",
            },
            "tool_timeout_ms": 30000,
            "max_concurrency": 1,
        },
        "policy": {
            "visibility": "listed",
            "scope": "agent-local",
            "owner_agent": "stock",
        },
    },
]

AGENT_CHOICES = sorted([*agent_setup.AGENT_TARGETS, "all"])


def parse_args():
    parser = argparse.ArgumentParser(
        description="Plan shared MCP definitions and visibility policy; write mode is retired"
    )
    parser.add_argument(
        "--scope",
        choices=["global", "project"],
        default="global",
        help="Hub scope shown in the registration plan",
    )
    parser.add_argument(
        "--disable-hidden",
        action="store_true",
        help="Retired compatibility option; no registrations are changed",
    )
    parser.add_argument(
        "--sync-agent-config",
        dest="sync_agent_config",
        action="store_true",
        default=True,
        help="Retired compatibility option; client config synchronization is refused",
    )
    parser.add_argument(
        "--no-sync-agent-config",
        dest="sync_agent_config",
        action="store_false",
        help="Retired compatibility option; client configs are never changed",
    )
    parser.add_argument(
        "--agent",
        action="append",
        choices=AGENT_CHOICES,
        help="Retired config-sync target option (repeatable); accepted without syncing",
    )
    parser.add_argument(
        "--agent-transport",
        choices=["http", "stdio"],
        default="http",
        help="Retired config-sync transport option; configure HTTP clients with setup_agent_mcp.py",
    )
    parser.add_argument(
        "--daemon-url",
        help="Retired config-sync endpoint option; no connection is made",
    )
    parser.add_argument(
        "--agent-config-scope",
        choices=["project", "user"],
        default="project",
        help="Retired config-sync location option; client configs are never changed",
    )
    parser.add_argument(
        "--workspace-root",
        help="Retired config-sync workspace option; no workspace is changed",
    )
    parser.add_argument(
        "--sync-only",
        action="store_true",
        help="Retired compatibility option; Hub verification and client sync are refused",
    )
    parser.add_argument("--dry-run", action="store_true", help="Print the registration plan without starting Tachi or writing config")
    args = parser.parse_args()
    if args.sync_only and not args.sync_agent_config:
        parser.error("--sync-only requires agent config sync")
    return args


def parse_csv_list(raw_value):
    return [item.strip() for item in raw_value.split(",") if item.strip()]


def apply_tool_filters(mcp):
    definition = copy.deepcopy(mcp["definition"])
    env_key = mcp["name"].upper().replace("-", "_")

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
    print("Tachi MCP stdio has been retired, and Hub MCP proxy registration is retired; this legacy bootstrap script cannot register Hub entries or sync client configs. No subprocess was started or config written. Configure third-party MCP servers directly in the host, manage retained skill/plugin entries with the owner CLI (tachi hub), and configure Tachi HTTP clients with setup_agent_mcp.py. The owner CLI does not reproduce every batch scope/policy option; migrate supported entries manually. Use --dry-run to inspect the legacy plan.", file=sys.stderr)
    return False


if __name__ == "__main__":
    sys.exit(0 if main() else 1)
