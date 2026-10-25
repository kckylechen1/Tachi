"""Unit tests for eng-checkpoint pure helpers (#2132). No live daemon."""

from __future__ import annotations

import unittest

from scripts.eng_checkpoint import (
    CheckpointSpec,
    build_body,
    build_idempotency_key,
    build_path,
    decide_action,
    extract_prior_key,
    full_repo_from_remote,
    normalize_event,
    parse_meta_from_text,
    repo_short_from_remote,
)


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


if __name__ == "__main__":
    unittest.main()
