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
import datetime as dt
import hashlib
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


# ── Scenario registry ─────────────────────────────────────────────────────────


@dataclass
class Scenario:
    name: str
    tags: tuple[str, ...]
    run: Callable[["Context"], None]


SCENARIOS: list[Scenario] = []


def scenario(*tags: str):
    def register(function: Callable[["Context"], None]):
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


@scenario("claims", "identity")
def two_identities_contend_for_a_scope(ctx: Context) -> None:
    alice = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": f"dogfood-alice-{ctx.tag}"})
    bob = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": f"dogfood-bob-{ctx.tag}"})
    lease = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=10)).strftime("%Y-%m-%dT%H:%M:%SZ")
    claim = dict(
        action="claim", claim_scope=[f"src/dogfood_{ctx.tag}.rs"], claim_mode="writable",
        claim_role="implementer", lease_expires_at=lease, expected_head=ctx.head(),
    )
    ok_a, claim_a, raw_a = alice.call("tachi_task", **claim)
    ctx.check("first claim on a scope succeeds", ok_a, claim_a if ok_a else raw_a)
    ok_b, _, raw_b = bob.call("tachi_task", **claim)
    # Only meaningful once the first claim holds the scope.
    ctx.check(
        "competing claim on the same scope is refused",
        ok_a and not ok_b,
        raw_b if ok_a else "not evaluated: first claim did not succeed",
    )
    ok, status, raw = alice.call("tachi_a2a", action="status")
    ctx.check(
        "a2a status is scoped to the caller's identity",
        ok and find_key(status, "actor_agent_identity_id") == f"dogfood-alice-{ctx.tag}",
        find_key(status, "actor_agent_identity_id") or raw,
    )


@scenario("staff", "binding")
def unbound_session_cannot_launch_a_worker(ctx: Context) -> None:
    ok, _, raw = ctx.connect().call(
        "tachi_staff", action="start", task=f"dogfood {ctx.tag} must refuse",
        staffing_reason="explicit_user_request", profile="codex_53_fast",
    )
    ctx.check("unbound staff start is refused with a binding remedy", not ok and "bind" in raw.lower(), raw)


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
        return
    by_alias = ctx.connect(**{"X-Tachi-Project": REPO_SHORT_NAME.lower()})
    ok, brief, raw = by_alias.call("tachi_memory", action="briefing", format="json")
    binding = find_key(brief, "binding")
    alias_bound = binding.get("session_project") if isinstance(binding, dict) else None
    ctx.check("X-Tachi-Project: <short name> binds the same library", ok and alias_bound == bound,
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
        return
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
        if not ctx.check("legacy recovery fixture is saved through HTTP MCP", ok and bool(saved_id), raw):
            return
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
        if not ctx.check(
            "incomplete legacy copy refuses startup before HTTP admission",
            refused and "incomplete legacy database copy" in refusal_log,
            refusal_log[-600:],
        ):
            return
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
            try:
                item.run(ctx)
            except Exception as error:  # a crashed scenario is a failed scenario
                ctx.check(f"{item.name} ran to completion", False, f"{type(error).__name__}: {error}")
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
