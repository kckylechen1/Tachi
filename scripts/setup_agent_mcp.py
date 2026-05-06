#!/usr/bin/env python3
"""
Detect local agent MCP config files and add/update a Tachi server entry.

Dry-run by default. Use --apply to persist changes.
"""

import argparse
import copy
import datetime as dt
import json
import sys
from pathlib import Path


AGENT_TARGETS = {
    "antigravity": {
        "path": Path("~/.gemini/antigravity/mcp_config.json"),
        "server_key": "tachi",
    },
    "claude-code": {
        "path": Path("~/.claude/.mcp.json"),
        "server_key": "tachi",
    },
    "claude-desktop": {
        "path": Path("~/Library/Application Support/Claude/claude_desktop_config.json"),
        "server_key": "tachi",
    },
    "cursor": {
        "path": Path("~/.cursor/mcp.json"),
        "server_key": "tachi",
    },
    "gemini-cli": {
        "path": Path("~/.gemini/mcp.json"),
        "server_key": "tachi",
    },
    "trae": {
        "path": Path("~/.trae/mcp.json"),
        "server_key": "tachi",
    },
}

LEGACY_SERVER_KEYS = {
    "antigravity": ["memory"],
    "claude-code": ["sigil-memory"],
    "gemini-cli": ["memory"],
}


def parse_args():
    parser = argparse.ArgumentParser(
        description="Auto-configure common agent MCP config files for Tachi"
    )
    parser.add_argument(
        "--agent",
        action="append",
        choices=sorted(list(AGENT_TARGETS.keys()) + ["all"]),
        help="Target agent(s). Repeatable. Defaults to all.",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Write updates in place (default: dry-run only)",
    )
    parser.add_argument(
        "--command",
        default="tachi",
        help="MCP command for Tachi (default: tachi)",
    )
    parser.add_argument(
        "--arg",
        action="append",
        default=[],
        help="Extra MCP command arg (repeatable)",
    )
    parser.add_argument(
        "--memory-db-path",
        help="Optional MEMORY_DB_PATH to pin DB location in agent config",
    )
    parser.add_argument(
        "--enable-pipeline",
        choices=["true", "false"],
        help="Optional ENABLE_PIPELINE value stored in MCP env",
    )
    parser.add_argument(
        "--remove-server",
        action="append",
        default=[],
        help="Remove direct MCP server key from mcpServers (repeatable), e.g. --remove-server exa",
    )
    return parser.parse_args()


def expand(path_obj):
    return path_obj.expanduser().resolve()


def read_json(path):
    try:
        return json.loads(path.read_text())
    except Exception as exc:
        raise RuntimeError(f"failed to read {path}: {exc}") from exc


def build_entry(agent_name, command, args, env_overrides):
    entry = {"command": command}
    if args:
        entry["args"] = args
    
    env = {}
    if agent_name in ("antigravity", "claude-code", "claude-desktop", "cursor", "gemini-cli", "trae"):
        env["TACHI_PROFILE"] = "standard"

    if env_overrides:
        env.update(env_overrides)
        
    if env:
        entry["env"] = env
        
    if agent_name == "antigravity":
        entry.setdefault("disabled", False)
    return entry


def merge_entry(existing, desired):
    merged = copy.deepcopy(existing) if isinstance(existing, dict) else {}
    merged["command"] = desired["command"]
    if "args" in desired:
        merged["args"] = desired["args"]
    if "env" in desired:
        env_map = copy.deepcopy(merged.get("env", {}))
        if not isinstance(env_map, dict):
            env_map = {}
        env_map.update(desired["env"])
        merged["env"] = env_map
    if "disabled" in desired:
        merged["disabled"] = desired["disabled"]
    return merged


def backup_file(path):
    stamp = dt.datetime.now().strftime("%Y%m%d_%H%M%S")
    backup = path.with_suffix(path.suffix + f".bak.{stamp}")
    backup.write_text(path.read_text())
    return backup


def select_agents(agent_args):
    if not agent_args or "all" in agent_args:
        return list(AGENT_TARGETS.keys())
    seen = []
    for name in agent_args:
        if name not in seen:
            seen.append(name)
    return seen


def dedupe_items(items):
    seen = set()
    out = []
    for item in items:
        if item in seen:
            continue
        seen.add(item)
        out.append(item)
    return out


def main():
    args = parse_args()
    selected_agents = select_agents(args.agent)
    remove_servers = dedupe_items(args.remove_server)

    env_overrides = {}
    if args.memory_db_path:
        env_overrides["MEMORY_DB_PATH"] = args.memory_db_path
    if args.enable_pipeline is not None:
        env_overrides["ENABLE_PIPELINE"] = args.enable_pipeline

    changed = 0
    scanned = 0
    missing = 0
    failures = 0

    for agent_name in selected_agents:
        target = AGENT_TARGETS[agent_name]
        path = expand(target["path"])
        scanned += 1

        if not path.exists():
            missing += 1
            print(f"[skip] {agent_name}: config not found at {path}")
            continue

        try:
            cfg = read_json(path)
            if not isinstance(cfg, dict):
                raise RuntimeError("top-level JSON must be an object")

            if "mcpServers" not in cfg or not isinstance(cfg["mcpServers"], dict):
                cfg["mcpServers"] = {}

            server_key = target["server_key"]
            mcp_servers = cfg["mcpServers"]

            # Backward compatibility: migrate legacy key (e.g. antigravity "memory") to "tachi".
            migrated_from = None
            for legacy_key in LEGACY_SERVER_KEYS.get(agent_name, []):
                if server_key in mcp_servers:
                    break
                if legacy_key in mcp_servers:
                    mcp_servers[server_key] = mcp_servers[legacy_key]
                    migrated_from = legacy_key
                    break

            desired_entry = build_entry(
                agent_name, args.command, args.arg, env_overrides
            )
            existing_entry = mcp_servers.get(server_key, {})
            merged_entry = merge_entry(existing_entry, desired_entry)
            entry_changed = existing_entry != merged_entry
            removed_keys = []

            mcp_servers[server_key] = merged_entry

            for legacy_key in LEGACY_SERVER_KEYS.get(agent_name, []):
                if legacy_key != server_key and legacy_key in mcp_servers:
                    del mcp_servers[legacy_key]
                    if legacy_key not in removed_keys:
                        removed_keys.append(legacy_key)

            for key in remove_servers:
                if key == server_key:
                    continue
                if key in mcp_servers:
                    del mcp_servers[key]
                    if key not in removed_keys:
                        removed_keys.append(key)

            if not entry_changed and not removed_keys:
                print(f"[ok]   {agent_name}: already configured ({server_key})")
                continue

            changed += 1

            if args.apply:
                backup = backup_file(path)
                path.write_text(json.dumps(cfg, indent=2, ensure_ascii=False) + "\n")
                detail_parts = [f"mcpServers.{server_key}"]
                if migrated_from:
                    detail_parts.append(f"migrated from '{migrated_from}'")
                if removed_keys:
                    detail_parts.append(f"removed: {', '.join(removed_keys)}")
                print(
                    f"[edit] {agent_name}: updated {path} ({'; '.join(detail_parts)}; backup: {backup.name})"
                )
            else:
                detail_parts = [f"mcpServers.{server_key}"]
                if migrated_from:
                    detail_parts.append(f"migrated from '{migrated_from}'")
                if removed_keys:
                    detail_parts.append(f"removed: {', '.join(removed_keys)}")
                print(
                    f"[plan] {agent_name}: would update {path} ({'; '.join(detail_parts)})"
                )
        except Exception as exc:
            failures += 1
            print(f"[fail] {agent_name}: {exc}")

    mode = "APPLY" if args.apply else "DRY-RUN"
    print("\n--- Summary ---")
    print(f"Mode: {mode}")
    print(f"Agents scanned: {scanned}")
    print(f"Configs missing: {missing}")
    print(f"Configs changed: {changed}")
    print(f"Failures: {failures}")

    if not args.apply and changed > 0:
        print("\nRun with --apply to write these changes.")

    return 1 if failures > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
