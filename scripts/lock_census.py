#!/usr/bin/env python3
"""Lock-census regeneration and validation for #1096 Leaf-0 (#1476).

Subcommands:
  regen     — discover every live ``global_test_lock`` callsite, classify
              ``env_role`` by inspecting the constructor/function under test,
              preserve historical Leaf-0 evidence (matched by file + function
              name, not nearest line), and emit the refreshed JSON artifact.
              Run manually after the callsite set changes.
  validate  — re-derive the live callsite set and diff it against the
              committed fixture.  Exits nonzero on missing/stale/duplicate/
              invalid-enum/count drift.  Does NOT rewrite the fixture.

The classification heuristic is structural and deterministic: it reads the
enclosing function body and pattern-matches for env-reading constructors
(``from_env``, ``LlmClient::new``, ``tachi_home``, ``model_lanes_json`` …)
versus injection constructors (``new_with_config``,
``new_with_home_for_test``, ``make_server_with_temp_home`` …).

``make_server()`` is intentionally NOT an env-reading pattern — it is a
generic test-server/temp-DB factory, not proof that env parsing is the
behavior under test.

``unknown`` is emitted when neither family is found — never inferred from
env-var names alone (Refs #1476).
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

# -- constants --------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_FIXTURE = REPO_ROOT / "docs/engineering/receipts/1096-leaf0-global-test-lock-baseline.json"
ARCHIVE_FALLBACK = Path.home() / ".cache/sigil-eval-archive/1096-leaf0-global-test-lock-baseline.json"

VALID_ENV_ROLES = {"behavior_under_test", "incidental_delivery", "mixed", "unknown"}
VALID_DELETION_SCOPES = {"none", "1319"}

# #1319 contraction-umbrella deletion targets — ONLY source paths directly
# authorized for physical deletion by #1319:
#   - Shell tests (Leaf B deletes shell_ops)
#   - Arena tests (Leaf D deletes arena_ops)
#   - Task-dispatch facade tests (Leaf C deletes the typed task dispatch action)
# Surviving dispatch kernel (dispatch_ops/dispatch/), prompt, bootstrap/serve,
# and cli_tool/tool_dispatch are NOT blanket-marked — their roots survive #1319.
DELETION_PATH_FRAGMENTS = [
    "tests/dispatch_tests/",
    "/arena_ops/",
    "/shell_ops/",
]

# Env-reading constructors: calling these in the function body is evidence that
# the test's contract is env parsing / default resolution / fail-closed.
#
# NOTE: bare ``make_server()`` is deliberately ABSENT.  It is a generic
# test-server/temp-DB factory (creates a MemoryServer from the default home),
# not proof that env is the behavior under test.  A test calling make_server()
# to get a server instance, while mutating env for unrelated setup, must NOT
# be classified behavior_under_test on that basis alone.
ENV_READING_PATTERNS = [
    re.compile(r"\bfrom_env\s*\(\s*\)"),
    re.compile(r"\bLlmClient::new\s*\("),         # bare new(), not new_with_*
    re.compile(r"\btachi_home\s*\("),
    re.compile(r"\bcollect_api_key_status_from_sources\s*\("),
    re.compile(r"\bcollect_api_key_status\s*\("),
    re.compile(r"\bmodel_lanes_json\s*\("),
    re.compile(r"\bcollect_config_env_values\s*\("),
]

# Injection constructors: calling these is evidence that env mutation is
# incidental config delivery to a runtime path with an existing seam (#1117/#1122).
INJECTION_PATTERNS = [
    re.compile(r"\bnew_with_config\s*\("),
    re.compile(r"\bnew_with_home_for_test\s*\("),
    re.compile(r"\bnew_with_migration_authority_and_home\s*\("),
    re.compile(r"\bmake_server_with_temp_home\s*\("),
    re.compile(r"\bnew_with_vault_db\s*\("),
]

# Env-var mutation extraction: capture env-var names from mutation calls.
# Covers EnvRestore::set/unset/remove/set_path and std::env::set_var/remove_var,
# for both quoted-string and bare-const first arguments.
ENV_SET_QUOTED_RE = re.compile(
    r'(?:EnvRestore::(?:set|unset|remove|set_path)|std::env::(?:set_var|remove_var))'
    r'\s*\(\s*"([A-Z_][A-Z0-9_]*)"'
)
ENV_SET_BARE_RE = re.compile(
    r'(?:EnvRestore::(?:set|unset|remove|set_path))\s*\(\s*([A-Z_]{3,}[A-Z0-9_]*)\s*[,)]'
)

FN_NAME_RE = re.compile(r"\b(?:async\s+)?fn\s+(\w+)")


# -- census discovery -------------------------------------------------------

def run_rg(pattern: str) -> str:
    """Run rg in REPO_ROOT and return stdout."""
    result = subprocess.run(
        ["rg", "-n", pattern, "crates/", "--type", "rust"],
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
    )
    return result.stdout


def discover_raw_hits() -> list[dict]:
    """Return every rg hit for ``global_test_lock()`` with file/line/content."""
    stdout = run_rg(r"global_test_lock\(\)")
    hits = []
    for line in stdout.strip().split("\n"):
        if not line:
            continue
        parts = line.split(":", 2)
        if len(parts) < 3:
            continue
        hits.append({"file": parts[0], "line": int(parts[1]), "content": parts[2]})
    return hits


def classify_raw_hits(hits: list[dict]) -> tuple[list, list, list, list]:
    """Split raw hits into definitions, re-exports, doc-refs, and callsites."""
    definitions, reexports, docrefs, callsites = [], [], [], []
    for h in hits:
        stripped = h["content"].strip()
        if "fn global_test_lock" in stripped:
            definitions.append({"file": h["file"], "line": h["line"], "kind": "fn_definition"})
        elif stripped.startswith("pub(crate) use ") or stripped.startswith("use "):
            reexports.append({"file": h["file"], "line": h["line"], "kind": "re_export"})
        elif stripped.startswith("//"):
            docrefs.append(f"{h['file']}:{h['line']} ({stripped[:80]})")
        else:
            callsites.append({"file": h["file"], "line": h["line"]})
    return definitions, reexports, docrefs, callsites


# -- function-body analysis -------------------------------------------------

def find_enclosing_function(file_path: Path, callsite_line: int) -> tuple[str, list[str]]:
    """Return (fn_name, body_lines) for the function enclosing the callsite.

    Scans backwards for the nearest ``fn`` declaration, then uses brace
    matching to bound the body to exactly the current function — preventing
    overflow into sibling test functions that would cause false ``mixed``
    classifications.
    """
    try:
        lines = file_path.read_text(encoding="utf-8").split("\n")
    except (OSError, UnicodeDecodeError):
        return ("unknown", [])

    fn_line_idx = None
    fn_name = "unknown"
    for idx in range(min(callsite_line - 1, len(lines) - 1), -1, -1):
        m = FN_NAME_RE.search(lines[idx])
        if m:
            fn_line_idx = idx
            fn_name = m.group(1)
            break

    if fn_line_idx is None:
        return ("unknown", [])

    depth = 0
    started = False
    end_idx = fn_line_idx
    for idx in range(fn_line_idx, min(fn_line_idx + 200, len(lines))):
        for ch in lines[idx]:
            if ch == "{":
                depth += 1
                started = True
            elif ch == "}":
                depth -= 1
                if started and depth == 0:
                    end_idx = idx
                    break
        if started and depth == 0:
            end_idx = idx
            break
    else:
        end_idx = min(fn_line_idx + 120, len(lines) - 1)

    body = lines[fn_line_idx : end_idx + 1]
    return (fn_name, body)


def classify_env_role(body_lines: list[str]) -> tuple[str, str]:
    """Classify env_role by pattern-matching the function body.

    Returns (env_role, evidence_string).
    """
    body_text = "\n".join(body_lines)

    env_reading_found = []
    for pat in ENV_READING_PATTERNS:
        m = pat.search(body_text)
        if m:
            env_reading_found.append(m.group(0).strip())

    injection_found = []
    for pat in INJECTION_PATTERNS:
        m = pat.search(body_text)
        if m:
            injection_found.append(m.group(0).strip())

    has_env = bool(env_reading_found)
    has_inj = bool(injection_found)

    if has_env and has_inj:
        role = "mixed"
    elif has_env:
        role = "behavior_under_test"
    elif has_inj:
        role = "incidental_delivery"
    else:
        role = "unknown"

    parts = []
    if env_reading_found:
        parts.append("env-reading constructors: " + ", ".join(sorted(set(env_reading_found))))
    if injection_found:
        parts.append("injection constructors: " + ", ".join(sorted(set(injection_found))))
    if not parts:
        parts.append("no env-reading or injection constructor detected in enclosing function body")

    return (role, "; ".join(parts))


def extract_env_vars(body_lines: list[str]) -> list[str]:
    """Extract env-var names mutated in the function body (re-derived, not preserved)."""
    body_text = "\n".join(body_lines)
    found: set[str] = set()
    for m in ENV_SET_QUOTED_RE.finditer(body_text):
        found.add(m.group(1))
    for m in ENV_SET_BARE_RE.finditer(body_text):
        name = m.group(1)
        if name not in {"true", "false", "None", "Some"}:
            found.add(name)
    return sorted(found)


def deletion_scope_for(file_path: str) -> str:
    """Return '1319' if the file is in a #1319 deletion-scope path, else 'none'."""
    return "1319" if any(frag in file_path for frag in DELETION_PATH_FRAGMENTS) else "none"


# -- historical evidence mapping -------------------------------------------

def load_committed_prior(fixture_path: Path) -> dict | None:
    """Load the committed prior fixture (preferred historical source)."""
    if fixture_path.exists():
        try:
            return json.loads(fixture_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            pass
    return None


def load_archive() -> dict | None:
    """Load the original Leaf-0 archive (fallback historical source)."""
    if ARCHIVE_FALLBACK.exists():
        try:
            return json.loads(ARCHIVE_FALLBACK.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            pass
    return None


def is_placeholder_evidence(evidence: str) -> bool:
    """True if the evidence was auto-generated by regen rather than hand-audited.

    Placeholder evidence starts with ``"structural inspection of "`` — the
    exact string emitted by regenerate() when no historical source matched.
    Owner-audited evidence (from the original Leaf-0 archive) is never a
    placeholder.
    """
    return evidence.startswith("structural inspection of ")


def match_historical(
    callsite: dict,
    fn_name: str,
    historical_sources: list[dict],
) -> tuple[dict | None, str]:
    """Match by (file, test_or_fn_name) across ALL historical sources.

    For each source, find the best candidate (same file + same fn_name,
    nearest line to disambiguate within the same function — never
    cross-function).  Among candidates from multiple sources:

    - **Prefer non-placeholder evidence over placeholder evidence.**  This
      prevents a committed-prior structural placeholder (generated by a
      previous regen) from shadowing richer owner-audited archive evidence
      for the same function.
    - If both sources have non-placeholder evidence, prefer committed_prior
      (it may contain manual edits from a prior review round).
    - If all candidates are placeholders, use committed_prior (first found).

    Returns (matched_entry_or_None, source_label).
    """
    f = callsite["file"]
    candidates: list[tuple[dict, str]] = []
    for idx, source in enumerate(historical_sources):
        if not source:
            continue
        label = "committed_prior" if idx == 0 else "archive_fallback"
        same_fn = [
            c for c in source.get("callsites", [])
            if c.get("file") == f and c.get("test_or_fn_name") == fn_name
        ]
        if same_fn:
            best = min(same_fn, key=lambda c: abs(c.get("line", 0) - callsite["line"]))
            candidates.append((best, label))

    if not candidates:
        return (None, "")

    # Split into non-placeholder vs placeholder.
    rich = [(c, l) for c, l in candidates if not is_placeholder_evidence(c.get("evidence", ""))]
    if rich:
        # Prefer committed_prior if it has non-placeholder evidence (manual edits).
        for c, l in rich:
            if l == "committed_prior":
                return (c, l)
        # Otherwise the archive entry has richer evidence — use it.
        return rich[0]

    # All placeholders: committed_prior first.
    for c, l in candidates:
        if l == "committed_prior":
            return (c, l)
    return candidates[0]


def refresh_secondary_classes(
    historical_secondary: dict,
    live_callsite_pairs: set[tuple[str, int]],
) -> dict:
    """Drop stale line references from secondary_classes_present.

    Only keep entries whose (file, line) is still a live callsite.
    """
    refreshed = {}
    for cls, refs in historical_secondary.items():
        kept = []
        for ref in refs:
            # ref format: "path/to/file.rs:LINE"
            parts = ref.rsplit(":", 1)
            if len(parts) != 2:
                continue
            f, ln_s = parts
            try:
                ln = int(ln_s)
            except ValueError:
                continue
            if (f, ln) in live_callsite_pairs:
                kept.append(ref)
        if kept:
            refreshed[cls] = kept
    return refreshed


# -- regen ------------------------------------------------------------------

def regenerate(fixture_path: Path) -> dict:
    """Produce the refreshed fixture and write it to *fixture_path*."""
    hits = discover_raw_hits()
    definitions, reexports, docrefs, raw_callsites = classify_raw_hits(hits)

    # Historical sources: committed prior first, archive fallback.
    prior_committed = load_committed_prior(fixture_path)
    archive = load_archive()
    historical_sources = [s for s in [prior_committed, archive] if s]

    callsite_entries = []
    provenance_counts: Counter = Counter()
    for cs in raw_callsites:
        file_path = REPO_ROOT / cs["file"]
        fn_name, body = find_enclosing_function(file_path, cs["line"])
        env_role, env_role_evidence = classify_env_role(body)
        env_vars = extract_env_vars(body)
        del_scope = deletion_scope_for(cs["file"])

        historical, hist_source = match_historical(cs, fn_name, historical_sources)
        if historical:
            evidence = historical.get("evidence", "")
            evidence_provenance = f"historical_leaf0 ({hist_source})"
            hist_class = historical.get("class", "class2_runtime_config")
        else:
            evidence = f"structural inspection of {fn_name}"
            evidence_provenance = "regenerated_structural"
            hist_class = "class2_runtime_config"

        provenance_counts[evidence_provenance] += 1

        callsite_entries.append({
            "file": cs["file"],
            "line": cs["line"],
            "test_or_fn_name": fn_name,
            "class": hist_class,
            "evidence": evidence,
            "evidence_provenance": evidence_provenance,
            "env_vars_touched": env_vars,
            "env_vars_touched_note": "re-derived from enclosing function body, not preserved from archive",
            "env_role": env_role,
            "env_role_evidence": env_role_evidence,
            "deletion_scope": del_scope,
        })

    callsite_entries.sort(key=lambda c: (c["file"], c["line"]))

    # Historical mapping accounting: specifically account for every row in the
    # original audited archive (the 142-entry hand-audited Leaf-0), NOT the
    # intermediate prior fixture.  Match by (file, test_or_fn_name).
    archive_entries = archive.get("callsites", []) if archive else []
    live_fn_pairs = {(c["file"], c["test_or_fn_name"]) for c in callsite_entries}
    archive_unmatched_list = []
    archive_matched = 0
    for ae in archive_entries:
        key = (ae.get("file", ""), ae.get("test_or_fn_name", ""))
        if key in live_fn_pairs:
            archive_matched += 1
        else:
            archive_unmatched_list.append({
                "file": ae.get("file", ""),
                "line": ae.get("line", 0),
                "test_or_fn_name": ae.get("test_or_fn_name", ""),
            })
    archive_total = len(archive_entries)
    archive_unmatched_n = len(archive_unmatched_list)

    # Stats
    per_env_role = Counter(c["env_role"] for c in callsite_entries)
    per_deletion = Counter(c["deletion_scope"] for c in callsite_entries)

    live_pairs = {(c["file"], c["line"]) for c in callsite_entries}

    # Refresh secondary classes (drop stale line references).
    hist_secondary = {}
    for s in historical_sources:
        if s and "stats" in s:
            hist_secondary = s["stats"].get("secondary_classes_present", {})
            break
    refreshed_secondary = refresh_secondary_classes(hist_secondary, live_pairs)

    fixture = {
        "schema_version": "3",
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "generated_from_note": (
            "Refreshed by scripts/lock_census.py regen against the current branch head. "
            "Evidence selection: for the same (file, test_or_fn_name), non-placeholder "
            "evidence is preferred over placeholder evidence — committed-prior non-placeholder "
            "beats archive; archive non-placeholder beats committed-prior placeholder. "
            "This prevents structural placeholders from shadowing owner-audited archive evidence. "
            "env_vars_touched is re-derived; env_role and deletion_scope are new in schema v3 "
            "(Refs #1476). See historical_mapping.evidence_provenance_counts for the distribution."
        ),
        "definitions": definitions,
        "doc_comment_references_non_callsites": docrefs,
        "callsites": callsite_entries,
        "historical_mapping": {
            "archive_total_entries": archive_total,
            "archive_matched": archive_matched,
            "archive_unmatched": archive_unmatched_n,
            "unmatched_entries": archive_unmatched_list,
            "evidence_provenance_counts": dict(sorted(provenance_counts.items())),
            "source_preference": (
                "For the same (file, test_or_fn_name), non-placeholder evidence "
                "is preferred over placeholder evidence. Committed-prior "
                "non-placeholder (manually edited) evidence beats archive; "
                "archive non-placeholder (hand-audited) evidence beats "
                "committed-prior placeholder. This prevents structural "
                "placeholders generated by a prior regen from shadowing richer "
                "owner-audited archive evidence."
            ),
            "note": (
                "Every original Leaf-0 archive row is accounted for. "
                "Matched by (file, test_or_fn_name) against the live callsite set; "
                "unmatched rows are callsites whose function was removed, renamed, "
                "or whose line drifted to a different function since the 2026-07-14 audit."
            ),
        },
        "stats": {
            "total_rg_hits": len(hits),
            "definitions_and_reexports": len(definitions) + len(reexports),
            "doc_comment_references_non_callsites": len(docrefs),
            "total_callsites": len(callsite_entries),
            "per_class_primary": {
                "class1_registration": 0,
                "class2_runtime_config": len(callsite_entries),
                "class3_fs_identity": 0,
                "class4_harness": 0,
                "class5_product_concurrency": 0,
                "unexplained": 0,
            },
            "secondary_classes_present": refreshed_secondary,
            "secondary_classes_note": (
                "Refreshed: stale line references dropped. Only entries whose "
                "(file, line) is still a live callsite are retained."
            ),
            "per_env_role": dict(sorted(per_env_role.items())),
            "per_deletion_scope": dict(sorted(per_deletion.items())),
        },
    }

    fixture_path.parent.mkdir(parents=True, exist_ok=True)
    fixture_path.write_text(json.dumps(fixture, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    return fixture


# -- validate ---------------------------------------------------------------

def validate_census(fixture: dict, live_callsites: list[tuple[str, int]]) -> list[str]:
    """Pure validation: return list of error strings (empty = valid).

    Args:
        fixture: the committed census dict.
        live_callsites: list of (file, line) tuples from live rg discovery.
    """
    errors: list[str] = []

    # 1. Every live callsite appears in fixture (missing detection).
    fixture_pairs = {(c["file"], c["line"]) for c in fixture.get("callsites", [])}
    live_set = set(live_callsites)
    for f, ln in sorted(live_set - fixture_pairs):
        errors.append(f"missing: live callsite {f}:{ln} not in fixture")

    # 2. No fixture callsite is stale (not in live set).
    for f, ln in sorted(fixture_pairs - live_set):
        errors.append(f"stale: fixture callsite {f}:{ln} not in live code")

    # 3. No duplicate identities.
    seen: dict[tuple, int] = {}
    for c in fixture.get("callsites", []):
        key = (c["file"], c["line"])
        seen[key] = seen.get(key, 0) + 1
    for key, count in seen.items():
        if count > 1:
            errors.append(f"duplicate: {key[0]}:{key[1]} appears {count} times")

    # 4. Valid enum values.
    for c in fixture.get("callsites", []):
        role = c.get("env_role", "")
        if role not in VALID_ENV_ROLES:
            errors.append(f"invalid env_role '{role}' at {c['file']}:{c['line']}")
        scope = c.get("deletion_scope", "")
        if scope not in VALID_DELETION_SCOPES:
            errors.append(f"invalid deletion_scope '{scope}' at {c['file']}:{c['line']}")

    # 5. Summary count integrity.
    stats = fixture.get("stats", {})
    callsites = fixture.get("callsites", [])

    expected_total = len(callsites)
    actual_total = stats.get("total_callsites", -1)
    if actual_total != expected_total:
        errors.append(
            f"count drift: stats.total_callsites={actual_total} but len(callsites)={expected_total}"
        )

    per_role = stats.get("per_env_role", {})
    for role in VALID_ENV_ROLES:
        expected = sum(1 for c in callsites if c.get("env_role") == role)
        actual = per_role.get(role, 0)
        if expected != actual:
            errors.append(
                f"count drift: stats.per_env_role.{role}={actual} but actual={expected}"
            )

    per_del = stats.get("per_deletion_scope", {})
    for scope in VALID_DELETION_SCOPES:
        expected = sum(1 for c in callsites if c.get("deletion_scope") == scope)
        actual = per_del.get(scope, 0)
        if expected != actual:
            errors.append(
                f"count drift: stats.per_deletion_scope.{scope}={actual} but actual={expected}"
            )

    return errors


def validate_command(fixture_path: Path) -> int:
    """Validate the committed fixture against live code. Return exit code."""
    if not fixture_path.exists():
        print(f"validate: fixture not found at {fixture_path}", file=sys.stderr)
        return 2
    fixture = json.loads(fixture_path.read_text(encoding="utf-8"))

    hits = discover_raw_hits()
    _, _, _, raw_callsites = classify_raw_hits(hits)
    live = [(c["file"], c["line"]) for c in raw_callsites]

    errors = validate_census(fixture, live)
    if errors:
        print(f"validate: {len(errors)} error(s):", file=sys.stderr)
        for e in errors[:50]:
            print(f"  - {e}", file=sys.stderr)
        if len(errors) > 50:
            print(f"  ... and {len(errors) - 50} more", file=sys.stderr)
        return 1
    print(f"validate: OK — {len(live)} live callsites match fixture")
    return 0


# -- main -------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(description="Lock census regen/validate (#1476)")
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_regen = sub.add_parser("regen", help="Regenerate the fixture from live code")
    p_regen.add_argument("--fixture", default=str(DEFAULT_FIXTURE))

    p_val = sub.add_parser("validate", help="Validate the committed fixture")
    p_val.add_argument("--fixture", default=str(DEFAULT_FIXTURE))

    args = parser.parse_args()
    fixture_path = Path(args.fixture)

    if args.cmd == "regen":
        fixture = regenerate(fixture_path)
        n = len(fixture["callsites"])
        hm = fixture.get("historical_mapping", {})
        print(f"regen: wrote {n} callsites to {fixture_path}")
        print(f"  historical: matched={hm.get('archive_matched',0)}/{hm.get('archive_total_entries',0)} "
              f"unmatched={hm.get('archive_unmatched',0)}")
        return 0
    elif args.cmd == "validate":
        return validate_command(fixture_path)

    return 2


if __name__ == "__main__":
    raise SystemExit(main())
