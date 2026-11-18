"""Native recall evaluation gates, exercised with the packaged CLI and HTTP."""

from __future__ import annotations

import json
import uuid


def register(scenario, find_key):
    @scenario("recall-eval-receipts")
    def recall_eval_receipts_bind_inputs_and_reject_drift(ctx):
        needle = f"RECEIPT_GATE_{ctx.tag}"
        target = str(uuid.uuid4())
        ok, saved, raw = ctx.session.call(
            "tachi_memory", action="save", id=target, kind="memory", storage_scope="global",
            text=f"{needle} frozen receipt evidence", path=f"/notes/receipt-gate/{ctx.tag}",
        )
        ctx.require("seed receipt evidence", ok and saved.get("id") == target, raw)
        cases = ctx.daemon.app_home / "receipt-cases.json"
        rows = [{"query": needle, "expected_id": target, "label_kind": "reviewed"}]
        cases.write_text(json.dumps(rows))
        baseline = ctx.daemon.app_home / "baseline-run.json"
        candidate = ctx.daemon.app_home / "candidate-run.json"
        ctx.daemon.stop()  # Same frozen copy, with no daemon writer during replay.
        old_disable = ctx.daemon.env.get("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING")
        ctx.daemon.env["TACHI_SEARCH_DISABLE_QUERY_EMBEDDING"] = "1"
        try:
            def replay(output, previous=None):
                args = ["--no-project-db", "eval", "recall", "--cases", str(cases),
                        "--top-k", "3", "--reviewed-only", "--min-mrr", "0.9", "--json", "--report", str(output)]
                if previous is not None:
                    args += ["--compare", str(previous)]
                return ctx.daemon.cli(*args, cwd=ctx.daemon.home)

            for reserved in ("recall_eval.latest.json", "recall_eval.latest.json.tmp"):
                result = replay(ctx.daemon.app_home / "status" / reserved)
                ctx.check(f"receipt rejects reserved status output {reserved}",
                          result.returncode != 0 and "reserved shared-status path" in result.stderr,
                          result.stdout + result.stderr)
            result = replay(baseline)
            ctx.require("baseline receipt succeeds", result.returncode == 0, result.stdout + result.stderr)
            saved_baseline = baseline.read_bytes()
            result = replay(candidate, baseline)
            ctx.require("same controlled inputs can be paired", result.returncode == 0, result.stdout + result.stderr)
            receipt = json.loads(candidate.read_text())
            ctx.check("paired receipt preserves actual case evidence and binary identity",
                      receipt.get("comparison", {}).get("status") == "no_regression_observed"
                      and receipt["replay"]["cases"][0]["returned_ids"] == [target]
                      and len(receipt["build"]["binary_sha256"]) == 64
                      and receipt["replay"]["receipt_context"]["stable"] is True, receipt.get("comparison"))
            ctx.check("receipt file is private", candidate.stat().st_mode & 0o777 == 0o600, oct(candidate.stat().st_mode & 0o777))
            rows[0]["query"] = f"changed_{needle}"
            cases.write_text(json.dumps(rows))
            changed = ctx.daemon.app_home / "changed-run.json"
            result = replay(changed, baseline)
            ctx.check("corpus drift refuses paired scoring",
                      result.returncode != 0 and json.loads(changed.read_text())["comparison"]["status"] == "incompatible", result.stdout)
            result = replay(baseline)
            ctx.check("receipt paths never overwrite prior evidence",
                      result.returncode != 0 and baseline.read_bytes() == saved_baseline, result.stdout)
            shared = (ctx.daemon.app_home / "status/recall_eval.latest.json").read_text()
            ctx.check("private receipt data stays outside shared status", needle not in shared and target not in shared, shared)
        finally:
            if old_disable is None:
                ctx.daemon.env.pop("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", None)
            else:
                ctx.daemon.env["TACHI_SEARCH_DISABLE_QUERY_EMBEDDING"] = old_disable
            ctx.daemon.start()
            ctx._session = None
            ctx._bound = None
        ctx.done()

    @scenario("recall-eval")
    def recall_eval_contracts_fail_closed_and_status_is_private(ctx):
        needle = f"RECALL_GATE_{ctx.tag}"
        target = str(uuid.uuid4())
        ok, saved, raw = ctx.session.call(
            "tachi_memory", action="save", id=target, kind="memory", storage_scope="global",
            text=f"{needle} release validation owner", path=f"/notes/recall-gate/{ctx.tag}",
        )
        ctx.require("seed recall gate evidence", ok and saved.get("id") == target, raw)
        positive = {"query": needle, "expected_id": target, "label_kind": "reviewed"}
        negative = {"query": needle, "expectation": "empty", "label_kind": "reviewed",
                    "path_prefix": f"/notes/absent-{ctx.tag}"}
        weak = {"query": needle, "expected_id": "missing", "auto_captured": True,
                "label_kind": "reviewed"}
        cases = ctx.daemon.app_home / "recall-cases.json"

        def replay(rows):
            cases.write_text(json.dumps(rows))
            result = ctx.daemon.cli(
                "--no-project-db", "eval", "recall", "--cases", str(cases),
                "--top-k", "3", "--min-mrr", "0.9", "--reviewed-only", "--json",
                cwd=ctx.daemon.home,
            )
            try:
                status = json.loads(result.stdout)
            except ValueError:
                ctx.require("recall CLI returns JSON", False, result.stdout + result.stderr)
            return result, status

        result, status = replay([
            positive, negative, weak, {"query": needle, "expected_id": "missing"},
            {**positive, "enabled": False},
        ])
        ctx.require("reviewed gate excludes weak and unspecified labels",
                    result.returncode == 0 and status.get("status") == "ok"
                    and status.get("excluded_by_label") == 2, status)
        current = status.get("current", {})
        ctx.check("negative cases do not inflate positive recall",
                  current.get("positive_case_count") == 1 and current.get("empty_pass_count") == 1
                  and current.get("recall_at_k") == 1.0 and status.get("skipped_rows") == 1
                  and status.get("disabled_rows") == 1, status)

        receipt_path, = ctx.daemon.app_home.glob("daemon-*.pid")
        receipt = json.loads(receipt_path.read_text())
        ctx.require("status uses this isolated daemon's owner receipt",
                    receipt.get("pid") == ctx.daemon.process.pid
                    and receipt.get("port") == ctx.daemon.port, "local daemon identity")
        owner = ctx.connect(**{
            "X-Tachi-Owner-Tool": "tachi_status",
            "X-Tachi-Internal-Proxy-Token": receipt["internal_proxy_token"],
        })
        ok, health, raw = owner.call("tachi_status", format="json")
        shared = health.get("recall_eval", {}) if isinstance(health, dict) else {}
        ctx.check("HTTP status preserves label and skipped-row provenance",
                  ok and shared.get("gate_label_policy") == "reviewed_only"
                  and shared.get("skipped_rows") == 1
                  and shared.get("label_counts", {}).get("reviewed") == 2, shared)
        ctx.check("shared recall status omits private case data",
                  needle not in json.dumps(shared) and target not in json.dumps(shared), shared)

        result, status = replay([positive, {**negative, "path_prefix": "/notes"}])
        ctx.check("negative violation fails despite perfect positive recall",
                  result.returncode != 0 and status.get("status") == "failed"
                  and status.get("current", {}).get("recall_at_k") == 1.0
                  and status.get("current", {}).get("contract_failures") == 1, status)
        result, status = replay([weak])
        ctx.check("empty reviewed selection cannot pass",
                  result.returncode != 0 and status.get("status") == "incomplete", status)
        result, status = replay([{**positive, "expectation": "empty"}])
        ctx.check("contradictory corpus replaces previous status with error",
                  result.returncode != 0 and status.get("status") == "error"
                  and json.loads((ctx.daemon.app_home / "status/recall_eval.latest.json").read_text())
                  .get("status") == "error", status)
        # A DB corpus must not filter malformed IDs into a weaker contract.
        for mode in ("all", "empty"):
            malformed = {**positive, "expectation": mode, "expected_ids": "missing-required-id"}
            if mode == "empty":
                malformed.pop("expected_id")
                malformed["path_prefix"] = f"/notes/absent-{ctx.tag}"
            ok, _, raw = ctx.session.call(
                "tachi_memory", action="save", kind="memory", storage_scope="global", force=True,
                id=f"eval-contract-{ctx.tag}", text="Reviewed recall contract",
                path=f"/eval/contract-{ctx.tag}", metadata={"recall_eval": malformed},
            )
            ctx.require(f"seed malformed DB {mode} case", ok, raw)
            ok, stored, raw = ctx.session.call("tachi_memory", action="get", id=f"eval-contract-{ctx.tag}", format="json")
            metadata = find_key(stored, "metadata")
            ctx.require(f"DB {mode} fixture retains its actual eval contract",
                        ok and find_key(stored, "path") == f"/eval/contract-{ctx.tag}"
                        and isinstance(metadata, dict) and metadata.get("recall_eval") == malformed, raw)
            result = ctx.daemon.cli(
                "--no-project-db", "eval", "recall", "--reviewed-only", "--min-mrr", "0.9", "--json",
                cwd=ctx.daemon.home,
            )
            status = json.loads(result.stdout)
            ctx.check(f"DB {mode} contract cannot discard incorrectly typed expected IDs",
                      result.returncode != 0 and status.get("status") == "error", status)
        ctx.done()


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
        ctx.done()

    @scenario("adjudication-provenance")
    def adjudication_provenance_enforces_admission_corrections_and_knob1(ctx: Context) -> None:
        def seed_dispatch_run(dispatch_id: str) -> str:
            (ctx.daemon.app_home / "runs" / dispatch_id).mkdir(parents=True, exist_ok=True)
            return dispatch_id

        # 1. Unauthenticated caller -> unknown basis, display-only self-witness.
        unauth_did = seed_dispatch_run(f"disp-unauth-{ctx.tag}")
        ok, unauth, raw = ctx.session.call(
            "tachi_task", action="complete",
            task=f"Provenance unauthenticated {ctx.tag}", agent="codex", outcome="success",
            dispatch_id=unauth_did, scope="global", format="full",
            adjudication={"verdict": "accepted", "adjudicator": "alice"},
        )
        unauth_adj = (unauth.get("pipeline", {}) if isinstance(unauth, dict) else {}).get("adjudication", {})
        ctx.require(
            "unauthenticated complete adjudication lands unknown basis",
            ok and unauth_adj.get("recorded") is True
            and unauth_adj.get("adjudication_attribution_basis") == "unknown"
            and unauth_adj.get("evidence_ref_self_witness") is True,
            unauth if ok else raw,
        )

        # 2. Self-asserted local connection -> planned_unconfirmed (non-completing).
        alice_id = f"dogfood-alice-{ctx.tag}"
        alice = ctx.connect(**ctx.workspace_headers, **{"X-Tachi-Agent-Identity": alice_id})
        local_did = seed_dispatch_run(f"disp-local-{ctx.tag}")
        ok, local, raw = alice.call(
            "tachi_task", action="complete",
            task=f"Provenance local {ctx.tag}", agent="codex", outcome="success",
            dispatch_id=local_did, scope="global", format="full",
            adjudication={"verdict": "accepted", "adjudicator": alice_id},
        )
        local_adj = (local.get("pipeline", {}) if isinstance(local, dict) else {}).get("adjudication", {})
        local_outcome_id = local_adj.get("outcome_id")
        local_event_key = local_adj.get("event_key")
        ctx.require(
            "self-asserted local adjudication lands planned_unconfirmed",
            ok and local_adj.get("recorded") is True
            and local_adj.get("adjudication_attribution_basis") == "planned_unconfirmed"
            and bool(local_outcome_id) and bool(local_event_key),
            local if ok else raw,
        )

        # S1 complete MUST reject prior_event_key.
        s1_corr_did = seed_dispatch_run(f"disp-s1corr-{ctx.tag}")
        ok, s1_corr, raw = alice.call(
            "tachi_task", action="complete",
            task=f"Provenance s1 reject corr {ctx.tag}", agent="codex", outcome="success",
            dispatch_id=s1_corr_did, scope="global", format="full",
            adjudication={"verdict": "accepted", "adjudicator": alice_id, "prior_event_key": local_event_key},
        )
        s1_corr_adj = (s1_corr.get("pipeline", {}) if isinstance(s1_corr, dict) else {}).get("adjudication", {})
        ctx.check(
            "S1 complete rejects prior_event_key",
            ok and s1_corr_adj.get("recorded") is False
            and "prior_event_key" in str(s1_corr_adj.get("error", "")),
            s1_corr if ok else raw,
        )

        # Identity mismatch on self-asserted session degrades to unknown.
        ok, mismatch, raw = alice.call(
            "tachi_task", action="adjudicate",
            outcome_id=local_outcome_id, scope="global", format="json",
            adjudication={"verdict": "accepted", "adjudicator": f"dogfood-bob-{ctx.tag}"},
        )
        ctx.check(
            "S2 identity mismatch lands unknown basis",
            ok and find_key(mismatch, "recorded") is True
            and find_key(mismatch, "adjudication_attribution_basis") == "unknown",
            mismatch if ok else raw,
        )

        # S2 explicit correction citing prior_event_key: append, replay, and conflict.
        corr_payload = {"verdict": "rejected", "adjudicator": alice_id, "prior_event_key": local_event_key}
        ok, corr_first, raw = alice.call(
            "tachi_task", action="adjudicate",
            outcome_id=local_outcome_id, scope="global", format="json",
            adjudication=corr_payload,
        )
        corr_key = find_key(corr_first, "event_key")
        corr_id = find_key(corr_first, "adjudication_id")
        ctx.require(
            "S2 correction appends under server-generated correction key",
            ok and find_key(corr_first, "recorded") is True
            and corr_key == f"correction:{local_outcome_id}:{local_event_key}"
            and find_key(corr_first, "adjudication_attribution_basis") == "planned_unconfirmed",
            corr_first if ok else raw,
        )
        ok, corr_replay, raw = alice.call(
            "tachi_task", action="adjudicate",
            outcome_id=local_outcome_id, scope="global", format="json",
            adjudication=corr_payload,
        )
        ctx.check(
            "S2 identical correction replay returns original row",
            ok and find_key(corr_replay, "recorded") is True
            and find_key(corr_replay, "adjudication_id") == corr_id,
            corr_replay if ok else raw,
        )
        ok, corr_conflict, raw = alice.call(
            "tachi_task", action="adjudicate",
            outcome_id=local_outcome_id, scope="global", format="json",
            adjudication={**corr_payload, "verdict": "accepted"},
        )
        ctx.check(
            "S2 conflicting payload on same correction key is rejected",
            ok and find_key(corr_conflict, "recorded") is False
            and "mismatch" in str(find_key(corr_conflict, "error") or "").lower(),
            corr_conflict if ok else raw,
        )

        # 3. S3 mirror adjudicate/get keep cross_model_independent=false (declared_only)
        # even across distinct declared model vendors.
        ok, reg, raw = alice.call(
            "tachi_agent_eval", action="register",
            register={
                "frozen_contract_ref": "kckylechen1/Tachi#2012",
                "execution_origin": "host_native_subagent",
                "lifecycle_owner": "host",
                "harness": "dogfood-fixture",
                "native_child_id": f"prov-mirror-{ctx.tag}",
                "requested_profile": "review",
                "requested_model": "anthropic/claude-sonnet",
                "requested_agent": "worker",
            },
        )
        run_id = reg.get("eval_run_id") if isinstance(reg, dict) else None
        ctx.require("S3 register succeeds", ok and bool(run_id), reg if ok else raw)
        ok, _, raw = alice.call(
            "tachi_agent_eval", action="observe",
            observe={
                "eval_run_id": run_id,
                "terminal_outcome": "success",
                "effective_model": "anthropic/claude-sonnet",
            },
        )
        ctx.require("S3 observe succeeds", ok, raw)
        ok, s3_adj, raw = alice.call(
            "tachi_agent_eval", action="adjudicate",
            adjudicate={
                "eval_run_id": run_id,
                "actor": alice_id,
                "verifier_model": "openai/gpt-5",
                "usefulness": "useful",
                "evidence_usable": True,
                "used_in_final_claim": True,
                "human_override": False,
                "evidence_ref": f"dogfood:{ctx.tag}",
                "rubric": {
                    "contract_correctness": "pass",
                    "evidence_quality": "pass",
                    "safety": "pass",
                    "scope_discipline": "pass",
                    "intervention_burden": "not_assessed",
                    "completion_integrity": "pass",
                    "adjudication_confidence": "high",
                },
            },
        )
        ctx.check(
            "S3 adjudicate caps distinct declared vendors to declared_only and cross_model_independent=false",
            ok and isinstance(s3_adj, dict)
            and s3_adj.get("cross_model_independent") is False
            and s3_adj.get("independence_basis") == "declared_only"
            and s3_adj.get("adjudication_attribution_basis") == "planned_unconfirmed",
            s3_adj if ok else raw,
        )
        ok, s3_get, raw = alice.call("tachi_agent_eval", action="get", get={"eval_run_id": run_id})
        ctx.check(
            "S3 get reports cross_model_independent=false and planned_unconfirmed basis",
            ok and isinstance(s3_get, dict)
            and s3_get.get("cross_model_independent") is False
            and s3_get.get("current_adjudication_attribution_basis") == "planned_unconfirmed",
            s3_get if ok else raw,
        )

        # S4 authority must survive the public read exit, not just the write response.
        s4_did = seed_dispatch_run(f"disp-s4-{ctx.tag}")
        ok, completed, raw = alice.call(
            "tachi_task", action="complete", task=f"Historical ruling {ctx.tag}",
            agent="codex", outcome="success", dispatch_id=s4_did, scope="global", format="full",
            rulings=[{
                "case": f"Historical adjudicator provenance {ctx.tag}",
                "ruling": "A committing caller cannot witness a historical adjudicator.",
                "principles_cited": ["constitution:precedent/provenance"],
                "outcome": "validated", "adjudicator": alice_id,
                "source_refs": [{
                    "relation": "supports", "target_kind": "comment",
                    "target_ref": "kckylechen1/Tachi#2012#issuecomment-42", "comment_id": "42",
                    "updated_at": "2026-07-01T00:00:00Z", "body_hash": f"dogfood-{ctx.tag}",
                }],
            }],
        )
        candidates = completed.get("pipeline", {}).get("precedent_candidate_decomposition", {}).get("recorded", []) if isinstance(completed, dict) else []
        candidate_id = candidates[0].get("id") if candidates else None
        ctx.require("S4 candidate is recorded", ok and bool(candidate_id), completed if ok else raw)
        ok, candidate, raw = alice.call("tachi_memory", action="get", id=candidate_id, format="json")
        ctx.check(
            "S4 public read preserves raw completeness and reports effective authority",
            ok and candidate.get("metadata", {}).get("authority_complete") is True
            and candidate.get("effective_authority_complete") is False
            and candidate.get("adjudication_attribution_basis") == "unknown",
            candidate if ok else raw,
        )
        read_request = dict(
            query=f"Historical adjudicator provenance {ctx.tag}",
            path_prefix="/precedent_candidates/global/", enable_rerank=False,
            auto_conversation_scope=False,
        )
        ok, search, raw = alice.call("tachi_memory", action="search", **read_request)
        matches = [row for section in search.get("sections", []) for row in section.get("rows", [])
                   if row.get("id") == candidate_id] if isinstance(search, dict) else []
        ctx.check(
            "S4 default JSON search preserves derived authority fields",
            ok and len(matches) == 1 and matches[0].get("effective_authority_complete") is False
            and matches[0].get("adjudication_attribution_basis") == "unknown",
            search if ok else raw,
        )
        ok, _, raw = alice.call("tachi_memory", action="search", format="markdown", **read_request)
        ctx.check(
            "S4 Markdown search labels the candidate advisory",
            ok and "[advisory: unknown]" in raw, raw,
        )

        forged_id = f"s4-forged-{ctx.tag}"
        ok, saved, raw = alice.call(
            "tachi_memory", action="save", id=forged_id, storage_scope="project", force=True,
            path=f"/precedent_candidates/project/{forged_id}", format="json",
            text=f"Caller forged provenance cannot establish precedent authority {ctx.tag}.",
            metadata={
                "kind": "precedent_candidate", "adjudicator": alice_id, "authority_complete": True,
                "effective_authority_complete": True, "adjudication_attribution_basis": "acknowledged_overlay",
                "adjudication_receipt": {
                    "subject": {"actor": alice_id, "model": None}, "channel": "verified_admission",
                    "binding": {"admission_id": "caller-invented"}, "captured_at": "2026-07-01T00:00:00Z",
                },
            },
        )
        ctx.require("public forged metadata sample is saved", ok and find_key(saved, "id") == forged_id, saved if ok else raw)
        ok, forged, raw = alice.call("tachi_memory", action="get", id=forged_id, format="json")
        ctx.check(
            "public save cannot mint adjudication provenance",
            ok and forged.get("effective_authority_complete") is False
            and all(key not in forged.get("metadata", {}) for key in (
                "adjudication_receipt", "adjudication_attribution_basis", "effective_authority_complete")),
            forged if ok else raw,
        )
        ok, briefing, raw = alice.call(
            "tachi_memory", action="briefing", format="json", compact=False,
            query=f"Caller forged provenance cannot establish precedent authority {ctx.tag}",
            path_prefix="/precedent_candidates/project/", enable_rerank=False,
            auto_conversation_scope=False,
        )
        matches = [row for row in briefing.get("memories", []) if row.get("id") == forged_id] if isinstance(briefing, dict) else []
        ctx.check(
            "S4 project briefing preserves the derived authority fields",
            ok and len(matches) == 1 and matches[0].get("effective_authority_complete") is False
            and matches[0].get("adjudication_attribution_basis") == "fallback_unreceipted",
            briefing if ok else raw,
        )
        ctx.done()
