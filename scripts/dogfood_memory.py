"""Memory read-boundary HTTP scenarios registered by dogfood_e2e."""

from __future__ import annotations

import datetime as dt
import json
import math
import sqlite3
import subprocess
import time
import urllib.request
from contextlib import closing
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from dogfood_e2e import Context


def register_scenarios(scenario, find_key, make_fixture_repo, Daemon, McpSession, free_port, fixture_connection):
    @scenario("periodic-maintenance")
    def periodic_checkpoint_preserves_readiness_with_an_external_reader(ctx: Context) -> None:
        """Exercise the real timer, runtime writer gate and readiness route."""
        root = ctx.daemon.home / f"periodic-maintenance-{ctx.tag}"
        daemon = Daemon(ctx.daemon.tachi, root, free_port())
        daemon.env.update({"TACHI_WAL_CHECKPOINT_SECS": "30", "MEMORY_GC_ENABLED": "0",
                           "TACHI_SEARCH_DISABLE_QUERY_EMBEDDING": "1"})
        db = daemon.app_home / "global" / "tachi-memory.db"
        marker = f"periodic-{ctx.tag}"
        reader = None
        try:
            daemon.start()
            session = McpSession(daemon.url)
            def save(suffix):
                memory_id = f"{marker}-{suffix}"
                ok, saved, raw = session.call(
                    "tachi_memory", action="save", kind="memory", format="json",
                    id=memory_id, storage_scope="global", path=f"/scratch/{memory_id}",
                    text=f"{memory_id} records a maintenance checkpoint fixture.",
                )
                ctx.require(f"periodic maintenance saves {suffix} through HTTP",
                            ok and find_key(saved, "id") == memory_id, raw)
                return memory_id
            save("before")
            reader = sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True)
            reader.execute("BEGIN")
            reader.execute("SELECT COUNT(*) FROM memories").fetchone()
            after_id = save("after")
            # This row is only an observation marker in the disposable fixture;
            # its deletion proves the actual periodic tick completed.
            with closing(fixture_connection(db, "rw")) as fixture:
                fixture.execute(
                    "INSERT INTO hard_state(namespace,key,value_json,version,created_at,updated_at) "
                    "VALUES ('capture_manifest', ?, ?, 1, ?, ?)",
                    (marker, json.dumps({"expires_at": "2000-01-01T00:00:00Z"}),
                     "2000-01-01T00:00:00Z", "2000-01-01T00:00:00Z"),
                )
                fixture.commit()
            samples, failures, observed = [], [], False
            deadline = time.monotonic() + 45
            while time.monotonic() < deadline:
                started = time.monotonic()
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{daemon.port}/health", timeout=1) as response:
                        ready = json.load(response).get("db_ready") is True
                    if not ready:
                        failures.append("db_ready=false")
                except OSError as error:
                    failures.append(type(error).__name__)
                samples.append((time.monotonic() - started) * 1000)
                with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True)) as observation:
                    observed = observation.execute(
                        "SELECT 1 FROM hard_state WHERE namespace='capture_manifest' AND key=?", (marker,)
                    ).fetchone() is None
                if observed:
                    break
                time.sleep(0.2)
            ctx.require("periodic maintenance tick completed while snapshot was held", observed,
                        {"samples": len(samples), "failures": failures})
            ctx.check("readiness stays responsive through the periodic checkpoint", not failures,
                      {"samples": len(samples), "max_ms": round(max(samples), 2), "failures": failures})
            ctx.check("external reader retains its original snapshot",
                      reader.execute("SELECT 1 FROM memories WHERE id=?", (after_id,)).fetchone() is None,
                      "new HTTP write is outside the original snapshot")
            reader.rollback()
            reader.close()
            reader = None
            ok, found, raw = session.call("tachi_memory", action="get", id=after_id, format="json")
            ctx.check("committed HTTP write survives the busy checkpoint",
                      ok and find_key(found, "id") == after_id, raw)
        finally:
            if reader is not None:
                reader.close()
            daemon.stop()
        ctx.done()

    @scenario("foundry-replay")
    def restart_recovers_a_foundry_backlog_larger_than_the_channel(ctx: Context) -> None:
        """Run a durable crash backlog through the real startup and scheduler workers."""
        root = ctx.daemon.home / f"foundry-replay-{ctx.tag}"
        daemon = Daemon(ctx.daemon.tachi, root, free_port())
        daemon.env.update({"MEMORY_GC_ENABLED": "0", "TACHI_SEARCH_DISABLE_QUERY_EMBEDDING": "1"})
        db = daemon.app_home / "global" / "tachi-memory.db"
        marker = f"foundry-replay-{ctx.tag}"
        memory_id = f"{marker}-memory"
        job_count = 260  # Exceeds the real 256-slot Foundry channel.
        try:
            daemon.start()
            session = McpSession(daemon.url)
            ok, saved, raw = session.call(
                "tachi_memory", action="save", format="json", id=memory_id,
                storage_scope="global", path=f"/scratch/{memory_id}",
                text=f"{marker} records a durable restart recovery checkpoint.",
            )
            ctx.require("replay fixture is saved through HTTP",
                        ok and find_key(saved, "id") == memory_id, raw)
            daemon.stop()
            created = dt.datetime.now(dt.timezone.utc).isoformat()
            # Seed only the stopped disposable daemon's ordinary queue table.
            # Empty, uniquely scoped forget sweeps are provider-free real jobs;
            # different paths give them independent event claims.
            with closing(fixture_connection(db, "rw")) as fixture, fixture:
                fixture.executemany(
                    "INSERT INTO foundry_jobs "
                    "(id,kind,lane,status,target_db,path_prefix,memory_ids,target_agent_id,"
                    "requested_by,metadata,created_at,updated_at) "
                    "VALUES (?,'forget_sweep','reasoning','queued','global',?,'[]',?,?,?,?,?)",
                    [(f"{marker}-{index:04}", f"/scratch/{marker}/{index}", marker, marker,
                      json.dumps({"fixture": marker, "index": index}), created, created)
                     for index in range(job_count)],
                )
            daemon.start()
            states = {}
            deadline = time.monotonic() + 75
            while time.monotonic() < deadline:
                with closing(fixture_connection(db, "ro")) as observation:
                    states = dict(observation.execute(
                        "SELECT status, COUNT(*) FROM foundry_jobs WHERE requested_by=? GROUP BY status",
                        (marker,),
                    ))
                if states == {"completed": job_count}:
                    break
                time.sleep(0.2)
            ctx.require("restart drains the durable tail beyond channel capacity",
                        states == {"completed": job_count}, states)
            with closing(fixture_connection(db, "ro")) as observation:
                tail = observation.execute(
                    "SELECT status FROM foundry_jobs WHERE id=?", (f"{marker}-{job_count - 1:04}",)
                ).fetchone()
            ctx.check("last durable job survives bounded replay", tail == ("completed",), tail)

            receipts = [json.loads(path.read_text())
                        for path in sorted(daemon.app_home.glob("daemon-*.pid"))]
            info = receipts[0] if len(receipts) == 1 else {}
            if (daemon.process is None or info.get("pid") != daemon.process.pid
                    or info.get("port") != daemon.port or str(info.get("global_db")) != str(db)
                    or not info.get("internal_proxy_token")):
                raise RuntimeError("replay owner discovery did not match its daemon")
            owner = McpSession(daemon.url)
            owner.headers.update({"X-Tachi-Owner-Tool": "tachi_status",
                                  "X-Tachi-Internal-Proxy-Token": info["internal_proxy_token"]})
            ok, status, raw = owner.call("tachi_status", format="json")
            jobs = status.get("jobs", {}) if isinstance(status, dict) else {}
            terminal = jobs.get("terminal_history")
            ctx.check("HTTP status retains the recovered terminal queue evidence",
                      ok and type(terminal) is int and terminal >= job_count, jobs)
            session = McpSession(daemon.url)
            ok, found, raw = session.call("tachi_memory", action="get", id=memory_id, format="json")
            ctx.check("HTTP memory remains readable after replay",
                      ok and find_key(found, "id") == memory_id, raw)
        finally:
            daemon.stop()
        ctx.done()

    @scenario("wiki-lint")
    def wiki_lint_keeps_ordered_similarity_and_alert_counts(ctx: Context) -> None:
        """Write actual Wiki pages and exercise owner lint plus ordinary alerts."""
        root = ctx.daemon.home / f"wiki-lint-{ctx.tag}"
        daemon = Daemon(ctx.daemon.tachi, root, free_port())
        daemon.env.update({"MEMORY_GC_ENABLED": "0", "TACHI_SEARCH_DISABLE_QUERY_EMBEDDING": "1"})
        prefix = f"/wiki/lint-{ctx.tag}"
        # Twenty distinct terms: a/b/d have equal frequencies but different
        # text; c shares 19/20, at the strict duplicate threshold boundary.
        terms = "durable queue replay preserves ordered jobs across restart while bounded batches retain pending work until available workers complete every task".split()
        bodies = {"a": terms, "b": list(reversed(terms)),
                  "c": [*terms[:-1], "receipt"], "d": terms[1:] + terms[:1]}
        ids = {}
        try:
            daemon.start()
            session = McpSession(daemon.url)
            for name, words in bodies.items():
                ok, saved, raw = session.call(
                    "tachi_memory", action="save", kind="wiki", format="json", title=f"Queue replay {name}",
                    text=" ".join(words), path=f"{prefix}/{name}", storage_scope="global", force=True,
                )
                ids[name] = find_key(saved, "id")
                ctx.require(f"Wiki lint page {name} is written through HTTP",
                            ok and bool(ids[name]), raw)
            ctx.require("all four Wiki paths retain distinct rows", len(set(ids.values())) == 4, ids)

            receipts = [json.loads(path.read_text())
                        for path in sorted(daemon.app_home.glob("daemon-*.pid"))]
            info = receipts[0] if len(receipts) == 1 else {}
            db = daemon.app_home / "global" / "tachi-memory.db"
            if (daemon.process is None or info.get("pid") != daemon.process.pid
                    or info.get("port") != daemon.port or str(info.get("global_db")) != str(db)
                    or not info.get("internal_proxy_token")):
                raise RuntimeError("Wiki lint owner discovery did not match its daemon")
            owner = McpSession(daemon.url)
            owner.headers.update({"X-Tachi-Owner-Tool": "wiki_lint",
                                  "X-Tachi-Internal-Proxy-Token": info["internal_proxy_token"]})
            ok, lint, raw = owner.call(
                "wiki_lint", path_prefix=prefix, checks=["duplicates"], limit=25,
                include_skill_quality=False, persist_stale=False,
            )
            pairs = lint.get("duplicates") if isinstance(lint, dict) else None
            ctx.require("owner HTTP lint returns duplicate pairs", ok and isinstance(pairs, list), raw)
            expected = [(ids[left], ids[right], f"{prefix}/{left}", f"{prefix}/{right}")
                        for left, right in [("a", "b"), ("a", "d"), ("b", "d")]]
            actual = [(pair.get("left_id"), pair.get("right_id"),
                       pair.get("left_path"), pair.get("right_path")) for pair in pairs]
            expected_similarity = 20 / (math.sqrt(20) * math.sqrt(20))
            ctx.check("lint preserves ordered pairs and the strict duplicate boundary",
                      actual == expected and all(pair.get("similarity") == expected_similarity for pair in pairs),
                      pairs)
            ctx.check("lint skips stale persistence and skill refresh",
                      lint.get("stale_persisted") is False and lint.get("skill_quality") == {"skipped": True},
                      {"stale_persisted": lint.get("stale_persisted"), "skill_quality": lint.get("skill_quality")})
            ok, alerts, raw = session.call("tachi_memory", action="alerts", format="json")
            counts = find_key(alerts, "wiki_counts")
            ctx.check("ordinary HTTP alerts see the same controlled duplicate pairs",
                      ok and isinstance(counts, dict) and counts.get("duplicates") == len(expected), counts)
        finally:
            daemon.stop()
        ctx.done()

    @scenario("memory", "temporal-cue")
    def out_of_range_cues_preserve_save_time_validity(ctx: Context) -> None:
        for name, text in [
            ("tomorrow", "Conversation recorded 9999-12-30: ship tomorrow"),
            ("range", "Role held from 9999-12-31 until 2026-01-01"),
        ]:
            ok, saved, raw = ctx.session.call(
                "tachi_memory", action="save", text=text,
                path=f"/scratch/dogfood/{ctx.tag}/{name}", format="json",
            )
            memory_id = find_key(saved, "id")
            ctx.require(f"{name} note saved", ok and bool(memory_id), raw)
            ok, got, raw = ctx.session.call(
                "tachi_memory", action="get", id=memory_id, format="json",
            )
            ctx.check(
                f"{name} note keeps its save timestamp and has no expiry",
                ok and isinstance(got, dict) and got.get("id") == memory_id and bool(got.get("timestamp"))
                and got.get("valid_from") == got["timestamp"] and got.get("valid_until") is None,
                raw,
            )
        ctx.done()

    @scenario("memory", "ledger-snapshots")
    def retired_ledger_snapshots_stay_out_of_default_recall(ctx: Context) -> None:
        """#2305 finding 3: retired /cards/* lane mirrors and dated
        /agent/checkpoints/* snapshots must not surface in default recall
        (token-dense snapshots crowd out direct matches); a path_prefix opt-in
        retrieves them, and get-by-id keeps working — the boundary is recall,
        not storage."""
        needle = f"LedgerBoundary{ctx.tag}"
        unbound = ctx.connect()
        ok, saved_card, raw = unbound.call(
            "tachi_memory", action="save", id=f"dogfood-card-{ctx.tag}",
            text=f"{needle} retired lane card cargo main dispatch fleet",
            summary="Retired dispatch-ledger card",
            path=f"/cards/dogfood-{ctx.tag}", storage_scope="global", force=True,
        )
        ctx.require("card row saved", ok, raw)
        ok, saved_live, raw = unbound.call(
            "tachi_memory", action="save",
            text=f"{needle} live dispatch decision note",
            summary="Live memory row",
            path=f"/scratch/dogfood/{ctx.tag}/live-note", storage_scope="global",
        )
        ctx.require("live row saved", ok and isinstance(saved_live, dict) and bool(saved_live.get("id")), raw)
        ok, _, raw = unbound.call(
            "tachi_memory", action="search", query=needle,
            content_scope="memory", top_k=10, format="full",
        )
        body = json.dumps(raw, ensure_ascii=False) if not isinstance(raw, str) else raw
        payload = raw if ok else {}
        ctx.check("default recall returns the live row",
                  ok and saved_live.get("id") in body, body[:200])
        ctx.check("default recall hides the retired card",
                  f"dogfood-card-{ctx.tag}" not in body, body[:200])
        ok, _, raw = unbound.call(
            "tachi_memory", action="search", query=needle,
            content_scope="memory", top_k=10, format="full",
            path_prefix=f"/cards/dogfood-{ctx.tag}",
        )
        body = json.dumps(raw, ensure_ascii=False) if not isinstance(raw, str) else raw
        ctx.check("path_prefix opt-in retrieves the card",
                  ok and f"dogfood-card-{ctx.tag}" in body, body[:200])
        ok, got, raw = unbound.call(
            "tachi_memory", action="get", id=f"dogfood-card-{ctx.tag}",
        )
        ctx.check("get by id still reads the card",
                  ok and needle in json.dumps(got, ensure_ascii=False), raw[:200])
        ctx.done()

    @scenario("memory", "checkpoint-freshness")
    def checkpoint_freshness_respects_agent_and_project(ctx: Context) -> None:
        """A dated snapshot can be historical without mutating its saved lifecycle."""
        identity = f"checkpoint-alice-{ctx.tag}"
        alice = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": identity})
        bob = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": f"checkpoint-bob-{ctx.tag}"})
        other_root = ctx.daemon.home / f"checkpoint-other-{ctx.tag}"
        other_root.mkdir()
        other_repo = make_fixture_repo(other_root)
        subprocess.run(["git", "-C", str(other_repo), "remote", "set-url", "origin",
                        "https://github.com/kckylechen1/Dogfood-Checkpoint-Other.git"], check=True)
        foreign = ctx.connect(**{"X-Tachi-Workspace-Root": str(other_repo), "X-Tachi-Agent-Identity": identity})
        today = dt.datetime.now(dt.timezone.utc).date()
        dates = [(today - dt.timedelta(days=days)).isoformat() for days in (4, 3, 2, 1)]
        needle = f"CheckpointBoundary{ctx.tag}"
        saved = {}
        # Save the old observation last. Creation order must not redefine its date.
        for name, session, date in [("new", alice, dates[1]), ("bob", bob, dates[2]),
                                    ("foreign", foreign, dates[3]), ("old", alice, dates[0])]:
            ok, receipt, raw = session.call(
                "tachi_memory", action="checkpoint", format="json",
                path=f"/agent/checkpoints/{date}",
                text=f"{needle}{name} session decision details for {name}",
                summary=f"Checkpoint freshness {name}", storage_scope="project",
            )
            saved[name] = find_key(receipt, "id")
            ctx.require(f"{name} checkpoint saved", ok and bool(saved[name]), raw)

        request = dict(action="search", query=f"{needle}old", content_scope="memory",
                       path_prefix="/agent/checkpoints", top_k=1, format="full",
                       auto_conversation_scope=False, multi_evidence_pack=False)
        ok, result, raw = alice.call("tachi_memory", **request)
        hint = find_key(result, "checkpoint_freshness")
        ctx.require("explicit old checkpoint has a freshness hint", ok and isinstance(hint, dict), raw)
        ctx.check("newer nonmatching checkpoint from the same agent is identified",
                  hint.get("newer_checkpoint_id") == saved["new"]
                  and hint.get("newer_checkpoint_date") == dates[1]
                  and hint.get("read_time_only") is True, hint)
        ctx.check("hint does not claim durable lifecycle supersession",
                  find_key(result, "superseded_by") is None, raw)
        ok, _, raw = alice.call("tachi_memory", **{**request, "format": "markdown"})
        ctx.check("default agent-facing rendering labels the old checkpoint",
                  ok and f"older checkpoint; newer checkpoint: {dates[1]}" in raw, raw)
        ok, result, raw = alice.call("tachi_memory", **{**request, "query": f"{needle}new"})
        ctx.check("other agents and projects cannot supersede this agent's newest checkpoint",
                  ok and find_key(result, "id") == saved["new"]
                  and find_key(result, "checkpoint_freshness") is None, raw)

        ok, briefing, raw = alice.call("tachi_memory", action="briefing", format="json", compact=False)
        recent = find_key(briefing, "recent_checkpoints")
        recent_by_id = {row.get("id"): row for row in recent} if isinstance(recent, list) else {}
        ctx.require("briefing retains historical checkpoint rows", ok and saved["old"] in recent_by_id, raw)
        ctx.check("recency briefing labels older dates without mixing foreign projects",
                  recent_by_id[saved["old"]].get("checkpoint_freshness", {}).get("newer_checkpoint_id") == saved["new"]
                  and saved["foreign"] not in recent_by_id, recent)

        ok, historical, raw = alice.call("tachi_memory", action="get", id=saved["old"], format="json")
        provenance = find_key(historical, "provenance")
        ctx.check("historical get keeps checkpoint text and durable metadata",
                  ok and f"{needle}old" in raw
                  and find_key(historical, "checkpoint_freshness") is None
                  and find_key(historical, "superseded_by") is None, raw)
        ctx.check("checkpoint records the connection identity as attribution",
                  isinstance(provenance, dict) and provenance.get("agent_identity_id") == identity, provenance)
        ctx.done()
