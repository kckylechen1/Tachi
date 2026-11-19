#!/usr/bin/env python3
"""Owner-Mac native Staff acceptance with real accounts. Never runs in CI.

Starts a candidate daemon on a temporary TACHI_HOME and fixture repository,
then for each native profile runs one real supervised turn in which the worker
uses a shell and the injected tachi MCP, and the owner finishes the run. Host
credentials are only read (snapshotted into the run sandbox); the live Tachi
store is never touched.

    python3 scripts/native_acceptance.py --tachi <candidate> [--profiles codex_sol_review,opencode_sol_review]
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import dogfood_e2e as d  # noqa: E402

TASK = (
    "Acceptance probe. 1) In your shell run `git rev-parse --is-inside-work-tree` in the workspace. "
    "2) Call the tachi MCP tool `tachi_memory` with action `briefing`. Do not modify files. "
    "Reply with one line exactly: ACCEPTANCE_OK inside_work_tree=<shell output> mcp=<ok|failed>"
)


def until(predicate, seconds: float) -> bool:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(1)
    return bool(predicate())


def which(name: str) -> str | None:
    return shutil.which(name)


def version(binary: str | None) -> str:
    if not binary:
        return "missing"
    output = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=30)
    return (output.stdout or output.stderr).strip().splitlines()[0] if output.returncode == 0 else "unavailable"


def host_credential_fingerprint(backend: str) -> str:
    """Local change evidence: refresh metadata, or a digest of legacy auth.json."""
    if backend == "codex":
        path = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex")) / "auth.json"
        return json.loads(path.read_text()).get("last_refresh", "") if path.is_file() else "absent"
    db = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "opencode/opencode.db"
    if db.is_file():
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as connection:
            has_credentials = connection.execute(
                "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'credential')"
            ).fetchone()[0]
            if has_credentials:
                rows = connection.execute(
                    "SELECT integration_id, time_updated FROM credential "
                    "WHERE active = 1 OR active IS NULL ORDER BY 1"
                ).fetchall()
                return hashlib.sha256(json.dumps(rows).encode()).hexdigest()[:16]
    legacy = db.with_name("auth.json")
    return hashlib.sha256(legacy.read_bytes()).hexdigest()[:16] if legacy.is_file() else "absent"


def worker_used(events: list[dict], kind: str) -> bool:
    """Protocol evidence of a successful shell command or the injected MCP briefing."""
    acp_calls: dict[tuple[str, str], dict] = {}
    for row in events:
        message = row.get("message", {}) if row.get("direction") == "agent" else {}
        params = message.get("params") or {}
        item = params.get("item") or {}
        if message.get("method") == "item/completed" and item.get("status") == "completed":
            if kind == "shell" and item.get("type") == "commandExecution" and item.get("exitCode") == 0:
                return True
            if kind == "mcp" and item.get("type") == "mcpToolCall" and not item.get("error") \
                    and item.get("server") == "tachi" and item.get("tool") == "tachi_memory" \
                    and (item.get("arguments") or {}).get("action") == "briefing":
                return True

        update = params.get("update") or {}
        if message.get("method") != "session/update" \
                or update.get("sessionUpdate") not in ("tool_call", "tool_call_update"):
            continue
        key = (params.get("sessionId"), update.get("toolCallId"))
        if not all(key):
            continue
        if update["sessionUpdate"] == "tool_call":
            # OpenCode v1 puts the tool name in the initial title; later titles are display text.
            acp_calls[key] = {**update, "name": update.get("name") or update.get("title")}
        elif key in acp_calls:
            acp_calls[key].update(update)
        else:
            continue
        call = acp_calls[key]
        if update.get("status") != "completed":
            continue
        if kind == "shell" and call.get("kind") == "execute":
            return True
        if kind == "mcp" and call.get("name") == "tachi_tachi_memory" \
                and (call.get("rawInput") or {}).get("action") == "briefing":
            return True
    return False


def accept(ctx: d.Context, daemon: d.Daemon, owner: d.McpSession, profile: str) -> None:
    backend = profile.split("_", 1)[0]
    before = host_credential_fingerprint(backend)
    ok, payload, raw = owner.call("tachi_staff", action="start", profile=profile, task=TASK,
                                  staffing_reason="explicit_user_request", format="json")
    dispatch = d.find_key(payload, "dispatch_id") if ok else None
    ctx.require(f"{profile} starts a native session", bool(dispatch), raw)
    status: dict = {}
    deadline = time.monotonic() + 900
    while time.monotonic() < deadline:
        _, status, _ = owner.call("tachi_staff", action="status", dispatch_id=dispatch, format="json")
        phase = d.find_key(status, "phase")
        if phase in ("awaiting_instruction", "failed", "finished", "cancelled"):
            break
        time.sleep(2)
    session = d.find_key(status, "native_session") or {}
    ctx.require(f"{profile} real turn reaches the supervised boundary", session.get("phase") == "awaiting_instruction",
                {k: session.get(k) for k in ("phase", "failure_class", "failure")})
    requested = session.get("requested") or {}
    observed = session.get("observed") or {}
    ctx.check(f"{profile} observed model and effort equal the pinned profile",
              observed.get("model") == requested.get("model")
              and (requested.get("effort") is None or observed.get("effort") == requested.get("effort")),
              {"requested": requested, "observed": observed})
    run_dir = daemon.app_home / "runs" / dispatch
    result = (run_dir / "result.md").read_text() if (run_dir / "result.md").is_file() else ""
    events = [json.loads(line) for line in (run_dir / "events.jsonl").read_text().splitlines()] \
        if (run_dir / "events.jsonl").is_file() else []
    ctx.check(f"{profile} worker reports the acceptance line",
              "ACCEPTANCE_OK" in result and "inside_work_tree=true" in result and "mcp=ok" in result, result[-300:])
    ctx.check(f"{profile} worker ran a shell command inside the sandbox", worker_used(events, "shell"), len(events))
    ctx.check(f"{profile} worker called the injected tachi MCP", worker_used(events, "mcp"), len(events))
    ok, final, raw = owner.call("tachi_staff", action="finish", dispatch_id=dispatch,
                                expected_status_revision=d.find_key(status, "status_revision"), format="json")
    settled = until(lambda: d.find_key(owner.call("tachi_staff", action="status", dispatch_id=dispatch,
                                                    format="json")[1], "state") == "TASK_STATE_COMPLETED", 60) if ok else False
    ctx.check(f"{profile} owner finish settles the run", settled, raw if not ok else dispatch)
    residue = [p.name for p in (run_dir / "runtime").rglob("*")
               if p.name in ("auth.json", "opencode.db", "opencode.db-wal", "opencode.db-shm")]
    ctx.check(f"{profile} credential snapshot is removed at terminal", not residue, residue)
    ctx.check(f"{profile} host credential fingerprint is unchanged",
              host_credential_fingerprint(backend) == before, {"before": before})


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tachi", type=Path, required=True, help="candidate tachi binary")
    parser.add_argument("--profiles", default="codex_sol_review,opencode_sol_review")
    parser.add_argument("--keep", action="store_true", help="keep the temporary home for inspection")
    arguments = parser.parse_args()
    tachi = arguments.tachi.expanduser().resolve()
    codex, opencode, node, git = which("codex"), which("opencode"), which("node"), which("git")
    print(f"## macOS {platform.mac_ver()[0]} ({platform.machine()})")
    print(f"## {version(str(tachi))}")
    print(f"## codex {version(codex)} | opencode {version(opencode)}")

    run = d.Run()
    root = Path(tempfile.mkdtemp(prefix="tachi-native-acceptance-"))
    daemon = d.Daemon(tachi, root, d.free_port())
    search = [str(Path(p).parent) for p in (codex, opencode, node, git) if p]
    daemon.env.update({
        "PATH": ":".join(dict.fromkeys(search + ["/usr/bin", "/bin", "/usr/sbin", "/sbin"])),
        "CODEX_HOME": os.environ.get("CODEX_HOME", str(Path.home() / ".codex")),
        "XDG_DATA_HOME": os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local/share")),
        "TACHI_HOST_PROFILE": "development", "DISPATCH_V2_ENABLED": "true", "DISPATCH_V2_PLAN_REVIEW": "false",
        "TACHI_WORKTREES_ROOT": str(root / "worktrees"), "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
    })
    for key in ("HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY", "SSL_CERT_FILE", "SSL_CERT_DIR"):
        if key in os.environ:
            daemon.env[key] = os.environ[key]
    try:
        repo = d.make_fixture_repo(root)
        daemon.start()
        ctx = d.Context(daemon, repo, run)
        owner = d.McpSession(daemon.url, {"X-Tachi-Workspace-Root": str(repo),
                                          "X-Tachi-Agent-Identity": f"native-acceptance-{ctx.tag}"})
        for profile in [p.strip() for p in arguments.profiles.split(",") if p.strip()]:
            ctx.current = profile
            try:
                accept(ctx, daemon, owner, profile)
            except d.ScenarioAborted:
                pass
    finally:
        daemon.stop()
        if arguments.keep:
            print(f"## kept {root}")
        else:
            shutil.rmtree(root, ignore_errors=True)
    d.print_run(run)
    return 1 if any(c.status == "FAIL" for c in run.checks) or not run.checks else 0


if __name__ == "__main__":
    sys.exit(main())
