#!/usr/bin/env python3
"""
Batch-register Claude Code skills into Tachi Hub with visibility policy.

Profiles:
- balanced (default): core shared skills are listed, others are discoverable.
- minimal: only core shared skills are listed, others are hidden.
- all: all skills are listed.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

REQUEST_ID = 0

CORE_SHARED_SKILLS = {
    "skill:pdf",
    "skill:docx",
    "skill:xlsx",
    "skill:pptx",
    "skill:data-analysis",
    "skill:doc-coauthoring",
}


def next_id():
    global REQUEST_ID
    REQUEST_ID += 1
    return REQUEST_ID


def parse_args():
    parser = argparse.ArgumentParser(
        description="Register Claude skills into Hub with local/shared visibility policy"
    )
    parser.add_argument(
        "--skills-dir",
        default=os.path.expanduser("~/.claude/skills"),
        help="Directory containing skill folders with SKILL.md",
    )
    parser.add_argument(
        "--server-bin",
        default=os.environ.get("TACHI_SERVER_BIN", "tachi"),
        help="Tachi server binary path (default: tachi)",
    )
    parser.add_argument(
        "--profile",
        choices=["balanced", "minimal", "all"],
        default=os.environ.get("TACHI_SKILL_PROFILE", "balanced"),
        help="Visibility profile",
    )
    parser.add_argument(
        "--scope",
        choices=["global", "project"],
        default="global",
        help="Hub scope to register into",
    )
    parser.add_argument(
        "--listed-skill",
        action="append",
        default=[],
        help="Skill id/name to force listed (repeatable)",
    )
    parser.add_argument(
        "--discoverable-skill",
        action="append",
        default=[],
        help="Skill id/name to force discoverable (repeatable)",
    )
    parser.add_argument(
        "--hidden-skill",
        action="append",
        default=[],
        help="Skill id/name to force hidden (repeatable)",
    )
    parser.add_argument(
        "--agent-local-skill",
        action="append",
        default=[],
        help="Skill id/name to mark as agent-local scope (repeatable)",
    )
    parser.add_argument(
        "--owner-agent",
        help="Owner agent id for agent-local skills (stored in policy.owner_agent)",
    )
    parser.add_argument(
        "--disable-hidden",
        action="store_true",
        help="Set enabled=false for hidden skills after registration",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print plan only; do not call hub_register",
    )
    return parser.parse_args()


def normalize_skill_id(raw):
    value = (raw or "").strip()
    if not value:
        return ""
    if value.startswith("skill:"):
        return value
    return f"skill:{value}"


def parse_frontmatter(content):
    """Extract YAML frontmatter from SKILL.md."""
    m = re.match(r"^---\s*\n(.*?)\n---\s*\n", content, re.DOTALL)
    if not m:
        return {}, content

    fm = {}
    for line in m.group(1).split("\n"):
        if ":" in line and not line.startswith(" "):
            key, val = line.split(":", 1)
            fm[key.strip()] = val.strip()
        elif line.startswith(" ") and "description" in fm:
            fm["description"] += " " + line.strip()

    body = content[m.end() :]
    return fm, body


def detect_server_bin(raw):
    path = Path(raw).expanduser()
    if path.is_file():
        return str(path)
    which = shutil.which(raw)
    if which:
        return which

    fallback = Path(__file__).resolve().parents[1] / "target/release/tachi-server"
    if fallback.is_file():
        return str(fallback)

    raise RuntimeError(
        f"Cannot find server binary '{raw}'. Set --server-bin or TACHI_SERVER_BIN."
    )


def send_request(proc, request):
    if proc.stdin is None or proc.stdout is None:
        raise RuntimeError("server stdio not available")
    proc.stdin.write((json.dumps(request) + "\n").encode())
    proc.stdin.flush()
    line = proc.stdout.readline().decode().strip()
    if not line:
        raise RuntimeError("empty response from server")
    return json.loads(line)


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


def send_notification(proc, method, params=None):
    if proc.stdin is None:
        raise RuntimeError("server stdin is not available")
    payload = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        payload["params"] = params
    proc.stdin.write((json.dumps(payload) + "\n").encode())
    proc.stdin.flush()


def discover_skills(skills_dir):
    skills = []
    for entry in sorted(os.listdir(skills_dir)):
        if entry.startswith("."):
            continue
        skill_path = os.path.join(skills_dir, entry, "SKILL.md")
        if os.path.isfile(skill_path):
            skills.append((entry, skill_path))
    return skills


def plan_visibility(all_skill_ids, args):
    listed = set()
    discoverable = set()
    hidden = set()

    if args.profile == "all":
        listed = set(all_skill_ids)
    elif args.profile == "minimal":
        listed = set(sid for sid in all_skill_ids if sid in CORE_SHARED_SKILLS)
    else:  # balanced
        listed = set(sid for sid in all_skill_ids if sid in CORE_SHARED_SKILLS)
        discoverable = set(all_skill_ids) - listed

    force_listed = {
        normalize_skill_id(v) for v in args.listed_skill if normalize_skill_id(v)
    }
    force_discoverable = {
        normalize_skill_id(v) for v in args.discoverable_skill if normalize_skill_id(v)
    }
    force_hidden = {
        normalize_skill_id(v) for v in args.hidden_skill if normalize_skill_id(v)
    }

    listed |= force_listed
    discoverable |= force_discoverable

    # Explicit overrides take precedence: demote/promote as requested
    listed -= force_discoverable
    listed -= force_hidden
    discoverable -= force_listed
    discoverable -= force_hidden

    discoverable -= listed

    accounted = listed | discoverable | hidden
    for sid in all_skill_ids:
        if sid not in accounted and sid not in force_hidden:
            hidden.add(sid)
    hidden |= force_hidden

    policy = {}
    agent_local = {
        normalize_skill_id(v) for v in args.agent_local_skill if normalize_skill_id(v)
    }
    for sid in all_skill_ids:
        if sid in listed:
            visibility = "listed"
        elif sid in discoverable:
            visibility = "discoverable"
        else:
            visibility = "hidden"

        if sid in CORE_SHARED_SKILLS:
            scope = "core-shared"
        elif sid in agent_local:
            scope = "agent-local"
        else:
            scope = "pack-shared"

        item = {"visibility": visibility, "scope": scope}
        if scope == "agent-local" and args.owner_agent:
            item["owner_agent"] = args.owner_agent
        policy[sid] = item

    return policy


def parse_tool_text_response(resp):
    result = resp.get("result", {})
    content = result.get("content", [])
    if not content:
        return {}
    text = content[0].get("text", "{}")
    return json.loads(text)


def main():
    args = parse_args()
    skills_dir = os.path.expanduser(args.skills_dir)
    if not os.path.isdir(skills_dir):
        print(f"Skills directory not found: {skills_dir}")
        return 1

    server_bin = detect_server_bin(args.server_bin)
    skills = discover_skills(skills_dir)
    if not skills:
        print(f"No SKILL.md files found in {skills_dir}")
        return 0

    all_skill_ids = [normalize_skill_id(dirname) for dirname, _ in skills]
    policy_plan = plan_visibility(all_skill_ids, args)

    by_visibility = {"listed": 0, "discoverable": 0, "hidden": 0}
    for sid in all_skill_ids:
        by_visibility[policy_plan[sid]["visibility"]] += 1

    print(f"Found {len(skills)} skills in {skills_dir}")
    print(
        "Profile: {profile} | listed={listed} discoverable={discoverable} hidden={hidden}".format(
            profile=args.profile,
            listed=by_visibility["listed"],
            discoverable=by_visibility["discoverable"],
            hidden=by_visibility["hidden"],
        )
    )

    if args.dry_run:
        for dirname, _ in skills:
            sid = normalize_skill_id(dirname)
            p = policy_plan[sid]
            print(f"  PLAN  {sid}: visibility={p['visibility']} scope={p['scope']}")
        return 0

    env = os.environ.copy()
    env["ENABLE_PIPELINE"] = "false"

    print(f"Starting server: {server_bin}")
    proc = subprocess.Popen(
        [server_bin],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )

    registered = 0
    errors = 0
    disabled_hidden = 0

    try:
        send_request(
            proc,
            {
                "jsonrpc": "2.0",
                "id": next_id(),
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "skill-loader", "version": "2.0.0"},
                },
            },
        )
        send_notification(proc, "notifications/initialized")

        for dirname, skill_path in skills:
            skill_id = normalize_skill_id(dirname)
            policy = policy_plan[skill_id]

            with open(skill_path, "r") as f:
                content = f.read()

            fm, _ = parse_frontmatter(content)
            name = fm.get("name", dirname)
            description = fm.get("description", "")
            short_desc = description[:200] if description else f"Skill: {name}"

            definition = json.dumps(
                {
                    "format": "claude-code-skill-markdown",
                    "source_path": skill_path,
                    "content": content,
                    "policy": policy,
                }
            )

            resp = call_tool(
                proc,
                "hub_register",
                {
                    "id": skill_id,
                    "cap_type": "skill",
                    "name": name,
                    "description": short_desc,
                    "definition": definition,
                    "scope": args.scope,
                    "version": 1,
                },
            )

            try:
                data = parse_tool_text_response(resp)
                if "error" in data:
                    print(f"  ERR   {skill_id}: {data['error']}")
                    errors += 1
                    continue

                if args.disable_hidden and policy["visibility"] == "hidden":
                    call_tool(
                        proc, "hub_set_enabled", {"id": skill_id, "enabled": False}
                    )
                    disabled_hidden += 1
                    enabled_state = "disabled"
                else:
                    call_tool(
                        proc, "hub_set_enabled", {"id": skill_id, "enabled": True}
                    )
                    enabled_state = "enabled"

                print(
                    "  OK    {sid} ({vis}, {scope}, {state})".format(
                        sid=skill_id,
                        vis=policy["visibility"],
                        scope=policy["scope"],
                        state=enabled_state,
                    )
                )
                registered += 1
            except Exception as exc:
                print(f"  ERR   {skill_id}: {exc}")
                errors += 1

        print("\n" + "=" * 48)
        print(f"Registered: {registered}")
        print(f"Disabled (hidden): {disabled_hidden}")
        print(f"Errors: {errors}")

        resp = call_tool(proc, "hub_stats", {})
        try:
            stats = parse_tool_text_response(resp)
            print(f"Hub total: {stats.get('total_capabilities', '?')}")
            print(f"By type: {stats.get('by_type', '?')}")
        except Exception:
            pass

        return 0 if errors == 0 else 1
    finally:
        proc.terminate()
        proc.wait()
        stderr = (proc.stderr.read().decode() if proc.stderr else "").strip()
        if stderr and errors > 0:
            tail = "\n".join(stderr.splitlines()[-10:])
            print(f"\n--- Server stderr ---\n{tail}")


if __name__ == "__main__":
    sys.exit(main())
