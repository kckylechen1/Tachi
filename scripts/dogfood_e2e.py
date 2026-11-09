#!/usr/bin/env python3
"""Local end-to-end dogfood run against a real Tachi binary.

Starts the given binary as an isolated daemon (temporary HOME / TACHI_HOME,
its own port, never the live store), connects over HTTP MCP exactly like a
host connector does, and walks the coordination and memory paths that the
2026-11-08 dogfood round exercised. Prints one row per scenario and exits
non-zero if any scenario fails. Run it on the owner's Mac against a release
candidate before switching the live daemon (AGENTS.md §3, real-Mac rule).

    python3 scripts/dogfood_e2e.py --tachi ./target/release/tachi
    python3 scripts/dogfood_e2e.py --tachi ~/bin/tachi --keep   # keep temp dir

Nothing here reads or writes ~/.tachi; all rows are synthetic and live in
the temporary home.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid
from pathlib import Path

REPO_ORIGIN = "https://github.com/kckylechen1/Tachi.git"
REPO_SHORT_NAME = "Tachi"
COMPACT_BRIEF_BUDGET_BYTES = 16 * 1024


class McpSession:
    """One MCP session over the daemon's streamable HTTP endpoint."""

    def __init__(self, url: str, headers: dict[str, str] | None = None):
        self.url = url
        self.headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            **(headers or {}),
        }
        self.session_id: str | None = None
        self._next = 0
        _, response_headers = self._post(
            {
                "jsonrpc": "2.0",
                "id": self._id(),
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "tachi-dogfood-e2e", "version": "1"},
                },
            }
        )
        self.session_id = response_headers.get("Mcp-Session-Id") or response_headers.get(
            "mcp-session-id"
        )
        self._post({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _id(self) -> int:
        self._next += 1
        return self._next

    def _post(self, body: dict) -> tuple[dict | None, dict]:
        headers = dict(self.headers)
        if self.session_id:
            headers["Mcp-Session-Id"] = self.session_id
        request = urllib.request.Request(
            self.url, data=json.dumps(body).encode(), headers=headers, method="POST"
        )
        with urllib.request.urlopen(request, timeout=120) as response:
            raw = response.read().decode()
            response_headers = dict(response.headers)
        events = [
            line[5:].strip()
            for line in raw.splitlines()
            if line.startswith("data:") and line[5:].strip()
        ]
        if events:
            return json.loads(events[-1]), response_headers
        return (json.loads(raw) if raw.strip() else None), response_headers

    def call(self, tool: str, **arguments) -> tuple[bool, object, str]:
        """Returns (ok, parsed payload, raw text)."""
        message, _ = self._post(
            {
                "jsonrpc": "2.0",
                "id": self._id(),
                "method": "tools/call",
                "params": {"name": tool, "arguments": arguments},
            }
        )
        if message is None or "error" in message:
            return False, (message or {}).get("error"), json.dumps(message)
        result = message["result"]
        text = "".join(part.get("text", "") for part in result.get("content", []))
        try:
            payload = json.loads(text)
        except ValueError:
            payload = text
        return not result.get("isError", False), payload, text


class Report:
    def __init__(self) -> None:
        self.rows: list[tuple[str, str, str]] = []

    def record(self, name: str, ok: bool, evidence: str) -> bool:
        self.rows.append((name, "PASS" if ok else "FAIL", evidence))
        return ok

    def note(self, name: str, evidence: str) -> None:
        self.rows.append((name, "NOTE", evidence))

    def print(self) -> int:
        width = max(len(name) for name, _, _ in self.rows)
        for name, status, evidence in self.rows:
            print(f"{status:4}  {name:<{width}}  {evidence}")
        failed = sum(1 for _, status, _ in self.rows if status == "FAIL")
        passed = sum(1 for _, status, _ in self.rows if status == "PASS")
        print(f"\n{passed} passed, {failed} failed")
        return 1 if failed else 0


def short(value: object, limit: int = 160) -> str:
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
    return text if len(text) <= limit else text[: limit - 1] + "…"


def find_key(payload: object, key: str) -> object:
    if isinstance(payload, dict):
        if key in payload:
            return payload[key]
        for value in payload.values():
            found = find_key(value, key)
            if found is not None:
                return found
    elif isinstance(payload, list):
        for value in payload:
            found = find_key(value, key)
            if found is not None:
                return found
    return None


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def make_fixture_repo(root: Path) -> Path:
    repo = root / "repo"
    repo.mkdir()
    git = ["git", "-C", str(repo)]
    env = {**os.environ, "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}
    subprocess.run([*git, "init", "-q", "-b", "main"], check=True, env=env)
    subprocess.run([*git, "remote", "add", "origin", REPO_ORIGIN], check=True, env=env)
    (repo / "README.md").write_text("dogfood fixture\n")
    subprocess.run([*git, "add", "README.md"], check=True, env=env)
    subprocess.run(
        [*git, "-c", "user.name=dogfood", "-c", "user.email=dogfood@invalid",
         "commit", "-q", "-m", "fixture"],
        check=True,
        env=env,
    )
    (repo / ".tachi").mkdir()
    return repo


def repo_head(repo: Path) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD"], check=True, capture_output=True, text=True
    ).stdout.strip()


class Daemon:
    def __init__(self, tachi: Path, root: Path, port: int):
        self.tachi = tachi
        self.home = root / "home"
        self.app_home = self.home / ".tachi"
        self.app_home.mkdir(parents=True)
        self.port = port
        self.log = root / "daemon.log"
        self.env = {
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(self.home),
            "TACHI_HOME": str(self.app_home),
        }
        self.process: subprocess.Popen | None = None

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}/mcp"

    def cli(self, *args: str, cwd: Path | None = None) -> subprocess.CompletedProcess:
        return subprocess.run(
            [str(self.tachi), *args], env=self.env, cwd=cwd, capture_output=True, text=True
        )

    def start(self) -> None:
        with open(self.log, "w") as log:
            self.process = subprocess.Popen(
                [str(self.tachi), "--daemon", "--port", str(self.port),
                 "--no-project-db", "--allow-schema-migration"],
                env=self.env,
                cwd=self.home,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{self.port}/health", timeout=2):
                    return
            except OSError:
                if self.process.poll() is not None:
                    break
                time.sleep(0.5)
        raise RuntimeError(f"daemon did not become healthy; see {self.log}")

    def stop(self) -> None:
        if self.process and self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=5)


def run(tachi: Path, keep: bool) -> int:
    report = Report()
    root = Path(tempfile.mkdtemp(prefix="tachi-dogfood-"))
    daemon = Daemon(tachi, root, free_port())
    try:
        version = daemon.cli("--version").stdout.strip()
        report.note("binary", version)
        repo = make_fixture_repo(root)
        daemon.start()
        scenarios(daemon, repo, report)
    except Exception as error:  # an aborted run is a failed run
        report.record("harness", False, f"{type(error).__name__}: {error}")
    finally:
        daemon.stop()
        if keep:
            report.note("temp dir kept", str(root))
        else:
            shutil.rmtree(root, ignore_errors=True)
    return report.print()


def scenarios(daemon: Daemon, repo: Path, report: Report) -> None:
    tag = uuid.uuid4().hex[:8]
    workspace = {"X-Tachi-Workspace-Root": str(repo)}

    # 1. A headerless connector learns what it is missing, in-band.
    bare = McpSession(daemon.url)
    ok, brief, raw = bare.call("tachi_memory", action="briefing", format="json")
    readiness = find_key(brief, "readiness")
    report.record(
        "headerless briefing reports readiness",
        ok and isinstance(readiness, dict) and readiness.get("project_bound") is False
        and "x-tachi" in json.dumps(readiness).lower(),
        short(readiness if readiness is not None else raw),
    )
    report.record(
        "headerless briefing explains emptiness",
        ok and find_key(brief, "empty_reason") is not None,
        short(find_key(brief, "empty_reason")),
    )

    # 2. The workspace header binds the session to the repo's library.
    session = McpSession(daemon.url, workspace)
    ok, brief, raw = session.call("tachi_memory", action="briefing", format="json")
    binding = find_key(brief, "binding") or {}
    bound = binding.get("session_project") if isinstance(binding, dict) else None
    report.record("workspace header binds the session", ok and bool(bound), short(bound or raw))
    size = len(raw.encode())
    report.record(
        "compact briefing stays within budget",
        size <= COMPACT_BRIEF_BUDGET_BYTES,
        f"{size} bytes (budget {COMPACT_BRIEF_BUDGET_BYTES})",
    )

    # 3. Save, get, and a miss with a stable shape.
    ok, saved, raw = session.call(
        "tachi_memory", action="save", text=f"dogfood {tag} the release train leaves on Thursday",
        path=f"/scratch/dogfood/{tag}/fact",
    )
    saved_id = saved.get("id") if isinstance(saved, dict) else None
    report.record("save returns an id", ok and bool(saved_id), short(saved if ok else raw))
    ok, got, raw = session.call("tachi_memory", action="get", id=saved_id)
    report.record("get reads the row back", ok and find_key(got, "id") == saved_id, short(raw, 100))
    _, missing, raw = session.call("tachi_memory", action="get", id=str(uuid.uuid4()))
    report.record(
        "get miss has found=false + reason",
        isinstance(missing, dict) and missing.get("found") is False and bool(missing.get("reason")),
        short(missing),
    )

    # 4. Supersede: the receipt says what happened and history stays opt-in.
    ok, old, _ = session.call(
        "tachi_memory", action="save", text=f"dogfood {tag} deploy target is staging",
        path=f"/scratch/dogfood/{tag}/old",
    )
    old_id = old.get("id") if isinstance(old, dict) else None
    ok, new, raw = session.call(
        "tachi_memory", action="save", text=f"dogfood {tag} deploy target is production",
        path=f"/scratch/dogfood/{tag}/new", supersedes=[old_id],
    )
    new_id = new.get("id") if isinstance(new, dict) else None
    applied = find_key(new, "applied")
    report.record(
        "save reports supersede outcome per id",
        ok and isinstance(applied, list) and old_id in applied,
        short(find_key(new, "supersede_outcome") or find_key(new, "supersedes") or raw),
    )
    _, got_old, _ = session.call("tachi_memory", action="get", id=old_id)
    report.record(
        "superseded row names its successor",
        find_key(got_old, "superseded_by") == new_id,
        short(find_key(got_old, "superseded_by")),
    )
    _, found, raw = session.call("tachi_memory", action="search", query=f"dogfood {tag} deploy target")
    report.record(
        "default search hides superseded history",
        old_id not in raw and new_id in raw,
        f"old_in_results={old_id in raw} new_in_results={new_id in raw}",
    )

    # 5. Near duplicate at another path is reported, not silent.
    text = f"dogfood {tag} identical text " + " ".join(f"w{i}" for i in range(16))
    _, first, _ = session.call("tachi_memory", action="save", text=text, path=f"/scratch/dogfood/{tag}/dup-a")
    _, second, raw = session.call(
        "tachi_memory", action="save", text=text, path=f"/scratch/dogfood/{tag}/dup-b",
        valid_from="2026-01-01T00:00:00Z",
    )
    report.record(
        "near duplicate is named on save",
        isinstance(second, dict) and second.get("near_duplicate_of") == first.get("id"),
        short({k: second.get(k) for k in ("near_duplicate_of", "near_duplicate_merged")}
              if isinstance(second, dict) else raw),
    )

    # 6. Two identities: claims collide, a2a is scoped to the caller.
    alice = McpSession(daemon.url, {**workspace, "X-Tachi-Agent-Identity": f"dogfood-alice-{tag}"})
    bob = McpSession(daemon.url, {**workspace, "X-Tachi-Agent-Identity": f"dogfood-bob-{tag}"})
    lease = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=10)).strftime("%Y-%m-%dT%H:%M:%SZ")
    claim = dict(
        action="claim", claim_scope=[f"src/dogfood_{tag}.rs"], claim_mode="writable",
        claim_role="implementer", lease_expires_at=lease, expected_head=repo_head(repo),
    )
    ok_a, claim_a, raw_a = alice.call("tachi_task", **claim)
    report.record("first claim on a scope succeeds", ok_a, short(claim_a if ok_a else raw_a))
    ok_b, claim_b, raw_b = bob.call("tachi_task", **claim)
    # Only meaningful once the first claim holds the scope; a refusal for any
    # other reason (e.g. claims unavailable) must not count as a collision.
    report.record(
        "competing claim on the same scope is refused",
        ok_a and not ok_b,
        short(raw_b) if ok_a else "not evaluated: first claim did not succeed",
    )
    ok, status, raw = alice.call("tachi_a2a", action="status")
    report.record(
        "a2a status is scoped to the caller's identity",
        ok and find_key(status, "actor_agent_identity_id") == f"dogfood-alice-{tag}",
        short(find_key(status, "actor_agent_identity_id") or raw),
    )

    # 7. An unbound session can never launch a worker.
    ok, refused, raw = bare.call(
        "tachi_staff", action="start", task=f"dogfood {tag} must refuse",
        staffing_reason="explicit_user_request", profile="codex_53_fast",
    )
    report.record(
        "unbound staff start is refused with a binding remedy",
        not ok and "bind" in raw.lower(),
        short(raw),
    )

    # 8. Repository short-name alias: record it, then address the project by it.
    apply = daemon.cli("migrate", "--git-identity", "--apply", cwd=repo)
    recorded = REPO_SHORT_NAME in apply.stdout and apply.returncode == 0
    report.record("migrate --git-identity --apply records the repo alias", recorded,
                  short((apply.stdout or apply.stderr).strip().splitlines()[-1:] or ""))
    if recorded:
        by_alias = McpSession(daemon.url, {"X-Tachi-Project": REPO_SHORT_NAME.lower()})
        ok, brief, raw = by_alias.call("tachi_memory", action="briefing", format="json")
        alias_binding = find_key(brief, "binding") or {}
        report.record(
            "X-Tachi-Project: <short name> binds the same library",
            ok and isinstance(alias_binding, dict)
            and alias_binding.get("session_project") == bound,
            short(alias_binding.get("session_project") if isinstance(alias_binding, dict) else raw),
        )
        ok, got, raw = by_alias.call("tachi_memory", action="get", id=saved_id)
        report.record("alias session reads the same rows", ok and find_key(got, "id") == saved_id,
                      short(raw, 100))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tachi", type=Path, required=True, help="tachi binary to exercise")
    parser.add_argument("--keep", action="store_true", help="keep the temporary home for inspection")
    arguments = parser.parse_args()
    tachi = arguments.tachi.expanduser().resolve()
    if not os.access(tachi, os.X_OK):
        parser.error(f"not executable: {tachi}")
    return run(tachi, arguments.keep)


if __name__ == "__main__":
    sys.exit(main())
