"""Memory read-boundary HTTP scenarios registered by dogfood_e2e."""

from __future__ import annotations

import datetime as dt
import json
import subprocess
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from dogfood_e2e import Context


def register_scenarios(scenario, find_key, make_fixture_repo):
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
