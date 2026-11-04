#!/usr/bin/env python3
"""
Configure a resident Tachi Streamable HTTP server for the calling workspace.

Dry-run by default. Use --apply to persist changes.
"""

import argparse
import copy
import datetime as dt
import json
import shutil
import sys
from pathlib import Path
from urllib.parse import urlsplit


AGENT_TARGETS = {
    "antigravity": {
        "path": Path("~/.gemini/antigravity/mcp_config.json"),
        "server_key": "tachi",
    },
    "claude-code": {
        "path": Path("~/.claude.json"),
        "project_path": Path(".mcp.json"),
        "server_key": "tachi",
    },
    "claude-desktop": {
        "path": Path("~/Library/Application Support/Claude/claude_desktop_config.json"),
        "server_key": "tachi",
    },
    "cursor": {
        "path": Path("~/.cursor/mcp.json"),
        "project_path": Path(".cursor/mcp.json"),
        "server_key": "tachi",
    },
    "gemini-cli": {
        "path": Path("~/.gemini/settings.json"),
        "project_path": Path(".gemini/settings.json"),
        "server_key": "tachi",
    },
    "trae": {
        "path": Path("~/.trae/mcp.json"),
        "server_key": "tachi",
    },
}

# These hosts document native Streamable HTTP plus custom headers. The older
# config formats above remain available through explicit stdio compatibility.
HTTP_AGENTS = {"claude-code", "cursor", "gemini-cli"}

LEGACY_SERVER_KEYS = {
    "antigravity": ["memory"],
    "claude-code": ["sigil-memory"],
    "gemini-cli": ["memory"],
}


def parse_args(argv=None):
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
        "--transport",
        choices=["http", "stdio"],
        default="http",
        help="Tachi MCP transport (default: native Streamable HTTP)",
    )
    parser.add_argument(
        "--url",
        help="Fixed resident daemon endpoint, e.g. http://127.0.0.1:6919/mcp",
    )
    parser.add_argument(
        "--config-scope",
        choices=["project", "user"],
        default="project",
        help="Host config location (default: project, bound to this workspace)",
    )
    parser.add_argument(
        "--workspace-root",
        help="Workspace directory on the daemon host (project scope defaults to current directory)",
    )
    parser.add_argument(
        "--project",
        help="HTTP registered project identity instead of a workspace-root binding",
    )
    parser.add_argument(
        "--command",
        help="Stdio compatibility command (default: tachi); requires --transport stdio",
    )
    parser.add_argument(
        "--arg",
        action="append",
        default=[],
        help="Stdio command arg (repeatable; defaults to serve)",
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
    return parser.parse_args(argv)


def expand(path_obj):
    return path_obj.expanduser().resolve()


def read_json(path):
    try:
        return json.loads(path.read_text())
    except Exception as exc:
        raise RuntimeError(f"failed to read {path}: {exc}") from exc


def validate_http_url(raw):
    if not raw:
        raise ValueError(
            "HTTP requires --url with a fixed resident daemon endpoint. Start "
            "`tachi --daemon --port 6919 serve`, then use --url http://127.0.0.1:6919/mcp"
        )
    try:
        parsed = urlsplit(raw)
        valid = (
            not any(character.isspace() for character in raw)
            and parsed.scheme == "http"
            and parsed.hostname in ("127.0.0.1", "localhost")
            and parsed.port is not None
            and parsed.port > 0
            and parsed.path == "/mcp"
            and parsed.username is None
            and parsed.password is None
            and not parsed.query
            and not parsed.fragment
        )
    except ValueError:
        valid = False
    if not valid:
        raise ValueError("--url must be a loopback HTTP /mcp endpoint with an explicit nonzero fixed port")
    return raw


def workspace_for_args(args):
    if args.workspace_root:
        workspace = Path(args.workspace_root).expanduser()
        if not workspace.is_absolute():
            raise ValueError("--workspace-root must be an absolute directory")
    elif args.config_scope == "project":
        workspace = Path.cwd()
    else:
        return None
    workspace = workspace.resolve(strict=True)
    if not workspace.is_dir():
        raise ValueError("--workspace-root must be a directory")
    return workspace


def build_entry(agent_name, command=None, args=None, env_overrides=None, *,
                transport="http", url=None, workspace=None, project=None):
    if transport == "http":
        if agent_name not in HTTP_AGENTS:
            raise ValueError(f"{agent_name} has no verified native HTTP config; select --transport stdio --config-scope user explicitly")
        if command or args or env_overrides:
            raise ValueError("Command, args and daemon environment options require --transport stdio")
        headers = {"x-tachi-profile": "standard", "x-tachi-client": agent_name}
        if project:
            headers["x-tachi-project"] = project
        elif workspace is not None:
            headers["x-tachi-workspace-root"] = str(workspace)
        entry = {"headers": headers}
        if agent_name == "gemini-cli":
            entry["httpUrl"] = validate_http_url(url)
        else:
            entry["url"] = validate_http_url(url)
            if agent_name == "claude-code":
                entry["type"] = "http"
        return entry

    if project:
        raise ValueError("--project requires --transport http; stdio compatibility binds from its workspace")
    if workspace is not None and not any((root / ".git").exists() for root in (workspace, *workspace.parents)):
        raise ValueError("Stdio automatic workspace binding requires Git; use native HTTP for an ordinary folder")
    env = {"TACHI_PROFILE": "standard"}
    if workspace is not None:
        env["TACHI_WORKSPACE_ROOT"] = str(workspace)
    env.update(env_overrides or {})
    entry = {"command": command or "tachi", "args": args or ["serve"], "env": env}
    if agent_name == "antigravity":
        entry.setdefault("disabled", False)
    return entry


def merge_entry(existing, desired):
    merged = copy.deepcopy(existing) if isinstance(existing, dict) else {}
    previous_env = merged.get("env", {}) if "command" in desired else {}
    # A transport switch replaces the entire transport descriptor. In particular
    # no old process, stale project header or internal proxy token survives HTTP.
    for key in ("command", "args", "env", "cwd", "type", "url", "httpUrl", "headers", "headersHelper"):
        merged.pop(key, None)
    merged.update(copy.deepcopy(desired))
    if "env" in desired:
        env_map = copy.deepcopy(previous_env) if isinstance(previous_env, dict) else {}
        env_map.update(desired["env"])
        merged["env"] = env_map
    return merged


def backup_file(path):
    stamp = dt.datetime.now().strftime("%Y%m%d_%H%M%S_%f")
    backup = path.with_suffix(path.suffix + f".bak.{stamp}")
    shutil.copy2(path, backup)
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


def main(argv=None):
    args = parse_args(argv)
    selected_agents = select_agents(args.agent)
    remove_servers = dedupe_items(args.remove_server)

    env_overrides = {}
    if args.memory_db_path:
        env_overrides["MEMORY_DB_PATH"] = args.memory_db_path
    if args.enable_pipeline is not None:
        env_overrides["ENABLE_PIPELINE"] = args.enable_pipeline

    try:
        workspace = workspace_for_args(args)
        if args.project is not None and (not args.project.strip() or any(c in args.project for c in "\r\n")):
            raise ValueError("--project must be a nonempty project identity without line breaks")
        if args.transport == "http":
            validate_http_url(args.url)
            if args.command or args.arg or env_overrides:
                raise ValueError("--command, --arg, --memory-db-path and --enable-pipeline require --transport stdio; HTTP clients use the resident daemon's configuration")
        elif args.url:
            raise ValueError("--url is only valid with --transport http")
        elif args.project:
            raise ValueError("--project requires --transport http; stdio compatibility binds from its workspace")
    except (OSError, ValueError) as exc:
        print(f"[fail] {exc}")
        return 1

    changed = 0
    scanned = 0
    missing = 0
    failures = 0
    unsupported = 0
    select_all = not args.agent or "all" in args.agent

    for agent_name in selected_agents:
        target = AGENT_TARGETS[agent_name]
        scanned += 1
        if args.transport == "http" and agent_name not in HTTP_AGENTS:
            unsupported += 1
            failures += int(not select_all)
            print(f"[skip] {agent_name}: native HTTP config is not verified; select --transport stdio --config-scope user explicitly")
            continue
        if args.config_scope == "project":
            relative = target.get("project_path")
            if relative is None:
                unsupported += 1
                failures += int(not select_all)
                print(f"[skip] {agent_name}: no project config target; select --config-scope user explicitly")
                continue
            path = workspace / relative
        else:
            path = expand(target["path"])
        if not path.exists() and args.config_scope == "user":
            missing += 1
            print(f"[skip] {agent_name}: config not found at {path}")
            continue

        try:
            cfg = read_json(path) if path.exists() else {}
            if not isinstance(cfg, dict):
                raise RuntimeError("top-level JSON must be an object")

            if "mcpServers" not in cfg:
                cfg["mcpServers"] = {}
            elif not isinstance(cfg["mcpServers"], dict):
                raise RuntimeError("mcpServers must be an object; refusing to replace malformed configuration")

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
                agent_name, args.command, args.arg, env_overrides,
                transport=args.transport, url=args.url, workspace=workspace,
                project=args.project,
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
                backup = backup_file(path) if path.exists() else None
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(json.dumps(cfg, indent=2, ensure_ascii=False) + "\n")
                detail_parts = [f"mcpServers.{server_key}"]
                if migrated_from:
                    detail_parts.append(f"migrated from '{migrated_from}'")
                if removed_keys:
                    detail_parts.append(f"removed: {', '.join(removed_keys)}")
                print(
                    f"[edit] {agent_name}: updated {path} ({'; '.join(detail_parts)}; backup: {backup.name if backup else 'new file'})"
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
    print(f"Unsupported configs skipped: {unsupported}")
    print(f"Configs changed: {changed}")
    print(f"Failures: {failures}")

    if not args.apply and changed > 0:
        print("\nRun with --apply to write these changes.")

    return 1 if failures > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
