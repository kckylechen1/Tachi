"""Tests for eng-checkpoint and its CLI write-scope contract.

`EngCheckpointPureTests` need no binary and always run in CI.
`EngCheckpointGlobalScopeIntegrationTests` drive a real `tachi` binary in a
temp TACHI_HOME and are skipped unless one is available (`TACHI_BIN`, or
`tachi` on PATH). They never touch the real ~/.tachi.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import urllib.request
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

from scripts.eng_checkpoint import (
    DEFAULT_SCOPE,
    CheckpointSpec,
    build_body,
    build_idempotency_key,
    build_parser,
    build_path,
    build_remember_args,
    decide_action,
    extract_prior_key,
    full_repo_from_remote,
    normalize_event,
    parse_meta_from_text,
    plan_action,
    repo_short_from_remote,
    row_db_scope,
)

REPO_ROOT = Path(__file__).resolve().parent.parent
HELPER = REPO_ROOT / "scripts" / "eng-checkpoint"


class EngCheckpointPureTests(unittest.TestCase):
    def test_normalize_event_aliases(self):
        self.assertEqual(normalize_event("pr_opened"), "opened")
        self.assertEqual(normalize_event("CI_GREEN"), "ci-green")
        self.assertEqual(normalize_event("merged"), "merged")
        self.assertEqual(normalize_event("bench_writeup"), "bench")
        with self.assertRaises(ValueError):
            normalize_event("nope")

    def test_build_path(self):
        self.assertEqual(build_path("tachi", 2130, "opened"), "/eng/tachi/2130/opened")
        self.assertEqual(build_path("Tachi", 1, "ci_green"), "/eng/tachi/1/ci-green")

    def test_idempotency_key(self):
        k1 = build_idempotency_key("kckylechen1/Tachi", 2130, "opened", "abc123")
        k2 = build_idempotency_key("kckylechen1/Tachi", 2130, "opened", "abc123")
        k3 = build_idempotency_key("kckylechen1/Tachi", 2130, "opened", "def456")
        self.assertEqual(k1, k2)
        self.assertNotEqual(k1, k3)
        bench = build_idempotency_key(
            "kckylechen1/Tachi", 2130, "bench", "abc123", result_fingerprint="f1"
        )
        self.assertIn("|f1", bench)

    def test_remote_parsing(self):
        self.assertEqual(
            full_repo_from_remote("git@github.com:kckylechen1/Tachi.git"),
            "kckylechen1/Tachi",
        )
        self.assertEqual(
            repo_short_from_remote("https://github.com/kckylechen1/Tachi.git"),
            "tachi",
        )

    def test_meta_roundtrip_in_body(self):
        spec = CheckpointSpec(
            event="opened",
            pr=2130,
            sha="deadbeefcafebabe",
            title="supersede prior facts",
            body="- wired supersede\n- Full CI green",
            issue=2126,
        )
        body = build_body(spec)
        meta = parse_meta_from_text(body)
        self.assertEqual(meta["eng_event"], "opened")
        self.assertEqual(meta["pr"], 2130)
        self.assertEqual(meta["head_sha"], "deadbeefcafebabe")
        self.assertEqual(meta["idempotency_key"], spec.idempotency_key)
        self.assertGreaterEqual(len(body), 200)

    def test_decide_skip_same_key(self):
        spec = CheckpointSpec(event="opened", pr=9, sha="aaa111")
        prior_text = build_body(spec)
        existing = [{"id": "mem-1", "path": spec.path, "text": prior_text}]
        action, prior = decide_action(existing, spec)
        self.assertEqual(action, "skip")
        self.assertEqual(prior, "mem-1")
        self.assertEqual(extract_prior_key(existing[0]), spec.idempotency_key)

    def test_decide_supersede_newer_sha(self):
        old = CheckpointSpec(event="ci-green", pr=9, sha="oldsha000")
        new = CheckpointSpec(event="ci-green", pr=9, sha="newsha111")
        existing = [{"id": "mem-old", "path": old.path, "text": build_body(old)}]
        action, prior = decide_action(existing, new)
        self.assertEqual(action, "supersede")
        self.assertEqual(prior, "mem-old")

    def test_decide_save_when_empty(self):
        spec = CheckpointSpec(event="merged", pr=9, sha="abc")
        action, prior = decide_action([], spec)
        self.assertEqual(action, "save")
        self.assertIsNone(prior)

    # --- #2135: global write target + legacy project-DB rows -------------

    def test_default_scope_is_global(self):
        self.assertEqual(DEFAULT_SCOPE, "global")
        args = build_parser().parse_args(["--event", "merged", "--pr", "1"])
        self.assertEqual(args.scope, "global")
        opt_out = build_parser().parse_args(
            ["--event", "merged", "--pr", "1", "--scope", "project"]
        )
        self.assertEqual(opt_out.scope, "project")

    def test_remember_args_carry_scope(self):
        spec = CheckpointSpec(event="merged", pr=7, sha="abc")
        args = build_remember_args(spec)
        self.assertEqual(args[:2], ["--no-project-db", "remember"])
        self.assertEqual(args[args.index("--scope") + 1], "global")
        args = build_remember_args(spec, scope="project", supersedes="m1")
        self.assertEqual(args[0], "remember")
        self.assertNotIn("--no-project-db", args)
        self.assertEqual(args[args.index("--scope") + 1], "project")
        self.assertEqual(args[args.index("--supersedes") + 1], "m1")
        with self.assertRaises(ValueError):
            build_remember_args(spec, scope="user")

    def test_row_db_scope(self):
        self.assertEqual(row_db_scope({"db": "global"}), "global")
        self.assertEqual(row_db_scope({"db": "project"}), "project")
        self.assertEqual(row_db_scope({}), "project")
        with tempfile.TemporaryDirectory() as tmp:
            g = Path(tmp) / "g.db"
            g.write_text("")
            self.assertEqual(row_db_scope({"db": str(g)}, g), "global")
            self.assertEqual(row_db_scope({"db": str(Path(tmp) / "p.db")}, g), "project")

    def test_plan_prefers_target_db_row(self):
        old = CheckpointSpec(event="merged", pr=9, sha="old000")
        new = CheckpointSpec(event="merged", pr=9, sha="new111")
        rows = [
            {"id": "g-old", "path": old.path, "text": build_body(old), "db": "global"},
            {"id": "p-new", "path": new.path, "text": build_body(new), "db": "project"},
        ]
        # Global row exists → supersede it, even though a project row matches the key.
        self.assertEqual(plan_action(rows, new, "global"), ("supersede", "g-old", None))

    def test_plan_legacy_project_row_same_key_skips(self):
        spec = CheckpointSpec(event="merged", pr=2133, sha="c692095")
        rows = [{"id": "5cb83f3b", "path": spec.path, "text": build_body(spec), "db": "project"}]
        self.assertEqual(plan_action(rows, spec, "global"), ("skip", "5cb83f3b", "5cb83f3b"))

    def test_plan_legacy_project_row_new_tip_saves_without_cross_db_supersede(self):
        old = CheckpointSpec(event="merged", pr=9, sha="old000")
        new = CheckpointSpec(event="merged", pr=9, sha="new111")
        rows = [{"id": "p-old", "path": old.path, "text": build_body(old), "db": "project"}]
        self.assertEqual(plan_action(rows, new, "global"), ("save", None, "p-old"))

    def test_plan_empty_and_other_paths_save(self):
        spec = CheckpointSpec(event="merged", pr=9, sha="abc")
        other = CheckpointSpec(event="opened", pr=9, sha="abc")
        rows = [{"id": "x", "path": other.path, "text": build_body(other), "db": "global"}]
        self.assertEqual(plan_action([], spec), ("save", None, None))
        self.assertEqual(plan_action(rows, spec), ("save", None, None))


def _tachi_bin() -> str | None:
    explicit = os.environ.get("TACHI_BIN")
    if explicit:
        return explicit if Path(explicit).is_file() else None
    return shutil.which("tachi")


def _terminate_processes_under(root: Path) -> None:
    """Safety net: stop any tachi process whose argv references the temp root."""
    proc_dir = Path("/proc")
    if not proc_dir.is_dir():
        return
    needle = str(root)
    for entry in proc_dir.iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            argv = (entry / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        if any(needle.encode() in a for a in argv) and any(b"tachi" in a for a in argv[:1]):
            try:
                os.kill(int(entry.name), signal.SIGTERM)
            except OSError:
                pass


class _HttpMcp:
    """Minimal streamable-HTTP MCP client (JSON or SSE responses)."""

    def __init__(self, url: str):
        self.url = url
        self.session: str | None = None
        self._id = 0
        self.request("initialize", {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "test-eng-checkpoint", "version": "1"},
        })
        self._post({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _post(self, msg: dict) -> str:
        headers = {"Content-Type": "application/json",
                   "Accept": "application/json, text/event-stream"}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        req = urllib.request.Request(self.url, data=json.dumps(msg).encode(),
                                     headers=headers, method="POST")
        with urllib.request.urlopen(req, timeout=120) as resp:
            self.session = resp.headers.get("Mcp-Session-Id") or self.session
            return resp.read().decode()

    def request(self, method: str, params: dict) -> dict:
        self._id += 1
        raw = self._post({"jsonrpc": "2.0", "id": self._id, "method": method,
                          "params": params}).strip()
        if raw.startswith("{"):
            return json.loads(raw)
        for line in raw.splitlines():
            if line.startswith("data:") and line[5:].strip():
                msg = json.loads(line[5:].strip())
                if msg.get("id") == self._id:
                    return msg
        raise RuntimeError(f"no response to {method}: {raw[:500]}")


class _StdioMcp:
    """Minimal newline-delimited JSON-RPC client for `tachi serve` over stdio."""

    def __init__(self, cmd: list[str], env: dict[str, str], cwd: str):
        self.proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            env=env,
            cwd=cwd,
        )
        self._id = 0
        self.request(
            "initialize",
            {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "test-eng-checkpoint", "version": "1"},
            },
        )
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, msg: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def request(self, method: str, params: dict) -> dict:
        self._id += 1
        self._send({"jsonrpc": "2.0", "id": self._id, "method": method, "params": params})
        assert self.proc.stdout is not None
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError(f"tachi serve exited before answering {method}")
            line = line.strip()
            if not line.startswith("{"):
                continue
            msg = json.loads(line)
            if msg.get("id") == self._id:
                return msg

    def close(self) -> None:
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
            self.proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


@unittest.skipUnless(_tachi_bin(), "needs a tachi binary (TACHI_BIN or PATH)")
@unittest.skipIf(sys.platform == "win32", "POSIX shell wrapper")
class EngCheckpointGlobalScopeIntegrationTests(unittest.TestCase):
    """#2135: a helper write is visible to an unscoped `--no-project-db` search."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="eng-ckpt-2135-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.addCleanup(_terminate_processes_under, self.tmp)
        self.home = self.tmp / "home"
        self.tachi_home = self.home / ".tachi"
        self.repo = self.tmp / "repo"
        self.home.mkdir()
        self.repo.mkdir()
        self.bin = str(Path(_tachi_bin()).resolve())
        env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("TACHI_", "MEMORY_", "VOYAGE_"))
        }
        env.update(
            HOME=str(self.home),
            TACHI_HOME=str(self.tachi_home),
            TACHI_BIN=self.bin,
            GIT_CONFIG_NOSYSTEM="1",
        )
        self.env = env
        self.git("init", "-q")
        self.git("remote", "add", "origin", "https://github.com/acme/widget.git")
        self.git("-c", "user.email=t@example.com", "-c", "user.name=t",
                 "commit", "-q", "--allow-empty", "-m", "init")
        # Workspace project DB, created the normal way (default project scope).
        (self.repo / ".tachi").mkdir()
        seed = self.tachi("remember", "--path", "/notes/seed",
                          "seed note that creates the workspace project database for this test fixture",
                          extra_env={"TACHI_DISABLE_AUTO_DAEMON": "1"})
        self.assertEqual(seed.returncode, 0, seed.stderr)
        self.project_db = self.repo / ".tachi" / "tachi-memory.db"
        self.global_db = self.tachi_home / "global" / "tachi-memory.db"
        self.assertTrue(self.project_db.exists())
        self.assertTrue(self.global_db.exists())

    def git(self, *args: str) -> None:
        subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                       capture_output=True)

    def tachi(self, *args: str, extra_env: dict[str, str] | None = None
              ) -> subprocess.CompletedProcess[str]:
        env = dict(self.env, **(extra_env or {}))
        return subprocess.run([self.bin, *args], cwd=self.repo, env=env,
                              text=True, capture_output=True, timeout=120)

    def helper(self, sha: str) -> dict:
        proc = subprocess.run(
            [str(HELPER), "--event", "merged", "--pr", "1", "--sha", sha,
             "--title", "widget fix", "--agent-seat", "test"],
            cwd=self.repo, env=self.env, text=True, capture_output=True, timeout=120,
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        return json.loads(proc.stdout)

    def eng_rows(self, db: Path) -> list[tuple[str, str]]:
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        try:
            return conn.execute(
                "SELECT id, path FROM memories WHERE path LIKE '/eng/%' ORDER BY timestamp"
            ).fetchall()
        finally:
            conn.close()

    daemon_url: str | None = None
    daemon_project_db: Path | None = None

    def unscoped_search(self) -> dict:
        args = {"action": "search", "query": "PR 1 merged widget fix",
                "path_prefix": "/eng/widget/", "top_k": 5}
        if self.daemon_url:
            # The global-only daemon holds the store; ask it over MCP HTTP.
            resp = _HttpMcp(self.daemon_url).request(
                "tools/call", {"name": "tachi_memory", "arguments": args})
            self.assertNotIn("error", resp, resp)
            return json.loads(resp["result"]["content"][0]["text"])
        # No daemon in this posture: serve the global DB directly over stdio.
        serve_env = dict(self.env, TACHI_DISABLE_AUTO_DAEMON="1",
                         TACHI_DISABLE_STDIO_PROXY="1")
        mcp = _StdioMcp(
            [self.bin, "--no-project-db", "--global-db", str(self.global_db), "serve"],
            serve_env, str(self.tmp),
        )
        try:
            resp = mcp.request("tools/call", {"name": "tachi_memory", "arguments": args})
        finally:
            mcp.close()
        self.assertNotIn("error", resp, resp)
        return json.loads(resp["result"]["content"][0]["text"])

    def start_daemon(self, *, project_db: Path | None = None) -> None:
        log_path = self.tmp / "daemon.log"
        log = open(log_path, "w")
        self.addCleanup(log.close)
        binding_args = (["--project-db", str(project_db)] if project_db
                        else ["--no-project-db"])
        daemon = subprocess.Popen(
            [self.bin, "--daemon", "--port", "0", *binding_args,
             "--global-db", str(self.global_db), "--gc-enabled", "false"],
            cwd=str(self.tmp), env=self.env, stdout=log, stderr=subprocess.STDOUT,
        )

        def stop() -> None:
            daemon.terminate()
            try:
                daemon.wait(timeout=20)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()

        self.addCleanup(stop)
        deadline = time.time() + 60
        while time.time() < deadline:
            if daemon.poll() is not None:
                self.fail(f"daemon exited early: {log_path.read_text()}")
            m = re.search(r"listening on (http://127\.0\.0\.1:\d+)", log_path.read_text())
            if m:
                self.daemon_url = m.group(1) + "/mcp"
                self.daemon_project_db = project_db
                return
            time.sleep(0.25)
        self.fail(f"daemon never started listening: {log_path.read_text()}")

    def assert_direct_remember_scope(
        self,
        *,
        scope: str | None = "global",
        project: str | None = None,
        expected_db: Path | None = None,
    ) -> None:
        """Check placement without the helper's old-binary dual-flag recipe.

        The Rust dispatch matrix separately makes any local fallback fatal.
        """
        path = "/eng/widget/1/direct-scope"
        args = ["remember", "--path", path, "--category", "handoff", "--force",
                "PR 1 merged widget fix: explicit CLI storage scope must survive daemon forwarding."]
        if scope is not None:
            args.extend(["--scope", scope])
        if project is not None:
            args.extend(["--project", project])
        proc = self.tachi(*args)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertNotIn("executing in-process", proc.stderr)
        self.assertNotIn("falling back to in-process", proc.stderr)
        if self.daemon_url and self.daemon_project_db != self.project_db:
            self.assertIn("daemon project DB scope differs; forwarding via named project",
                          proc.stderr)
        saved_id = json.loads(proc.stdout)["id"]
        target = expected_db or self.global_db
        for db in (self.global_db, self.project_db):
            with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
                ids = [row[0] for row in conn.execute(
                    "SELECT id FROM memories WHERE path = ?", (path,)
                )]
            self.assertEqual(ids, [saved_id] if db == target else [], str(db))
        if target == self.global_db:
            payload = self.unscoped_search()
            hits = [row for section in payload.get("sections", [])
                    for row in section.get("rows", [])]
            found = [row for row in hits if row.get("id") == saved_id]
            self.assertTrue(found, json.dumps(payload)[:2000])
            self.assertEqual(found[0].get("db"), "global")

    def run_flow(self) -> None:
        first = self.helper("aaaa1111")
        self.assertEqual(first["action"], "save", first)
        first_id = first["id"]

        # Row lives in global; the workspace project DB has no /eng row.
        self.assertEqual([r[0] for r in self.eng_rows(self.global_db)], [first_id])
        self.assertEqual(self.eng_rows(self.project_db), [])

        # Unscoped, global-only MCP search (box serve posture) finds it.
        payload = self.unscoped_search()
        self.assertTrue(payload["binding"]["single_db_mode"], payload["binding"])
        hits = [r for sec in payload.get("sections", []) for r in sec.get("rows", [])]
        found = [r for r in hits if r.get("id") == first_id]
        self.assertTrue(found, json.dumps(payload)[:2000])
        self.assertEqual(found[0].get("db"), "global")

        # Identical event + sha -> skip, still one row.
        second = self.helper("aaaa1111")
        self.assertEqual(second["action"], "skip", second)
        self.assertEqual(second["id"], first_id)
        self.assertEqual(len(self.eng_rows(self.global_db)), 1)

        # New tip -> supersede the global row (#2126 semantics), still in global.
        third = self.helper("bbbb2222")
        self.assertEqual(third["action"], "supersede", third)
        self.assertEqual(third["prior_id"], first_id)
        self.assertEqual(
            sorted(r[0] for r in self.eng_rows(self.global_db)),
            sorted([first_id, third["id"]]),
        )
        self.assertEqual(self.eng_rows(self.project_db), [])
        conn = sqlite3.connect(f"file:{self.global_db}?mode=ro", uri=True)
        try:
            superseded_by = conn.execute(
                "SELECT superseded_by FROM memories WHERE id = ?", (first_id,)
            ).fetchone()[0]
        finally:
            conn.close()
        self.assertEqual(superseded_by, third["id"])

    def test_in_process_write_lands_in_global_and_is_searchable(self):
        self.env["TACHI_DISABLE_AUTO_DAEMON"] = "1"
        self.run_flow()

    def test_write_lands_in_global_while_global_only_daemon_runs(self):
        self.start_daemon()
        self.run_flow()

    def test_explicit_global_remember_uses_global_only_daemon(self):
        self.start_daemon()
        self.assert_direct_remember_scope()

    def test_explicit_global_remember_uses_project_bound_daemon(self):
        self.start_daemon(project_db=self.project_db)
        self.assert_direct_remember_scope()

    def test_explicit_global_remember_uses_differently_bound_daemon(self):
        other_db = self.tachi_home / "projects" / "other-scope" / "tachi-memory.db"
        seed = self.tachi(
            "--project-db", str(other_db), "remember", "--path", "/notes/other-seed",
            "An unrelated project library for scope-routing isolation checks.",
            extra_env={"TACHI_DISABLE_AUTO_DAEMON": "1"},
        )
        self.assertEqual(seed.returncode, 0, seed.stdout + seed.stderr)
        self.start_daemon(project_db=other_db)
        self.assert_direct_remember_scope()
        self.assertEqual(self.eng_rows(other_db), [])

    def test_explicit_global_remember_in_process(self):
        self.env["TACHI_DISABLE_AUTO_DAEMON"] = "1"
        self.assert_direct_remember_scope()

    def test_default_project_remember_stays_project(self):
        self.start_daemon()
        self.assert_direct_remember_scope(scope=None, expected_db=self.project_db)

    def test_explicit_project_with_global_scope_stays_project(self):
        aliases = [entry.name for entry in (self.tachi_home / "projects").iterdir()
                   if (entry / "tachi-memory.db").resolve() == self.project_db.resolve()]
        self.assertTrue(aliases, "fixture workspace must have a named-project alias")
        self.start_daemon()
        self.assert_direct_remember_scope(project=sorted(aliases)[0], expected_db=self.project_db)

    def test_legacy_project_row_with_same_key_is_skipped(self):
        self.env["TACHI_DISABLE_AUTO_DAEMON"] = "1"
        legacy = subprocess.run(
            [str(HELPER), "--event", "merged", "--pr", "1", "--sha", "aaaa1111",
             "--title", "widget fix", "--agent-seat", "test", "--scope", "project"],
            cwd=self.repo, env=self.env, text=True, capture_output=True, timeout=120,
        )
        self.assertEqual(legacy.returncode, 0, legacy.stdout + legacy.stderr)
        legacy_id = json.loads(legacy.stdout)["id"]
        self.assertEqual([r[0] for r in self.eng_rows(self.project_db)], [legacy_id])

        again = self.helper("aaaa1111")
        self.assertEqual(again["action"], "skip", again)
        self.assertEqual(again["legacy_prior_id"], legacy_id)
        self.assertEqual(self.eng_rows(self.global_db), [])

        newer = self.helper("bbbb2222")
        self.assertEqual(newer["action"], "save", newer)
        self.assertEqual(newer["legacy_prior_id"], legacy_id)
        self.assertEqual([r[0] for r in self.eng_rows(self.global_db)], [newer["id"]])
        self.assertEqual([r[0] for r in self.eng_rows(self.project_db)], [legacy_id])

if __name__ == "__main__":
    unittest.main()
