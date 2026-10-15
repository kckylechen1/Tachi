#!/usr/bin/env python3
"""
Register shared MCP servers into Tachi Hub.

Hub registration and standard client setup are separate from direct MCP routing.
Existing direct MCP client entries are preserved.
"""

import subprocess
import json
import sys
import os
import copy
import select
import time
import argparse
import tempfile

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
    parser.add_argument(
        "--sync-only",
        action="store_true",
        help="Verify approved, enabled Hub entries and sync without re-registering them",
    )
    args = parser.parse_args()
    if args.sync_only and not args.sync_agent_config:
        parser.error("--sync-only requires agent config sync")
    return args


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

    deadline = time.monotonic() + REQUEST_TIMEOUT_SECS
    buffered = getattr(proc, "_mcp_buffer", b"")
    while True:
        if b"\n" in buffered:
            line, buffered = buffered.split(b"\n", 1)
            proc._mcp_buffer = buffered
            if not line.strip():
                continue
            response = json.loads(line)
            if not isinstance(response, dict):
                raise RuntimeError("MCP reply must be an object")
            if "id" not in response and "method" in response:
                continue  # Notifications are not replies to this request.
            if response.get("id") != request["id"]:
                raise RuntimeError("MCP reply id does not match request")
            if "error" in response:
                raise RuntimeError(f"MCP request failed: {response['error']}")
            return response
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(
                f"Timed out waiting for MCP reply after {REQUEST_TIMEOUT_SECS:.1f}s"
            )
        ready, _, _ = select.select([proc.stdout], [], [], remaining)
        if not ready:
            continue
        chunk = os.read(proc.stdout.fileno(), 65536)
        if not chunk:
            raise RuntimeError("tachi closed stdio before replying")
        buffered += chunk


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


def require_tools(proc, names):
    resp = send_request(proc, {
        "jsonrpc": "2.0", "id": next_id(), "method": "tools/list", "params": {}
    })
    try:
        tools = resp["result"]["tools"]
        visible = {item["name"] for item in tools}
    except (KeyError, TypeError) as exc:
        raise RuntimeError("Invalid tools/list response") from exc
    missing = set(names) - visible
    if missing:
        raise RuntimeError(
            "Required operator tools unavailable: " + ", ".join(sorted(missing))
            + ". Select an explicitly authorized TACHI_PROFILE (admin for registration; "
            "ops or admin for --sync-only). No config was changed."
        )


def confirm_hub_mcp_servers(proc, scope):
    items = tool_result(call_tool(
        proc, "hub_discover", {"cap_type": "mcp", "enabled_only": False}
    ))
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


def sync_agent_configs(args):
    script_path = os.path.join(os.path.dirname(__file__), "setup_agent_mcp.py")
    cmd = [sys.executable, script_path, "--apply", "--command", SERVER_BIN]

    for agent_name in resolve_selected_agents(args.agent):
        cmd.extend(["--agent", agent_name])

    print("\nSyncing local agent MCP configs...")
    print("Preserving direct MCP entries: standard clients do not expose hub_call.")
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
    # An unset profile selects the ordinary Lead surface, never maintenance.
    if not os.environ.get("TACHI_PROFILE", "").strip():
        print("Explicit operator TACHI_PROFILE required (admin for registration; "
              "ops or admin for --sync-only). No config was changed.", file=sys.stderr)
        return False

    if args.scope == "project" and args.sync_agent_config:
        print("Project Hub evidence cannot authorize migration of global client configs. "
              "Use --no-sync-agent-config for project registration.", file=sys.stderr)
        return False

    proc = None
    # A file avoids deadlocking on an unread stderr pipe during slow discovery.
    with tempfile.TemporaryFile() as server_log:
        try:
            proc = subprocess.Popen(
                [SERVER_BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=server_log, bufsize=0,
            )
            resp = send_request(proc, {
                "jsonrpc": "2.0", "id": next_id(), "method": "initialize",
                "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                           "clientInfo": {"name": "hub-mcp-registrar", "version": "1.0.0"}},
            })
            if not isinstance(resp.get("result"), dict):
                raise RuntimeError("Invalid MCP initialization reply")
            proc.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            proc.stdin.flush()
            required = ["hub_discover"] if args.sync_agent_config else []
            if not args.sync_only:
                required.append("hub_register")
            if args.disable_hidden:
                required.append("hub_set_enabled")
            require_tools(proc, required)

            if not args.sync_only:
                print(f"Registering {len(MCP_SERVERS)} MCP servers into Tachi Hub...")
                for mcp in MCP_SERVERS:
                    definition, _ = apply_tool_filters(mcp)
                    definition, _ = apply_cap_policy(mcp, definition)
                    data = tool_result(call_tool(proc, "hub_register", {
                        "id": mcp["id"], "cap_type": "mcp", "name": mcp["name"],
                        "description": mcp["description"], "definition": json.dumps(definition),
                        "scope": args.scope, "version": 1,
                    }))
                    if (not isinstance(data, dict) or data.get("id") != mcp["id"]
                            or data.get("db") != args.scope or data.get("discovery_error")):
                        raise RuntimeError(f"Registration not confirmed for {mcp['id']} in {args.scope}")
                    print(f"  Registered {mcp['id']} (review/enablement is separate from registration)")
                    if args.disable_hidden and (definition.get("policy") or {}).get("visibility") == "hidden":
                        disabled = tool_result(call_tool(proc, "hub_set_enabled", {
                            "id": mcp["id"], "enabled": False,
                        }))
                        if not isinstance(disabled, dict) or disabled.get("enabled") is not False:
                            raise RuntimeError(f"Disable not confirmed for {mcp['id']}")

            if args.sync_agent_config:
                # Hub readiness is necessary evidence, but does not prove that
                # a standard client can route these MCPs. Never remove direct keys.
                confirm_hub_mcp_servers(proc, args.scope)
                sync_agent_configs(args)
            return True
        except (OSError, RuntimeError, TimeoutError, ValueError, TypeError, KeyError) as exc:
            print(f"Registration/config sync stopped: {exc}", file=sys.stderr)
            return False
        finally:
            if proc is not None:
                if proc.poll() is None:
                    proc.terminate()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()
                proc.stdin.close()
                proc.stdout.close()


if __name__ == "__main__":
    sys.exit(0 if main() else 1)
