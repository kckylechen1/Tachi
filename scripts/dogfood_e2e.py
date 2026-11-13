#!/usr/bin/env python3
"""Local end-to-end dogfood run against a real Tachi binary.

Starts the binary as an isolated daemon (temporary HOME / TACHI_HOME, its own
port, never the live ~/.tachi), connects over HTTP MCP exactly like a host
connector does, and runs dogfood scenarios: each one is how an agent actually
uses a feature, end to end. Every user- or agent-visible behavior change adds
or updates a scenario here (AGENTS.md §3).

    python3 scripts/dogfood_e2e.py --tachi ./target/release/tachi
    python3 scripts/dogfood_e2e.py --tachi ./candidate --baseline ~/bin/tachi
    python3 scripts/dogfood_e2e.py --tachi ./candidate --only claims,alias
    python3 scripts/dogfood_e2e.py --list

Without --baseline, any FAIL exits non-zero. With --baseline, both binaries
run the same scenarios and the exit code reports regressions only: a check
that passes on the baseline and fails on the candidate.

Adding a scenario: write a function taking a `Context`, decorate it with
`@scenario("feature-tag", ...)`, and record checks with `ctx.check(...)`.
Scenarios run in definition order and may use the shared helpers on Context
(a bound session, the fixture repo, a unique run tag).
"""

from __future__ import annotations

import argparse
import ast
import datetime as dt
import hashlib
import inspect
import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import textwrap
import time
import urllib.request
import uuid
from contextlib import closing
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

REPO_ORIGIN = "https://github.com/kckylechen1/Tachi.git"
REPO_SHORT_NAME = "Tachi"
COMPACT_BRIEF_BUDGET_BYTES = 16 * 1024


# ── MCP over HTTP ─────────────────────────────────────────────────────────────


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


# ── Isolated daemon and fixture ──────────────────────────────────────────────


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
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": "protocol.allow",
            "GIT_CONFIG_VALUE_0": "never",
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


# ── Results ───────────────────────────────────────────────────────────────────


@dataclass
class Check:
    scenario: str
    name: str
    status: str  # PASS / FAIL / NOTE
    evidence: str


@dataclass
class Run:
    binary: str = ""
    checks: list[Check] = field(default_factory=list)
    completed: list[str] = field(default_factory=list)

    def outcome(self) -> dict[str, str]:
        return {c.name: c.status for c in self.checks if c.status != "NOTE"}


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


def fixture_connection(db: Path, mode: str) -> sqlite3.Connection:
    """Raw handle over one private fixture store.

    The kernel registers its per-connection guard functions when it opens a
    store, so a bare handle must register the same ones before the `memories`
    triggers can resolve them (0 == no typed-write authorization on this
    handle, matching a freshly opened store).
    """
    conn = sqlite3.connect(f"{db.as_uri()}?mode={mode}", uri=True)
    conn.create_function("tachi_reserved_reference_write_enabled", 0, lambda: 0)
    conn.create_function("tachi_canonical_supersession_edge_write_enabled", 0, lambda: 0)
    return conn


# ── Scenario registry ─────────────────────────────────────────────────────────


@dataclass
class Scenario:
    name: str
    tags: tuple[str, ...]
    run: Callable[["Context"], None]


SCENARIOS: list[Scenario] = []


class ScenarioAborted(Exception):
    """A required check failed; its FAIL row is already recorded."""


def _scenario_returns(function: Callable) -> list[int]:
    """Line numbers of `return` in the scenario body itself (nested helpers excluded)."""
    tree = ast.parse(textwrap.dedent(inspect.getsource(function)))
    body = tree.body[0]
    found: list[int] = []

    def visit(node: ast.AST) -> None:
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda, ast.ClassDef)):
                continue
            if isinstance(child, ast.Return):
                found.append(child.lineno)
            visit(child)

    visit(body)
    return found


def scenario(*tags: str):
    def register(function: Callable[["Context"], None]):
        # A scenario ends only by running to completion or by a recorded failure
        # (ctx.require / ctx.abort); a bare return would silently skip checks.
        if hasattr(function, "__wrapped__"):
            # The source checked below must be the callable that actually runs.
            raise RuntimeError(f"scenario {function.__name__} must be a plain function, not a wrapper")
        if returns := _scenario_returns(function):
            raise RuntimeError(f"scenario {function.__name__} uses return; use ctx.require or ctx.abort ({returns})")
        SCENARIOS.append(Scenario(function.__name__, tags, function))
        return function

    return register


class Context:
    def __init__(self, daemon: Daemon, repo: Path, run: Run):
        self.daemon = daemon
        self.repo = repo
        self.run = run
        self.tag = uuid.uuid4().hex[:8]
        self.current = ""
        self._session: McpSession | None = None
        self._bound: str | None = None

    # recording
    def check(self, name: str, ok: bool, evidence: object) -> bool:
        self.run.checks.append(Check(self.current, name, "PASS" if ok else "FAIL", short(evidence)))
        return ok

    def require(self, name: str, ok: bool, evidence: object) -> None:
        """Record a check whose failure makes the rest of the scenario meaningless."""
        if not self.check(name, ok, evidence):
            raise ScenarioAborted(name)

    def abort(self) -> None:
        """Stop after the caller has recorded the FAIL rows explaining why."""
        raise ScenarioAborted(self.current)

    def note(self, name: str, evidence: object) -> None:
        self.run.checks.append(Check(self.current, name, "NOTE", short(evidence)))

    # shared helpers
    @property
    def workspace_headers(self) -> dict[str, str]:
        return {"X-Tachi-Workspace-Root": str(self.repo)}

    def connect(self, **headers: str) -> McpSession:
        return McpSession(self.daemon.url, {k.replace("_", "-"): v for k, v in headers.items()})

    @property
    def session(self) -> McpSession:
        """A session bound to the fixture repo through the workspace header."""
        if self._session is None:
            self._session = McpSession(self.daemon.url, self.workspace_headers)
        return self._session

    @property
    def bound_project(self) -> str | None:
        if self._bound is None:
            _, brief, _ = self.session.call("tachi_memory", action="briefing", format="json")
            binding = find_key(brief, "binding")
            self._bound = binding.get("session_project") if isinstance(binding, dict) else None
        return self._bound

    def head(self) -> str:
        return subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()


# ── Scenarios ─────────────────────────────────────────────────────────────────
# Each scenario is one agent-visible behavior, exercised the way a host uses it.
# Source of the original set: the 2026-11-08 dogfood round (#2242).


@scenario("binding", "readiness")
def headerless_connector_learns_what_is_missing(ctx: Context) -> None:
    ok, brief, raw = ctx.connect().call("tachi_memory", action="briefing", format="json")
    readiness = find_key(brief, "readiness")
    ctx.check(
        "headerless briefing reports readiness",
        ok and isinstance(readiness, dict) and readiness.get("project_bound") is False
        and "x-tachi" in json.dumps(readiness).lower(),
        readiness if readiness is not None else raw,
    )
    ctx.check(
        "headerless briefing explains emptiness",
        ok and find_key(brief, "empty_reason") is not None,
        find_key(brief, "empty_reason"),
    )


@scenario("binding", "briefing")
def workspace_header_binds_and_briefing_stays_small(ctx: Context) -> None:
    ctx.check("workspace header binds the session", bool(ctx.bound_project), ctx.bound_project)
    _, _, raw = ctx.session.call("tachi_memory", action="briefing", format="json")
    size = len(raw.encode())
    ctx.check(
        "compact briefing stays within budget",
        size <= COMPACT_BRIEF_BUDGET_BYTES,
        f"{size} bytes (budget {COMPACT_BRIEF_BUDGET_BYTES})",
    )


@scenario("memory")
def save_get_and_miss(ctx: Context) -> None:
    ok, saved, raw = ctx.session.call(
        "tachi_memory", action="save", text=f"dogfood {ctx.tag} the release train leaves on Thursday",
        path=f"/scratch/dogfood/{ctx.tag}/fact",
    )
    saved_id = saved.get("id") if isinstance(saved, dict) else None
    ctx.check("save returns an id", ok and bool(saved_id), saved if ok else raw)
    ok, got, raw = ctx.session.call("tachi_memory", action="get", id=saved_id)
    ctx.check("get reads the row back", ok and find_key(got, "id") == saved_id, raw[:100])
    _, missing, _ = ctx.session.call("tachi_memory", action="get", id=str(uuid.uuid4()))
    ctx.check(
        "get miss has found=false + reason",
        isinstance(missing, dict) and missing.get("found") is False and bool(missing.get("reason")),
        missing,
    )


@scenario("memory", "supersede")
def supersede_is_reported_and_history_is_opt_in(ctx: Context) -> None:
    _, old, _ = ctx.session.call(
        "tachi_memory", action="save", text=f"dogfood {ctx.tag} deploy target is staging",
        path=f"/scratch/dogfood/{ctx.tag}/old",
    )
    old_id = old.get("id") if isinstance(old, dict) else None
    ok, new, raw = ctx.session.call(
        "tachi_memory", action="save", text=f"dogfood {ctx.tag} deploy target is production",
        path=f"/scratch/dogfood/{ctx.tag}/new", supersedes=[old_id],
    )
    new_id = new.get("id") if isinstance(new, dict) else None
    applied = find_key(new, "applied")
    ctx.check(
        "save reports supersede outcome per id",
        ok and isinstance(applied, list) and old_id in applied,
        find_key(new, "supersede_outcome") or find_key(new, "supersedes") or raw,
    )
    _, got_old, _ = ctx.session.call("tachi_memory", action="get", id=old_id)
    ctx.check("superseded row names its successor",
              find_key(got_old, "superseded_by") == new_id, find_key(got_old, "superseded_by"))
    _, _, raw = ctx.session.call("tachi_memory", action="search", query=f"dogfood {ctx.tag} deploy target")
    ctx.check(
        "default search hides superseded history",
        old_id not in raw and new_id in raw,
        f"old_in_results={old_id in raw} new_in_results={new_id in raw}",
    )


@scenario("memory", "dedupe")
def near_duplicate_is_named(ctx: Context) -> None:
    text = f"dogfood {ctx.tag} identical text " + " ".join(f"w{i}" for i in range(16))
    _, first, _ = ctx.session.call("tachi_memory", action="save", text=text,
                                   path=f"/scratch/dogfood/{ctx.tag}/dup-a")
    _, second, raw = ctx.session.call(
        "tachi_memory", action="save", text=text, path=f"/scratch/dogfood/{ctx.tag}/dup-b",
        valid_from="2026-01-01T00:00:00Z",
    )
    ctx.check(
        "near duplicate is named on save",
        isinstance(second, dict) and second.get("near_duplicate_of") == first.get("id"),
        {k: second.get(k) for k in ("near_duplicate_of", "near_duplicate_merged")}
        if isinstance(second, dict) else raw,
    )


@scenario("memory", "synthetic", "briefing")
def synthetic_recall_is_opt_in_and_real_lessons_remain_visible(ctx: Context) -> None:
    ctx.require("synthetic recall fixture has a bound project",
                bool(ctx.bound_project), ctx.bound_project)
    query = f"SyntheticRecallIsolation{ctx.tag}"
    seeds = [
        ("reserved", f"/scratch/tachi-test/{ctx.tag}", False, "invented amber cargo manifests"),
        ("marked", f"/notes/dogfood/{ctx.tag}", True, "fabricated violet weather stations"),
        ("real", f"/scratch/dogfood/{ctx.tag}/production", False, "actual blue release recovery"),
        ("testing", f"/testing/dogfood/{ctx.tag}", False, "recorded green deployment checklist"),
        ("eval-real", f"/eval/dogfood/{ctx.tag}/real", False, "failure lesson preserve lease ownership"),
        ("eval-marked", f"/eval/dogfood/{ctx.tag}/fixture", True, "invented failure lesson for a simulation"),
    ]
    ids: dict[str, str] = {}
    for label, path, marked, text in seeds:
        expected_id = str(uuid.uuid4())
        ok, saved, raw = ctx.session.call(
            "tachi_memory", action="save", id=expected_id,
            text=f"{query} {text}", path=path, metadata={"synthetic_fixture": marked},
        )
        ctx.require(f"seed {label} memory", ok and isinstance(saved, dict)
                    and saved.get("id") == expected_id, saved if ok else raw)
        ids[label] = expected_id

    for action in ("search", "ask", "briefing"):
        for include_test in (False, True):
            ok, _, raw = ctx.session.call(
                "tachi_memory", action=action, query=query, format="json",
                scope="memory", top_k=10, compact=False, include_test=include_test,
                auto_conversation_scope=False, multi_evidence_pack=False,
            )
            ctx.check(
                f"{action} synthetic opt-in={include_test}",
                ok and all((ids[label] in raw) == include_test for label in ("reserved", "marked"))
                and all(ids[label] in raw for label in ("real", "testing")),
                {label: memory_id in raw for label, memory_id in ids.items()},
            )

    ok, _, raw = ctx.session.call(
        "tachi_memory", action="search", query=query, format="json", scope="memory",
        path_prefix="/scratch/tachi-test", auto_conversation_scope=False,
    )
    ctx.check("explicit reserved-prefix recall remains available",
              ok and ids["reserved"] in raw, raw)
    ok, _, raw = ctx.session.call(
        "tachi_memory", action="search", query=query, format="json", scope="memory",
        path_prefix="/eval", auto_conversation_scope=False, multi_evidence_pack=False,
    )
    ctx.check("eval scope preserves real lessons and excludes marked simulations",
              ok and ids["eval-real"] in raw and ids["eval-marked"] not in raw, raw)
    ok, _, raw = ctx.session.call("tachi_memory", action="get", id=ids["marked"])
    ctx.check("explicit get can inspect an isolated fixture", ok and ids["marked"] in raw, raw)


@scenario("claims", "identity")
def two_identities_contend_for_a_scope(ctx: Context) -> None:
    alice = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": f"dogfood-alice-{ctx.tag}"})
    bob = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": f"dogfood-bob-{ctx.tag}"})
    worktrees = [ctx.repo.parent / f"claim-{name}-{ctx.tag}" for name in ("alice", "bob")]
    for path in worktrees:
        subprocess.run(
            ["git", "-C", str(ctx.repo), "worktree", "add", "--quiet", "--detach", str(path), "HEAD"],
            check=True, capture_output=True,
            env={**os.environ, "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"},
        )
    lease = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=10)).strftime("%Y-%m-%dT%H:%M:%SZ")
    claim = dict(
        action="claim", claim_scope=[f"src/dogfood_{ctx.tag}.rs"], claim_mode="writable",
        claim_role="implementer", lease_expires_at=lease, expected_head=ctx.head(),
        issue_ref="kckylechen1/Tachi#2281",
    )
    ok, _, raw = alice.call("tachi_task", **claim)
    ctx.check("writable claim names the missing worktree_path",
              not ok and "worktree_path" in raw, raw)
    incomplete = {key: value for key, value in claim.items() if key not in {"claim_role", "expected_head"}}
    ok, _, raw = alice.call("tachi_task", **incomplete)
    ctx.check("claim reports missing required fields together",
              not ok and all(field in raw for field in ("claim_role", "expected_head", "worktree_path")), raw)
    valid_a = {**claim, "worktree_path": str(worktrees[0])}
    ok, _, raw = ctx.connect().call("tachi_task", **valid_a)
    ctx.check("headerless claim is refused", not ok and "admission" in raw.lower(), raw)
    ok, _, raw = alice.call("tachi_task", **valid_a, agent_identity_id=f"dogfood-bob-{ctx.tag}")
    ctx.check("claim cannot replace the admitted identity", not ok and "must match" in raw, raw)
    ok, _, raw = alice.call("tachi_task", **valid_a, project=f"foreign-{ctx.tag}")
    ctx.check("claim cannot replace the bound project", not ok, raw)
    ok_a, claim_a, raw_a = alice.call("tachi_task", **valid_a)
    ctx.check("first claim on a scope succeeds", ok_a, claim_a if ok_a else raw_a)
    ok_b, _, raw_b = bob.call("tachi_task", **claim, worktree_path=str(worktrees[1]))
    # Only meaningful once the first claim holds the scope.
    ctx.check(
        "same-issue scope conflict across distinct worktrees is actionable and private",
        ok_a and not ok_b and "WorkClaim conflict:" in raw_b
        and claim_a.get("claim_id", "") not in raw_b,
        raw_b if ok_a else "not evaluated: first claim did not succeed",
    )
    disjoint = {**claim, "claim_scope": [f"src/disjoint_{ctx.tag}.rs"], "worktree_path": str(worktrees[1])}
    ok_b, claim_b, raw_b = bob.call("tachi_task", **disjoint)
    ctx.check("second identity can claim a disjoint scope after refusal", ok_a and ok_b, raw_b)
    ok, status, raw = alice.call("tachi_a2a", action="status")
    ctx.check(
        "a2a status is scoped to the caller's identity",
        ok and find_key(status, "actor_agent_identity_id") == f"dogfood-alice-{ctx.tag}",
        find_key(status, "actor_agent_identity_id") or raw,
    )
    for name, session, ok, result in (("first", alice, ok_a, claim_a), ("second", bob, ok_b, claim_b)):
        if ok:
            released, _, raw = session.call(
                "tachi_task", action="release", claim_id=result["claim_id"], transition_version=0,
            )
            ctx.check(f"{name} claim releases after contention", released, raw)


@scenario("staff", "binding")
def unbound_session_cannot_launch_a_worker(ctx: Context) -> None:
    ok, _, raw = ctx.connect().call(
        "tachi_staff", action="start", task=f"dogfood {ctx.tag} must refuse",
        staffing_reason="explicit_user_request", profile="codex_sol_review",
    )
    ctx.check("unbound staff start is refused with a binding remedy", not ok and "bind" in raw.lower(), raw)


@scenario("submission", "staff-recovery")
def lost_staff_start_response_recovers_original_run(ctx: Context) -> None:
    """Canonical HTTP lifecycle with a deterministic worker, not a live vendor canary."""
    root = ctx.daemon.home / f"staff-recovery-{ctx.tag}"
    daemon = Daemon(ctx.daemon.tachi, root, free_port())
    repo = make_fixture_repo(root)
    native_fixture(daemon, root)

    def release_workers() -> None:
        for home in (daemon.app_home / "runs").glob("*/runtime/home"):
            (home / "release").touch()

    headers = {"X-Tachi-Workspace-Root": str(repo), "X-Tachi-Agent-Identity": f"submission-owner-{ctx.tag}"}
    start = dict(action="start", submission_id=f"lost-{ctx.tag}",
                 task=f"[hold] Read README.md and report once. Submission fixture {ctx.tag}.",
                 staffing_reason="explicit_user_request", profile="codex_sol_review", format="json")

    def launches() -> int:
        return sum(1 for _ in (daemon.app_home / "runs").glob("*/runtime/home/protocol.jsonl"))

    def until(predicate, seconds: float = 20) -> bool:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if predicate():
                return True
            time.sleep(0.25)
        return bool(predicate())

    def terminal(session: McpSession, dispatch_id: str) -> bool:
        release_workers()
        ok, status, _ = session.call("tachi_staff", action="status", dispatch_id=dispatch_id, format="json")
        if ok and find_key(status, "phase") == "awaiting_instruction":
            session.call("tachi_staff", action="finish", dispatch_id=dispatch_id,
                         expected_status_revision=find_key(status, "status_revision"), format="json")
            return False
        return ok and find_key(status, "state") == "TASK_STATE_COMPLETED"

    try:
        daemon.start()
        owner = McpSession(daemon.url, headers)
        ok, dropped_receipt, raw = owner.call("tachi_staff", **start)
        ctx.require("Staff accepts a keyed start before its response is discarded", ok, raw)
        # Deliberately lose the dispatch-bearing response. Recovery below has
        # only the caller's durably saved submission key and a fresh MCP session.
        del dropped_receipt, raw
        reconnected = McpSession(daemon.url, headers)
        ok, recovered, raw = reconnected.call(
            "tachi_staff", action="status", submission_id=start["submission_id"], format="json",
        )
        dispatch_id = find_key(recovered, "dispatch_id")
        ctx.require("reconnected owner recovers its original dispatch from the submission key",
                    ok and bool(dispatch_id) and find_key(recovered, "binding_state") == "bound", raw)
        ctx.check("accepted keyed start launches exactly one deterministic worker",
                  until(lambda: launches() == 1), launches())
        before_replay = (daemon.app_home / "runs" / dispatch_id / "mcp-tasks.json").read_bytes()
        polls = [reconnected.call("tachi_staff", action="status", submission_id=start["submission_id"],
                                  format="json") for _ in range(12)]
        ctx.check("repeated submission recovery remains a read-only status poll",
                  all(ok and find_key(payload, "dispatch_id") == dispatch_id for ok, payload, _ in polls)
                  and (daemon.app_home / "runs" / dispatch_id / "mcp-tasks.json").read_bytes() == before_replay,
                  polls[-1][2])
        ok, replayed, raw = reconnected.call("tachi_staff", **start)
        ctx.check("same-key same-input start replays without a second worker",
                  ok and find_key(replayed, "dispatch_id") == dispatch_id
                  and find_key(replayed, "replayed") is True and launches() == 1, raw)
        ok, _, raw = reconnected.call("tachi_staff", **{**start, "task": start["task"] + " Changed."})
        ctx.check("same-key changed-input start conflicts without overwriting its binding",
                  not ok and "conflict" in raw.lower() and launches() == 1
                  and (daemon.app_home / "runs" / dispatch_id / "mcp-tasks.json").read_bytes() == before_replay, raw)
        stranger = McpSession(daemon.url, {**headers, "X-Tachi-Agent-Identity": f"submission-other-{ctx.tag}"})
        ok, unrelated, raw = stranger.call(
            "tachi_staff", action="status", submission_id=start["submission_id"], format="json",
        )
        ctx.check("another admitted identity cannot discover the original submission",
                  ok and find_key(unrelated, "binding_state") == "unconfirmed"
                  and find_key(unrelated, "dispatch_id") is None and dispatch_id not in raw, raw)
        ok, missing, raw = reconnected.call(
            "tachi_staff", action="status", submission_id=f"missing-{ctx.tag}", format="json",
        )
        ctx.check("missing submission stays unconfirmed and never authorizes retry",
                  ok and find_key(missing, "binding_state") == "unconfirmed"
                  and find_key(missing, "retry_safe") is False and launches() == 1, raw)
        release_workers()
        ctx.require("original worker reaches its canonical completed receipt",
                    until(lambda: terminal(reconnected, dispatch_id)), dispatch_id)
        run_dir = daemon.app_home / "runs" / dispatch_id
        binding_before_restart = (run_dir / "mcp-tasks.json").read_bytes()
        daemon.stop()
        daemon.start()
        restarted = McpSession(daemon.url, headers)
        ok, recovered, raw = restarted.call(
            "tachi_staff", action="status", submission_id=start["submission_id"], format="json",
        )
        ctx.check("daemon restart preserves the original submission association",
                  ok and find_key(recovered, "dispatch_id") == dispatch_id
                  and find_key(recovered, "binding_state") == "bound", raw)
        ok, replayed, raw = restarted.call("tachi_staff", **start)
        ctx.check("terminal replay after restart neither starts work nor mutates the binding",
                  ok and find_key(replayed, "dispatch_id") == dispatch_id and launches() == 1
                  and (run_dir / "mcp-tasks.json").read_bytes() == binding_before_restart, raw)
        ok, second, raw = restarted.call("tachi_staff", **{**start, "submission_id": f"new-{ctx.tag}"})
        second_id = find_key(second, "dispatch_id")
        ctx.require("a new submission of the same task receives a distinct dispatch",
                    ok and bool(second_id) and second_id != dispatch_id, raw)
        completed = until(lambda: launches() == 2 and terminal(restarted, second_id))
        ok, original, raw = restarted.call(
            "tachi_staff", action="status", submission_id=start["submission_id"], format="json",
        )
        ctx.check("recovery selects the original submission instead of the newest matching task",
                  completed and ok and find_key(original, "dispatch_id") == dispatch_id and launches() == 2, raw)
    finally:
        release_workers()
        daemon.stop()


@scenario("alias", "binding")
def repository_short_name_addresses_the_library(ctx: Context) -> None:
    bound = ctx.bound_project  # registers the repo-local library first
    apply = ctx.daemon.cli("migrate", "--git-identity", "--apply", cwd=ctx.repo)
    recorded = REPO_SHORT_NAME in apply.stdout and apply.returncode == 0
    ctx.check("migrate --git-identity --apply records the repo alias", recorded,
              (apply.stdout or apply.stderr).strip().splitlines()[-1:] or "")
    if not recorded:
        ctx.check("X-Tachi-Project: <short name> binds the same library", False,
                  "not evaluated: the repo alias was not recorded")
        ctx.abort()
    by_alias = ctx.connect(**{"X-Tachi-Project": REPO_SHORT_NAME.lower()})
    ok, brief, raw = by_alias.call("tachi_memory", action="briefing", format="json")
    binding = find_key(brief, "binding")
    alias_bound = binding.get("session_project") if isinstance(binding, dict) else None
    ctx.check("X-Tachi-Project: <short name> binds the same library", ok and bool(bound) and alias_bound == bound,
              alias_bound or raw)


def modern_request(url: str, method: str, params: dict) -> dict:
    """One sessionless MCP 2026-07-28 request, the way a skill-loading host sends it."""
    meta = {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "tachi-dogfood-e2e", "version": "1"},
    }
    headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
        "MCP-Protocol-Version": "2026-07-28",
        "Mcp-Method": method,
    }
    if method == "resources/read":
        headers["Mcp-Name"] = params["uri"]
    body = {"jsonrpc": "2.0", "id": 1, "method": method, "params": {"_meta": meta, **params}}
    request = urllib.request.Request(url, data=json.dumps(body).encode(), headers=headers, method="POST")
    with urllib.request.urlopen(request, timeout=120) as response:
        raw = response.read().decode()
    events = [line[5:].strip() for line in raw.splitlines() if line.startswith("data:") and line[5:].strip()]
    return json.loads(events[-1] if events else raw)


@scenario("skills")
def skill_host_lists_gets_and_loads_the_tachi_skill(ctx: Context) -> None:
    listing = modern_request(ctx.daemon.url, "skills/list", {})
    skills = (listing.get("result") or {}).get("skills") or []
    entry = skills[0] if len(skills) == 1 else {}
    ctx.check("skills/list serves exactly the tachi skill",
              entry.get("frontmatter", {}).get("name") == "tachi", listing.get("error") or skills)
    uri = entry.get("uri")
    # Everything below is only meaningful once skills/list names a usable entry;
    # skipping it would hide owned checks from the report, so record the skips.
    if not uri:
        unmet = "not evaluated: skills/list returned no entry"
        ctx.check("skills/get returns the listed entry", False, unmet)
        ctx.check("loaded SKILL.md matches the advertised digest and size", False, unmet)
        ctx.check("a cursor Tachi never issued is refused", False, unmet)
        ctx.abort()
    got = modern_request(ctx.daemon.url, "skills/get", {"uri": uri})
    ctx.check("skills/get returns the listed entry", (got.get("result") or {}).get("skill") == entry,
              got.get("error") or got.get("result"))
    read = modern_request(ctx.daemon.url, "resources/read", {"uri": uri})
    contents = (read.get("result") or {}).get("contents") or [{}]
    content = contents[0]
    body = content.get("text", "")
    resource = (entry.get("resources") or [{}])[0]
    digest = "sha256:" + hashlib.sha256(body.encode()).hexdigest()
    # Conformance pins the advertised resource as {uri, size, digest} for this entry.
    ctx.check("loaded SKILL.md matches the advertised digest and size",
              bool(body) and content.get("uri") == uri and resource.get("uri") == uri
              and resource.get("digest") == digest and resource.get("size") == len(body.encode()),
              f"{resource} vs {uri}/{digest}/{len(body.encode())} (read uri {content.get('uri')})")
    forged = modern_request(ctx.daemon.url, "skills/list", {"cursor": "forged"})
    ctx.check("a cursor Tachi never issued is refused",
              (forged.get("error") or {}).get("code") == -32602, forged.get("error") or forged.get("result"))


@scenario("legacy-copy", "startup")
def interrupted_legacy_copy_refuses_then_recovers(ctx: Context) -> None:
    """An interrupted copy is fenced; an offline quarantine permits retry."""
    root = ctx.daemon.home / f"legacy-copy-{ctx.tag}"
    daemon = Daemon(ctx.daemon.tachi, root, free_port())
    target = daemon.app_home / "global" / "tachi-memory.db"
    source = daemon.app_home / "memory.db"
    marker = Path(str(target) + ".legacy-copy-incomplete")
    suffixes = ("", "-wal", "-shm")
    try:
        daemon.start()
        session = McpSession(daemon.url)
        ok, saved, raw = session.call(
            "tachi_memory", action="save", kind="note", format="json",
            storage_scope="global", text=f"Legacy recovery preserves dogfood row {ctx.tag}.",
            path=f"/scratch/dogfood/{ctx.tag}/legacy-recovery",
        )
        saved_id = find_key(saved, "id")
        ctx.require("legacy recovery fixture is saved through HTTP MCP", ok and bool(saved_id), raw)
        daemon.stop()

        # Move a stopped daemon's complete DB set into the pre-layout location.
        # The copy's failed state has a main file, but no verified WAL receipt.
        for suffix in suffixes:
            old = Path(str(target) + suffix)
            if old.exists():
                old.rename(Path(str(source) + suffix))
        source_hashes = {
            suffix: hashlib.sha256(Path(str(source) + suffix).read_bytes()).hexdigest()
            for suffix in suffixes if Path(str(source) + suffix).exists()
        }
        shutil.copyfile(source, target)
        target_before = hashlib.sha256(target.read_bytes()).hexdigest()
        marker.write_text(f"legacy-copy-v1\nsource={source}\ntarget={target}\n")
        refused = False
        try:
            daemon.start()
        except RuntimeError:
            refused = daemon.process is not None and daemon.process.poll() not in (None, 0)
        refusal_log = daemon.log.read_text(errors="replace")
        daemon.stop()
        ctx.require(
            "incomplete legacy copy refuses startup before HTTP admission",
            refused and "incomplete legacy database copy" in refusal_log,
            refusal_log[-600:],
        )
        ctx.check(
            "refused legacy copy preserves its target and recovery marker",
            marker.exists() and hashlib.sha256(target.read_bytes()).hexdigest() == target_before,
            str(marker),
        )

        # An operator preserves the incomplete set offline before clearing the
        # admission marker. All work stays in this disposable dogfood home.
        quarantine = root / "quarantine"
        quarantine.mkdir()
        for suffix in suffixes:
            partial = Path(str(target) + suffix)
            if partial.exists():
                partial.rename(quarantine / partial.name)
        marker.rename(quarantine / marker.name)
        daemon.start()
        recovered = McpSession(daemon.url)
        ok, row, raw = recovered.call("tachi_memory", action="get", id=saved_id, format="json")
        ctx.check("legacy-copy retry preserves the saved row through HTTP MCP",
                  ok and find_key(row, "id") == saved_id, raw)
        ctx.check("completed legacy copy clears its admission marker", not marker.exists(), str(marker))
        ctx.check(
            "legacy-copy recovery never changes the source snapshot",
            all(hashlib.sha256(Path(str(source) + suffix).read_bytes()).hexdigest() == digest
                for suffix, digest in source_hashes.items()),
            source_hashes,
        )
    finally:
        daemon.stop()


@scenario("mailbox", "coordination")
def project_mailbox_delivers_at_a_host_turn_boundary(ctx: Context) -> None:
    sender_id, recipient_id = f"mail-sender-{ctx.tag}", f"mail-recipient-{ctx.tag}"
    sender = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": sender_id})
    recipient = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": recipient_id})
    other_root = ctx.daemon.home / f"mail-other-{ctx.tag}"
    other_root.mkdir()
    other_repo = make_fixture_repo(other_root)
    subprocess.run(["git", "-C", str(other_repo), "remote", "set-url", "origin",
                    "https://github.com/kckylechen1/Dogfood-Other.git"], check=True)
    foreign = ctx.connect(**{"X-Tachi-Workspace-Root": str(other_repo), "X-Tachi-Agent-Identity": recipient_id})
    foreign_id = f"foreign-{ctx.tag}"
    ctx.connect(**{"X-Tachi-Workspace-Root": str(other_repo), "X-Tachi-Agent-Identity": foreign_id})
    body = f"Release candidate {ctx.tag} is ready for review."
    request = dict(action="send", recipient_agent_identity_id=recipient_id,
                   subject_ref=f"coordination:release-{ctx.tag}", text=body,
                   idempotency_key=f"mail-{ctx.tag}", ttl_days=1)
    ok, created, raw = sender.call("tachi_a2a", **request)
    envelope_id = find_key(created, "envelope_id")
    ctx.require("mailbox send creates one coordination envelope", ok and envelope_id
                and find_key(created, "kind") == "coordination/v1", raw)
    ok, replay, raw = sender.call("tachi_a2a", **request)
    ctx.check("mailbox same-input replay returns the original envelope", ok
              and find_key(replay, "envelope_id") == envelope_id and find_key(replay, "delivery") == "replayed", raw)
    ok, _, raw = sender.call("tachi_a2a", **{**request, "text": "Changed input"})
    ctx.check("mailbox changed-input replay conflicts", not ok and "conflict" in raw, raw)
    ok, inventory, raw = recipient.call("tachi_a2a", action="status")
    ctx.check("mailbox status is non-consuming and body-free", ok and body not in raw
              and find_key(inventory, "current_state") == "received", raw)
    ok, unavailable, raw = foreign.call("tachi_a2a", action="receive")
    ctx.check("mailbox same identity in another project sees no body", ok
              and unavailable.get("messages") == [], raw)
    denials = [sender.call("tachi_a2a", **{**request, "recipient_agent_identity_id": value})
               for value in (foreign_id, f"absent-{ctx.tag}")]
    ctx.check("mailbox absent and cross-project recipients have the same denial",
              not denials[0][0] and not denials[1][0] and denials[0][2] == denials[1][2], [row[2] for row in denials])
    recipient.call("tachi_memory", action="briefing", format="json")
    ok, pending, raw = recipient.call("tachi_a2a", action="status")
    ctx.check("mailbox ordinary briefing leaves coordination pending", ok
              and find_key(pending, "current_state") == "received", raw)
    hook = Path(__file__).with_name("tachi_mailbox_hook.py")
    env = {**ctx.daemon.env, "TACHI_MCP_URL": ctx.daemon.url, "TACHI_AGENT_IDENTITY": recipient_id}
    delivered = subprocess.run([sys.executable, str(hook)], input=json.dumps({
        "hook_event_name": "UserPromptSubmit", "cwd": str(ctx.repo), "prompt": "continue"}),
        env=env, text=True, capture_output=True, timeout=20)
    rendered = json.loads(delivered.stdout) if delivered.stdout.strip() else {}
    context = find_key(rendered, "additionalContext") or ""
    ctx.check("mailbox host hook delivers the peer body without human relay",
              delivered.returncode == 0 and body in context and envelope_id in context
              and find_key(rendered, "hookEventName") == "UserPromptSubmit", rendered or delivered.stderr)
    reconnected = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": recipient_id})
    ok, empty, raw = reconnected.call("tachi_a2a", action="receive")
    ctx.check("mailbox reconnect does not redeliver consumed content", ok and empty.get("messages") == [], raw)
    ok, terminal, raw = sender.call("tachi_a2a", action="status")
    chain = find_key(terminal, "receipts") or []
    ctx.check("mailbox issuer observes the complete consumption receipt chain", ok
              and [row["state"] for row in chain] == ["received", "accepted", "consumed"], raw)
    unbound = ctx.connect(**{"X-Tachi-Agent-Identity": recipient_id})
    ok, _, raw = unbound.call("tachi_a2a", action="receive")
    ctx.check("mailbox receive refuses an unbound project", not ok, raw)
    ok, _, raw = sender.call("tachi_a2a", **{**request, "idempotency_key": "secret",
        "text": "Authorization: Bearer sk-abc123def456ghi789jkl012mno345"})
    ctx.check("mailbox rejects credential-bearing content before storage", not ok and "secret-bearing" in raw, raw)
    no_ttl = {key: value for key, value in request.items() if key != "ttl_days"}
    ok, _, raw = sender.call("tachi_a2a", **no_ttl)
    ctx.check("mailbox send requires an explicit TTL", not ok and "ttl_days" in raw, raw)
    import sqlite3
    dbs = sorted({*ctx.daemon.app_home.rglob("*.db"), *ctx.repo.rglob("*.db"), *other_repo.rglob("*.db")})
    inspected, leaked = 0, 0
    for db in dbs:
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as connection:
            if connection.execute("SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='memories'").fetchone()[0]:
                inspected += 1
                leaked += connection.execute("SELECT COUNT(*) FROM memories WHERE instr(text,?)>0 OR instr(summary,?)>0", (body, body)).fetchone()[0]
    ctx.check("mailbox coordination never creates a durable memory", inspected > 0 and leaked == 0,
              {"memory_databases_checked": inspected, "matching_memories": leaked})


def native_fixture(daemon: Daemon, root: Path) -> Path:
    bins = root / "bin"
    bins.mkdir(exist_ok=True)
    source = Path(__file__).parent / "fixtures/native_acp_worker.py"
    for name in ("codex-acp", "opencode"):
        shutil.copyfile(source, bins / name)
        (bins / name).chmod(0o700)
    # Harmless local account files exercise the same required snapshot policy.
    for relative in (".codex/auth.json", ".local/share/opencode/auth.json"):
        auth = daemon.home / relative
        auth.parent.mkdir(parents=True, exist_ok=True)
        auth.write_text('{"fixture":"deterministic ACP peer; no real account"}')
        auth.chmod(0o600)
    # OpenCode v2 layout: the credential db is authoritative over the leftover auth.json.
    store = daemon.home / ".local/share/opencode/opencode.db"
    with sqlite3.connect(store) as connection:
        connection.execute("CREATE TABLE credential (id text PRIMARY KEY, integration_id text, label text NOT NULL, "
                           "value text NOT NULL, connector_id text, method_id text, active integer, "
                           "time_created integer NOT NULL, time_updated integer NOT NULL)")
        expires = int(time.time() + 30 * 86400) * 1000
        connection.executemany("INSERT INTO credential VALUES (?, ?, ?, ?, NULL, NULL, 1, 0, 0)", [
            ("1", "openai", "OpenAI", json.dumps({"type": "oauth", "refresh": "fixture-r", "access": "fixture-a",
                                                    "expires": expires, "metadata": {"accountID": "fixture"}})),
            ("2", "deepseek", "DeepSeek", json.dumps({"type": "key", "key": "fixture-unrelated"})),
        ])
    store.chmod(0o600)
    daemon.env.update({
        "PATH": f"{bins}:/usr/bin:/bin:/usr/sbin:/sbin",
        "TACHI_HOST_PROFILE": "development", "TACHI_ACP_APPROVAL_MODE": "interactive", "DISPATCH_V2_ENABLED": "true",
        "DISPATCH_V2_PLAN_REVIEW": "false", "TACHI_WORKTREES_ROOT": str(root / "worktrees"),
        "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
    })
    return bins


@scenario("dispatch-advisory", "briefing")
def legacy_planner_remains_advisory_without_staff_admission(ctx: Context) -> None:
    for risk in ("low", "medium"):
        ok, payload, raw = ctx.session.call("tachi_task", action="brief", task="plan a bounded feature",
                                            risk=risk, format="json")
        recommendation = payload.get("route_recommendation", {}) if isinstance(payload, dict) else {}
        ctx.check(f"{risk} planning briefing retains legacy advisory", ok
                  and recommendation.get("recommended_profile") == "claude_plan", raw)
    ok, _, raw = ctx.session.call("tachi_staff", action="start", profile="claude_plan",
                                 task="plan a bounded feature", staffing_reason="explicit_user_request")
    ctx.check("legacy planning advice cannot authorize native Staff", not ok
              and "no admitted native Staff Session" in raw, raw)


@scenario("native-session", "staff-native")
def native_session_supervision_and_mailbox(ctx: Context) -> None:
    """Actual MCP/dispatch/process/sandbox/storage chain; ACP peer is deterministic."""
    root = ctx.daemon.home / f"native-session-{ctx.tag}"
    daemon = Daemon(ctx.daemon.tachi, root, free_port())
    repo = make_fixture_repo(root)
    bins = native_fixture(daemon, root)
    # Exercise a logical home alias on every host, including macOS /var paths.
    alias = root / "home-alias"
    alias.symlink_to(daemon.home.resolve(), target_is_directory=True)
    daemon.env["TACHI_HOME"] = str(alias / ".tachi")
    headers = {"X-Tachi-Workspace-Root": str(repo), "X-Tachi-Agent-Identity": f"native-owner-{ctx.tag}"}
    original = (repo / "README.md").read_bytes()

    def read_status(session, dispatch):
        ok, payload, raw = session.call("tachi_staff", action="status", dispatch_id=dispatch, format="json")
        return payload if ok and isinstance(payload, dict) else {"error": raw}

    def wait(session, dispatch, predicate, seconds=25):
        deadline = time.monotonic() + seconds
        status = {}
        while time.monotonic() < deadline:
            status = read_status(session, dispatch)
            if predicate(status):
                return status
            time.sleep(0.1)
        return status

    def idle(session, dispatch, turn=1):
        return wait(session, dispatch, lambda status: find_key(status, "phase") == "awaiting_instruction"
                    and find_key(status, "turn") == turn)

    def revision(status):
        return find_key(status, "status_revision")

    def start(session, task, profile="codex_sol_review", key=None):
        args = dict(action="start", profile=profile, task=task, staffing_reason="explicit_user_request", format="json")
        if key:
            args["submission_id"] = key
        ok, payload, raw = session.call("tachi_staff", **args)
        dispatch = find_key(payload, "dispatch_id") if ok else None
        if not dispatch:
            raise RuntimeError(f"native start failed: {raw}")
        return dispatch

    def events(dispatch):
        path = daemon.app_home / "runs" / dispatch / "events.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def finish(session, dispatch, status):
        ok, payload, raw = session.call("tachi_staff", action="finish", dispatch_id=dispatch,
                                       expected_status_revision=revision(status), format="json")
        return ok, wait(session, dispatch, lambda value: find_key(value, "state") == "TASK_STATE_COMPLETED"), raw

    try:
        daemon.start()
        owner = McpSession(daemon.url, headers)
        stranger = McpSession(daemon.url, {**headers, "X-Tachi-Agent-Identity": f"native-stranger-{ctx.tag}"})
        for profile, worker, label in (("codex_53_fast", None, "retired CLI profile"),
                                       ("codex_sol_review", "opencode", "cross-backend override")):
            args = dict(action="start", profile=profile, task="must refuse", staffing_reason="explicit_user_request")
            if worker:
                args["worker"] = worker
            ok, _, raw = owner.call("tachi_staff", **args)
            ctx.check(f"native admission refuses {label}", not ok, raw)
        ok, _, raw = owner.call("tachi_staff", action="start", profile="codex_sol_review",
                               stage="auto", task="must refuse", staffing_reason="explicit_user_request")
        ctx.check("native profile refuses a stage override", not ok and "stage" in raw, raw)
        key = f"native-{ctx.tag}"
        dispatch = start(owner, "Read the assigned repository and report.", key=key)
        status = idle(owner, dispatch)
        protocol = daemon.app_home / "runs" / dispatch / "runtime/home/protocol.jsonl"
        requests = [json.loads(line) for line in protocol.read_text().splitlines()] if protocol.exists() else []
        expected_home = str(protocol.parent.resolve())
        ctx.require("native start reaches a supervised terminal-turn boundary",
                    find_key(status, "phase") == "awaiting_instruction"
                    and bool(requests) and all(row["home"] == expected_home for row in requests), status)
        session_id = find_key(status, "session_id")
        run_dir = daemon.app_home / "runs" / dispatch
        report = (run_dir / "result.md").read_text()
        ctx.check("server read-only boundary blocks workspace and outside writes",
                  '"cwd_writable": false' in report and '"outside_blocked": true' in report, report)
        ctx.check("model identity records requested resolved and observed facts",
                  find_key(status, "native_session")["observed"] == {"model": "gpt-6.1-sol", "effort": "high"}, status)
        ctx.check("fixed native permission policy ignores the legacy interactive toggle",
                  '"permission_allowed": true' in report, report)
        ctx.check("thought chunks never become the terminal report", "PRIVATE_THOUGHT_SENTINEL" not in report, report)
        raw_events = events(dispatch)
        ctx.check("native negotiation acknowledges exact model before first prompt",
                  [row.get("message", {}).get("method") for row in raw_events if row.get("direction") == "client" and row.get("message", {}).get("method")]
                  == ["initialize", "session/new", "session/set_config_option", "session/set_config_option", "session/prompt"],
                  "recorded client protocol ordering")
        ok, _, raw = stranger.call("tachi_staff", action="continue", dispatch_id=dispatch,
                                  expected_status_revision=revision(status), task="forbidden")
        ctx.check("visibility does not authorize another identity to continue", not ok, raw)
        ok, _, raw = owner.call("tachi_staff", action="continue", dispatch_id=dispatch,
                              expected_status_revision=revision(status) - 1, task="stale")
        ctx.check("native continuation rejects a stale receipt revision", not ok and "stale" in raw, raw)
        ok, _, raw = owner.call("tachi_staff", action="continue", dispatch_id=dispatch,
                              expected_status_revision=revision(status), task="Check the result once more.")
        ctx.check("owning controller accepts a correction as the next turn", ok, raw)
        status = idle(owner, dispatch, 2)
        ctx.check("correction retains the same native session",
                  find_key(status, "turn") == 2 and find_key(status, "session_id") == session_id, status)
        recipient = f"staff:{dispatch}"
        ok, _, raw = owner.call("tachi_a2a", action="send", recipient_agent_identity_id=recipient,
                               subject_ref=f"coordination:native-{ctx.tag}", text="The verification context is ready.",
                               idempotency_key=f"push-{ctx.tag}", ttl_days=1)
        ctx.check("independent harness can address the launch-bound managed identity", ok, raw)
        status = idle(owner, dispatch, 3)
        report = (run_dir / "result.md").read_text()
        ctx.check("managed mailbox pushes a new turn without human relay",
                  find_key(status, "turn") == 3 and '"mailbox": true' in report, report)
        _, mailbox, _ = owner.call("tachi_a2a", action="status")
        ctx.check("managed intake records consumed in the original mailbox",
                  find_key(mailbox, "current_state") == "consumed", mailbox)
        protocol = [row.get("message", {}) for row in events(dispatch) if row.get("direction") == "client"]
        prompts = [item for item in protocol if item.get("method") == "session/prompt"]
        ctx.check("three turns use one initialize and one session creation",
                  sum(item.get("method") == "initialize" for item in protocol) == 1
                  and sum(item.get("method") == "session/new" for item in protocol) == 1
                  and len(prompts) == 3 and all(item["params"]["sessionId"] == session_id for item in prompts), len(prompts))
        ok, final, raw = finish(owner, dispatch, status)
        ctx.check("finish settles the original run after its terminal turn",
                  ok and find_key(final, "state") == "TASK_STATE_COMPLETED", final if ok else raw)
        ok, recovery, raw = owner.call("tachi_staff", action="status", submission_id=key, format="json")
        ctx.check("native submission recovery retains the original association",
                  ok and find_key(recovery, "dispatch_id") == dispatch, recovery if ok else raw)
        ctx.check("native artifacts retain raw events atomic telemetry and full result",
                  all((run_dir / name).is_file() for name in ("events.jsonl", "telemetry.json", "result.md"))
                  and bool(find_key(final, "managed_run_identity")["execution_grant_ref"])
                  and bool(find_key(final, "managed_run_identity")["exec_env_ref"]), str(run_dir))

        held = start(owner, "[hold] Wait at the active prompt.")
        busy = wait(owner, held, lambda value: find_key(value, "phase") == "running")
        ok, _, raw = owner.call("tachi_staff", action="continue", dispatch_id=held,
                              expected_status_revision=revision(busy), task="must not overlap")
        ctx.check("a running prompt refuses overlapping supervision", not ok, raw)
        ok, cancellation, raw = owner.call("tachi_staff", action="cancel", dispatch_id=held,
                                         expected_status_revision=revision(busy), format="json")
        ctx.check("native cancel confirms owned process termination",
                  ok and find_key(cancellation, "receipt") == "cancellation_confirmed"
                  and find_key(cancellation, "backend") == "acp_native", cancellation if ok else raw)
        cancelled = read_status(owner, held)
        ctx.check("native cancellation is terminal in the canonical run",
                  find_key(cancelled, "state") == "TASK_STATE_CANCELED", cancelled)

        auth_failure = start(owner, "[auth-cleanup-fail] [hold] Keep the active turn until cancellation.")
        auth_home = daemon.app_home / "runs" / auth_failure / "runtime/home"
        auth_status = wait(owner, auth_failure, lambda value: (auth_home / ".codex/auth.json").is_dir())
        ok, cancelled_auth, raw = owner.call("tachi_staff", action="cancel", dispatch_id=auth_failure,
                                           expected_status_revision=revision(auth_status), format="json")
        ctx.check("native credential cleanup failure prevents cancellation confirmation",
                  ok and find_key(cancelled_auth, "receipt") == "cancellation_unavailable"
                  and find_key(cancelled_auth, "reason") == "credential_cleanup_failed", cancelled_auth if ok else raw)

        (bins / "mismatch").write_text("do not acknowledge selection")
        mismatch = start(owner, "No task may run with a substituted model.")
        failed = wait(owner, mismatch, lambda value: find_key(value, "state") == "TASK_STATE_FAILED")
        ctx.check("model mismatch fails before any prompt is sent",
                  find_key(failed, "state") == "TASK_STATE_FAILED"
                  and not any(row.get("message", {}).get("method") == "session/prompt" for row in events(mismatch)), failed)
        (bins / "mismatch").unlink()

        long_run = start(owner, "[long] Return the complete result text.", profile="codex_astra_review")
        long_status = idle(owner, long_run)
        ok, final, _ = finish(owner, long_run, long_status)
        long_result = (daemon.app_home / "runs" / long_run / "result.md").read_text()
        ctx.check("terminal result.md preserves output beyond 64 KiB",
                  ok and len(long_result) > 70000 and long_result.endswith("COMPLETE_RESULT_TAIL"), len(long_result))

        writer = start(owner, "Modify only the managed worktree.", profile="opencode_terra_write")
        write_status = idle(owner, writer)
        write_report = (daemon.app_home / "runs" / writer / "result.md").read_text()
        ctx.check("OpenCode native write profile writes only its managed workspace",
                  '"cwd_writable": true' in write_report and '"outside_blocked": true' in write_report
                  and not (repo / "native-probe.txt").exists(), write_report)
        ctx.check("OpenCode v2 credential db snapshots only the pinned provider",
                  '"opencode_auth": ["openai"]' in write_report, write_report)
        ok, _, _ = finish(owner, writer, write_status)
        ctx.check("OpenCode native session finishes through the same controller", ok, writer)
        writer_store = daemon.app_home / "runs" / writer / "runtime/data/opencode"
        residue = sorted(path.name for path in writer_store.glob("*")) if writer_store.exists() else []
        ctx.check("OpenCode credential snapshot and imported store are removed at terminal",
                  not {"auth.json", "opencode.db", "opencode.db-wal", "opencode.db-shm"} & set(residue), residue)

        drift = start(owner, "[drift] Refuse configuration changes during a turn.")
        drift_status = wait(owner, drift, lambda value: find_key(value, "state") == "TASK_STATE_FAILED")
        ctx.check("observed model drift fails the same run without replay",
                  find_key(drift_status, "failure_class") == "identity_mismatch"
                  and sum(row.get("message", {}).get("method") == "session/prompt" for row in events(drift)) == 1,
                  drift_status)
        for field, substituted in (("model", "unrequested-model"), ("effort", "low")):
            restored = start(owner, f"[{field}-drift-restore] Refuse transient identity changes before tool approval.")
            restored_status = wait(owner, restored, lambda value:
                                   find_key(value, "state") == "TASK_STATE_FAILED"
                                   or find_key(value, "phase") == "awaiting_instruction")
            client_messages = [row.get("message", {}) for row in events(restored)
                               if row.get("direction") == "client"]
            projection = find_key(restored_status, "native_session") or {}
            ctx.check(f"transient {field} drift fails before restored config or tool approval",
                      find_key(restored_status, "state") == "TASK_STATE_FAILED"
                      and projection.get("failure_class") == "identity_mismatch"
                      and projection.get("observed", {}).get(field) == substituted
                      and sum(item.get("method") == "session/prompt" for item in client_messages) == 1
                      and not any(str(item.get("id", "")).startswith("permission-") for item in client_messages),
                      restored_status)
            if find_key(restored_status, "phase") == "awaiting_instruction":
                finish(owner, restored, restored_status)
        crashed = start(owner, "[crash] Terminate before terminal output.")
        crash_status = wait(owner, crashed, lambda value: find_key(value, "state") == "TASK_STATE_FAILED")
        ctx.check("adapter exit before a terminal response is a classified failure",
                  find_key(crash_status, "failure_class") == "adapter_exit"
                  and find_key(crash_status, "state") == "TASK_STATE_FAILED", crash_status)

        queued = start(owner, "[hold] Complete this turn before reading the mailbox.")
        queued_home = daemon.app_home / "runs" / queued / "runtime/home"
        wait(owner, queued, lambda value: (queued_home / "turn.json").exists())
        ok, _, raw = owner.call("tachi_a2a", action="send", recipient_agent_identity_id=f"staff:{queued}",
                               subject_ref=f"coordination:queued-{ctx.tag}", text="Context for the following turn.",
                               idempotency_key=f"queued-{ctx.tag}", ttl_days=1)
        active = read_status(owner, queued)
        ctx.check("busy managed session leaves its mailbox message queued",
                  ok and find_key(active, "phase") == "running"
                  and not any(row.get("direction") == "mailbox_intake" for row in events(queued)), raw)
        (queued_home / "release").touch()
        queued_status = idle(owner, queued, 2)
        ctx.check("queued mailbox is consumed only after the active terminal turn",
                  find_key(queued_status, "turn") == 2
                  and sum(row.get("direction") == "mailbox_intake" for row in events(queued)) == 1, queued_status)
        finish(owner, queued, queued_status)

        ceiling = start(owner, "Check the fixed turn ceiling.")
        ceiling_status = idle(owner, ceiling)
        instructions_accepted = True
        for turn in range(2, 5):
            ok, _, _ = owner.call("tachi_staff", action="continue", dispatch_id=ceiling,
                                 expected_status_revision=revision(ceiling_status), task=f"Verification turn {turn}.")
            instructions_accepted &= ok
            ceiling_status = (idle(owner, ceiling, turn) if turn < 4 else
                              wait(owner, ceiling, lambda value: find_key(value, "state") == "TASK_STATE_COMPLETED"))
        ctx.check("fixed turn ceiling completes the fourth turn automatically",
                  instructions_accepted and find_key(ceiling_status, "turn") == 4
                  and find_key(ceiling_status, "state") == "TASK_STATE_COMPLETED"
                  and sum(row.get("message", {}).get("method") == "session/prompt" for row in events(ceiling)) == 4,
                  ceiling_status)
        ok, _, raw = owner.call("tachi_staff", action="continue", dispatch_id=ceiling,
                              expected_status_revision=revision(ceiling_status), task="Forbidden fifth turn.")
        ctx.check("terminal session never accepts an extra turn", not ok, raw)

        managed_identity = McpSession(daemon.url, {**headers, "X-Tachi-Agent-Identity": recipient})
        ok, _, raw = managed_identity.call("tachi_staff", action="start", task="Forbidden recursive launch.",
                                          profile="codex_sol_review", staffing_reason="explicit_user_request")
        ctx.check("managed identity cannot self-dispatch a new worker", not ok and "self-dispatch" in raw, raw)

        guarded = start(owner, "Stay at the boundary while the worker identity tries to complete.")
        guarded_status = idle(owner, guarded)
        guarded_prompt = (daemon.app_home / "runs" / guarded / "prompt.md").read_text()
        ctx.check("native worker prompt leaves lifecycle to the supervisor",
                  "supervised native session" in guarded_prompt
                  and 'Call `tachi_task(action="complete")` when done' not in guarded_prompt, guarded)
        worker_identity = McpSession(daemon.url, {**headers, "X-Tachi-Agent-Identity": f"staff:{guarded}"})
        ok, _, raw = worker_identity.call("tachi_task", action="complete", dispatch_id=guarded, outcome="success",
                                         summary="worker must not settle its own native run")
        still_idle = read_status(owner, guarded)
        ctx.check("native worker identity cannot complete its supervised run",
                  not ok and "supervisor" in raw and find_key(still_idle, "phase") == "awaiting_instruction", raw)
        ok, final, raw = finish(owner, guarded, still_idle)
        ctx.check("supervisor still settles the run after a refused worker completion",
                  ok and find_key(final, "state") == "TASK_STATE_COMPLETED", final if ok else raw)

        broken_projection = start(owner, "Refuse continuation if required telemetry cannot be persisted.")
        projection_status = idle(owner, broken_projection)
        telemetry = daemon.app_home / "runs" / broken_projection / "telemetry.json"
        telemetry.unlink()
        telemetry.mkdir()
        ok, _, raw = owner.call("tachi_staff", action="continue", dispatch_id=broken_projection,
                              expected_status_revision=revision(projection_status), task="Must fail before a second prompt.")
        projection_failed = wait(owner, broken_projection, lambda value: find_key(value, "state") == "TASK_STATE_FAILED")
        ctx.check("partial telemetry publication terminates instead of stranding supervision",
                  not ok and find_key(projection_failed, "state") == "TASK_STATE_FAILED"
                  and sum(row.get("message", {}).get("method") == "session/prompt" for row in events(broken_projection)) == 1,
                  projection_failed if not ok else raw)

        auth_source = daemon.home / ".codex/auth.json"
        auth_source.rename(auth_source.with_suffix(".saved"))
        missing_auth = start(owner, "An absent account must never launch an adapter.")
        no_auth = wait(owner, missing_auth, lambda value: find_key(value, "state") == "TASK_STATE_FAILED")
        ctx.check("missing provider auth snapshot fails before adapter spawn",
                  find_key(no_auth, "failure_class") == "authentication"
                  and not (daemon.app_home / "runs" / missing_auth / "runtime/home/protocol.jsonl").exists(), no_auth)
        auth_source.with_suffix(".saved").rename(auth_source)

        restart = start(owner, "Wait for owner instructions before finishing.")
        before = idle(owner, restart)
        daemon.stop()
        daemon.start()
        owner = McpSession(daemon.url, headers)
        after = read_status(owner, restart)
        ctx.check("restart exposes an indeterminate original run instead of missing",
                  find_key(after, "control_state") == "unavailable"
                  and find_key(after, "execution_state") != "missing", after)
        ok, _, raw = owner.call("tachi_staff", action="continue", dispatch_id=restart,
                              expected_status_revision=revision(before), task="do not redispatch")
        ctx.check("new daemon cannot inherit prior session control", not ok, raw)
        ctx.check("native conformance leaves original repository files unchanged",
                  (repo / "README.md").read_bytes() == original and not (repo / "native-probe.txt").exists()
                  and not (bins / "outside-write").exists(), "original repo and outside path intact")
    finally:
        daemon.stop()


@scenario("health-cohort")
def enrichment_failures_use_the_vector_coverage_cohort(ctx: Context) -> None:
    """Read the real owner status over HTTP with disposable failure metadata."""
    root = ctx.daemon.home / f"health-cohort-{ctx.tag}"
    daemon = Daemon(ctx.daemon.tachi, root, free_port())
    db = daemon.app_home / "global" / "tachi-memory.db"
    ids = {name: str(uuid.uuid4()) for name in ("cache", "ordinary")}
    failure = {"status": "failed", "failed_stage": "summary", "last_error": "cohort fixture"}
    try:
        daemon.start()
        session = McpSession(daemon.url)
        for name, memory_id in ids.items():
            # Plain save (no `kind`): it keeps the caller's id and path, which
            # the seeded-metadata lookups below key on.
            ok, saved, raw = session.call(
                "tachi_memory", action="save", format="json", id=memory_id,
                storage_scope="global", text=f"Cohort {ctx.tag} {name} fixture.",
                path=f"/scratch/dogfood/{ctx.tag}/{name}", metadata={"synthetic_fixture": True},
            )
            ctx.require(f"health cohort seeds {name} through HTTP",
                        ok and find_key(saved, "id") == memory_id, raw)
        daemon.stop()
        for eligible_failed in (False, True):
            # Seed a failure observation, not a fake provider success. The
            # private daemon is stopped; never mutate a live/user database.
            with closing(fixture_connection(db, "rw")) as conn, conn:
                for name, memory_id in ids.items():
                    row = conn.execute("SELECT metadata FROM memories WHERE id = ?", (memory_id,)).fetchone()
                    if row is None:
                        raise RuntimeError("health cohort fixture row disappeared")
                    metadata = json.loads(row[0])
                    metadata.pop("enrichment", None)
                    if name == "cache":
                        metadata["recall_rerank_cache"] = True
                    if name == "cache" or eligible_failed:
                        metadata["enrichment"] = failure
                    changed = conn.execute("UPDATE memories SET metadata = ? WHERE id = ?",
                                           (json.dumps(metadata), memory_id)).rowcount
                    if changed != 1:
                        raise RuntimeError("health cohort fixture update missed its row")
            daemon.start()
            # Use only this disposable daemon's owner capability, bound to
            # its recorded process, port and store. A scoped daemon writes its
            # discovery receipt as `.tachi/daemon-<scope>.pid` (the legacy
            # `daemon.pid` belongs to the unscoped single-DB layout). Never log
            # the capability itself.
            receipts = [json.loads(path.read_text())
                        for path in sorted(daemon.app_home.glob("daemon-*.pid"))]
            info = receipts[0] if len(receipts) == 1 else {}
            if (daemon.process is None or info.get("pid") != daemon.process.pid
                    or info.get("port") != daemon.port
                    or str(info.get("global_db")) != str(db)
                    or not info.get("internal_proxy_token")):
                raise RuntimeError("health cohort owner discovery did not match its daemon")
            owner = McpSession(daemon.url)
            owner.headers.update({"X-Tachi-Owner-Tool": "tachi_status",
                                  "X-Tachi-Internal-Proxy-Token": info["internal_proxy_token"]})
            ok, status, _ = owner.call("tachi_status", format="json")
            actual = status.get("enrichment_failures") if isinstance(status, dict) else None
            served_pid = find_key(status.get("runtime", {}), "pid") if isinstance(status, dict) else None
            ctx.check(
                f"HTTP health counts failed databases with eligible_failure={eligible_failed}",
                ok and served_pid == daemon.process.pid and type(actual) is int
                and actual == int(eligible_failed),
                {"failed_databases": actual, "eligible_failure": eligible_failed},
            )
            daemon.stop()
            with closing(fixture_connection(db, "ro")) as conn:
                retained = {}
                for name, memory_id in ids.items():
                    row = conn.execute("SELECT metadata FROM memories WHERE id = ?", (memory_id,)).fetchone()
                    retained[name] = json.loads(row[0]) if row else {}
            ctx.check(
                f"health observation preserves failure evidence with eligible_failure={eligible_failed}",
                retained["cache"].get("recall_rerank_cache") is True
                and retained["cache"].get("enrichment") == failure
                and retained["ordinary"].get("enrichment") == (failure if eligible_failed else None),
                {name: metadata.get("enrichment") for name, metadata in retained.items()},
            )
    finally:
        daemon.stop()


@scenario("queue-evidence")
def missing_vectors_do_not_claim_enrichment_queue_depth(ctx: Context) -> None:
    """A real save/search and owner CLI distinguish vector debt from queue evidence."""
    root = ctx.daemon.home / f"queue-evidence-{ctx.tag}"
    daemon = Daemon(ctx.daemon.tachi, root, free_port())
    daemon.env["TACHI_SEARCH_DISABLE_QUERY_EMBEDDING"] = "1"
    expected_id = str(uuid.uuid4())
    needle = f"QueueEvidence{ctx.tag}"
    try:
        daemon.start()
        session = McpSession(daemon.url)
        # Plain save (no `kind`): the note-file projection mints its own id,
        # which would decouple the fixture from the id search must return.
        ok, saved, raw = session.call(
            "tachi_memory", action="save", id=expected_id, format="json",
            storage_scope="global", text=f"{needle} records an operator deployment checklist.",
            path=f"/scratch/dogfood/{ctx.tag}/queue-evidence",
            metadata={"synthetic_fixture": True},
        )
        ctx.require("queue evidence fixture is saved through HTTP",
                    ok and find_key(saved, "id") == expected_id, raw)
        ok, found, raw = session.call(
            "tachi_memory", action="search", query=needle, format="json", scope="memory",
            include_test=True, auto_conversation_scope=False, multi_evidence_pack=False,
        )
        quality = find_key(found, "recall_quality")
        quality = quality if isinstance(quality, dict) else {}
        missing = quality.get("vector_missing")
        ctx.check(
            "HTTP search reports the actual vector deficit",
            ok and expected_id in raw and type(missing) is int and missing >= 1,
            {"fixture_found": expected_id in raw, "vector_missing": missing},
        )
        ctx.check(
            "HTTP search does not infer queue depth from missing vectors",
            ok and "pending_enrichment" in quality and quality["pending_enrichment"] is None,
            {"field_present": "pending_enrichment" in quality,
             "pending_enrichment": quality.get("pending_enrichment")},
        )
        snapshot = daemon.cli("status", "--json", cwd=daemon.home)
        payload = json.loads(snapshot.stdout) if snapshot.returncode == 0 else {}
        expected_db = (daemon.app_home / "global" / "tachi-memory.db").resolve()
        rows = [row for row in payload.get("dbs", []) if isinstance(row, dict)
                and row.get("label") == "global" and isinstance(row.get("path"), str)
                and Path(row["path"]).resolve() == expected_db]
        row = rows[0] if len(rows) == 1 else {}
        missing = row.get("vector_missing")
        ctx.check(
            "CLI snapshot preserves null queue evidence and numeric vector debt",
            snapshot.returncode == 0 and len(rows) == 1 and row.get("error") is None
            and type(missing) is int and missing >= 1
            and "pending_enrichment" in row and row["pending_enrichment"] is None,
            {"matching_databases": len(rows), "vector_missing": missing,
             "field_present": "pending_enrichment" in row,
             "pending_enrichment": row.get("pending_enrichment")},
        )
        text = daemon.cli("status", cwd=daemon.home)
        ctx.check("CLI text names unobserved queue depth as unknown",
                  text.returncode == 0 and "pending_enrichment=unknown" in text.stdout,
                  {"exit": text.returncode, "unknown_rendered": "pending_enrichment=unknown" in text.stdout})
    finally:
        daemon.stop()


@scenario("eval-receipts")
def eval_receipts_preserve_requested_identity_and_canonical_replay(ctx: Context) -> None:
    session = ctx.connect(
        **ctx.workspace_headers,
        **{"X-Tachi-Agent-Identity": f"dogfood-eval-{ctx.tag}"},
    )
    secret = "sk-" + "x" * 24  # synthetic scrub discriminator, never a credential
    registration = {
        "frozen_contract_ref": "kckylechen1/Tachi#2237",
        "execution_origin": "host_native_subagent",
        "lifecycle_owner": "host",
        "harness": "dogfood-fixture",
        "native_child_id": f"eval-receipt-{ctx.tag}",
        "requested_profile": "review",
        "requested_model": "openai/gpt-5",
        "requested_agent": f"worker {secret}",
    }
    expected = {
        "requested_profile": "review",
        "requested_model": "openai/gpt-5",
        "requested_agent": "worker [REDACTED]",
    }
    ok, registered, raw = session.call("tachi_agent_eval", action="register", register=registration)
    run_id = registered.get("eval_run_id") if isinstance(registered, dict) else None
    ctx.require("eval register returns a run id", ok and isinstance(run_id, str)
                and bool(run_id), registered)
    ctx.check(
        "eval register returns scrubbed requested fields only",
        all(registered.get(key) == value for key, value in expected.items())
        and secret not in raw and "effective_model" not in registered,
        {key: registered.get(key) for key in expected},
    )
    ok, replayed, _ = session.call("tachi_agent_eval", action="register", register=registration)
    ctx.check("eval register replay returns the same stored receipt", ok and replayed == registered,
              replayed)
    ok, view, _ = session.call("tachi_agent_eval", action="get", get={"eval_run_id": run_id})
    ctx.require("eval get preserves requested receipt fields", ok and isinstance(view, dict)
                and all(view.get(key) == value for key, value in expected.items()), view)
    ctx.check(
        "requested identity never becomes observed or independent",
        "observation" in view and view["observation"] is None
        and view.get("cross_model_independent") is False and view.get("self_eval") is False,
        {key: view.get(key) for key in ("observation", "cross_model_independent", "self_eval")},
    )
    ok, _, raw = session.call(
        "tachi_agent_eval", action="register",
        register={**registration, "requested_model": "different/requested-model"},
    )
    ctx.check("eval registration still refuses changed replay content",
              not ok and "register conflict" in raw.lower(), raw)

    judgment = {
        "eval_run_id": run_id, "actor": "reviewer", "verifier_model": "openai/gpt-5",
        "usefulness": "useful", "evidence_ref": f"dogfood:{ctx.tag}",
        "evidence_usable": True, "used_in_final_claim": False, "human_override": False,
    }
    canonical = f"{run_id}\x01reviewer\x01useful"
    display = f"{run_id}\\u{{1}}reviewer\\u{{1}}useful"
    ok, first, _ = session.call("tachi_agent_eval", action="adjudicate", adjudicate=judgment)
    ctx.require("eval adjudicate separates canonical and readable event keys",
                ok and isinstance(first, dict) and first.get("event_key") == canonical
                and first.get("event_key_display") == display
                and first.get("cross_model_independent") is False, first)
    ok, replayed, _ = session.call(
        "tachi_agent_eval", action="adjudicate", adjudicate={**judgment, "event_key": canonical},
    )
    ctx.check("canonical event key replays the original adjudication", ok and replayed == first,
              replayed)
    ok, _, raw = session.call(
        "tachi_agent_eval", action="adjudicate",
        adjudicate={**judgment, "event_key": canonical, "evidence_ref": "changed-evidence"},
    )
    ctx.check("canonical event key still refuses changed evidence",
              not ok and "event_key replay payload mismatch" in raw.lower(), raw)

    explicit_key = f"literal\\u{{1}}评审\t{ctx.tag}"
    explicit_display = f"literal\\\\u{{1}}评审\\t{ctx.tag}"
    ok, second, _ = session.call(
        "tachi_agent_eval", action="adjudicate", adjudicate={**judgment, "event_key": explicit_key},
    )
    ctx.check(
        "literal escapes and Unicode stay distinct from control separators",
        ok and isinstance(second, dict) and second.get("event_key") == explicit_key
        and second.get("event_key_display") == explicit_display
        and second.get("adjudication_id") != first.get("adjudication_id"),
        second,
    )
    ok, view, _ = session.call("tachi_agent_eval", action="get", get={"eval_run_id": run_id})
    events = view.get("adjudications", []) if isinstance(view, dict) else []
    ctx.check(
        "eval get retains exactly two events after replay and refused corrections",
        ok and len(events) == 2
        and [(event.get("event_key"), event.get("event_key_display")) for event in events]
        == [(canonical, display), (explicit_key, explicit_display)]
        and [event.get("insertion_seq") for event in events] == [1, 2]
        and all(view.get(key) == value for key, value in expected.items())
        and view.get("observation") is None and view.get("cross_model_independent") is False,
        events,
    )
    no_request = {key: value for key, value in registration.items() if key not in expected}
    no_request["native_child_id"] += "-omitted"
    ok, omitted, _ = session.call("tachi_agent_eval", action="register", register=no_request)
    ctx.check("omitted requested fields remain explicit nulls",
              ok and isinstance(omitted, dict)
              and all(key in omitted and omitted[key] is None for key in expected), omitted)


# ── Driver ────────────────────────────────────────────────────────────────────


def selected(only: set[str] | None) -> list[Scenario]:
    if not only:
        return list(SCENARIOS)
    return [s for s in SCENARIOS if only & set(s.tags) or s.name in only]


def run_binary(tachi: Path, chosen: list[Scenario], keep: bool) -> Run:
    run = Run()
    root = Path(tempfile.mkdtemp(prefix="tachi-dogfood-"))
    daemon = Daemon(tachi, root, free_port())
    try:
        run.binary = daemon.cli("--version").stdout.strip() or str(tachi)
        repo = make_fixture_repo(root)
        daemon.start()
        ctx = Context(daemon, repo, run)
        for item in chosen:
            ctx.current = item.name
            start = len(run.checks)
            failures_before = sum(c.status == "FAIL" for c in run.checks)
            try:
                item.run(ctx)
            except ScenarioAborted:
                if sum(c.status == "FAIL" for c in run.checks) == failures_before:
                    ctx.check(f"{item.name} aborted with a recorded failure", False, "abort without a FAIL row")
            except Exception as error:  # a crashed scenario is a failed scenario
                ctx.check(f"{item.name} ran to completion", False, f"{type(error).__name__}: {error}")
            else:
                if any(c.status in ("PASS", "FAIL") for c in run.checks[start:]):
                    run.completed.append(item.name)
                else:
                    ctx.check(f"{item.name} records at least one check", False, "scenario recorded no checks")
    except Exception as error:
        run.checks.append(Check("harness", "harness", "FAIL", f"{type(error).__name__}: {error}"))
    finally:
        daemon.stop()
        if keep:
            run.checks.append(Check("harness", "temp dir kept", "NOTE", str(root)))
        else:
            shutil.rmtree(root, ignore_errors=True)
    return run


def print_run(run: Run) -> None:
    print(f"## {run.binary}")
    width = max((len(c.name) for c in run.checks), default=0)
    for c in run.checks:
        print(f"{c.status:4}  {c.name:<{width}}  {c.evidence}")
    for name in run.completed:
        print(f"SCENARIO {name} completed")
    passed = sum(c.status == "PASS" for c in run.checks)
    failed = sum(c.status == "FAIL" for c in run.checks)
    print(f"{passed} passed, {failed} failed\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tachi", type=Path, help="candidate tachi binary")
    parser.add_argument("--baseline", type=Path, help="currently deployed binary to compare against")
    parser.add_argument("--only", help="comma-separated feature tags or scenario names")
    parser.add_argument("--keep", action="store_true", help="keep temporary homes for inspection")
    parser.add_argument("--list", action="store_true", help="list scenarios and their tags")
    arguments = parser.parse_args()

    if arguments.list:
        for item in SCENARIOS:
            print(f"{item.name:<52} {','.join(item.tags)}")
        return 0
    if not arguments.tachi:
        parser.error("--tachi is required")
    only = {t.strip() for t in arguments.only.split(",")} if arguments.only else None
    chosen = selected(only)
    if not chosen:
        parser.error(f"no scenario matches --only {arguments.only}")

    binaries = [("candidate", arguments.tachi)]
    if arguments.baseline:
        binaries.insert(0, ("baseline", arguments.baseline))
    runs: dict[str, Run] = {}
    for label, path in binaries:
        path = path.expanduser().resolve()
        if not os.access(path, os.X_OK):
            parser.error(f"not executable: {path}")
        runs[label] = run_binary(path, chosen, arguments.keep)
        print(f"[{label}]")
        print_run(runs[label])

    candidate = runs["candidate"].outcome()
    if "baseline" not in runs:
        return 1 if "FAIL" in candidate.values() else 0
    baseline = runs["baseline"].outcome()
    regressions = [n for n, s in baseline.items() if s == "PASS" and candidate.get(n) != "PASS"]
    fixed = [n for n, s in candidate.items() if s == "PASS" and baseline.get(n) != "PASS"]
    print("## candidate vs baseline")
    for name in fixed:
        print(f"FIXED       {name}")
    for name in regressions:
        print(f"REGRESSION  {name}")
    print(f"{len(fixed)} fixed, {len(regressions)} regressed")
    return 1 if regressions else 0


if __name__ == "__main__":
    sys.exit(main())
