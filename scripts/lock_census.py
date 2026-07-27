#!/usr/bin/env python3
"""Lock-census regeneration and validation for #1096 Leaf-0 (#1476).

Subcommands:
  regen     — discover every live ``global_test_lock`` callsite, classify
              ``env_role`` by inspecting the constructor/function under test,
              preserve historical Leaf-0 evidence, and emit the refreshed
              JSON artifact.  Run manually after the callsite set changes.
  validate  — re-derive the live callsite set and diff it against the
              committed fixture.  Exits nonzero on missing/stale/duplicate/
              invalid-enum/count drift.  Does NOT rewrite the fixture.

The classification heuristic is structural and deterministic: it reads the
enclosing function body and pattern-matches for env-reading constructors
(``from_env``, ``LlmClient::new``, ``make_server``, ``tachi_home`` …) versus
injection constructors (``new_with_config``, ``new_with_home_for_test``,
``make_server_with_temp_home`` …).  ``unknown`` is emitted when neither is
found — never inferred from env-var names alone (Refs #1476).
"""

from __future__ import annotations

import argparse
import json
import os
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

# #1319 contraction-umbrella deletion targets (Shell, Arena, Task-dispatch).
DELETION_PATH_FRAGMENTS = [
    "tests/dispatch_tests/",
    "/arena_ops/",
    "/shell_ops/",
    "bootstrap/serve",
    "dispatch_ops/dispatch/",
    "dispatch_ops/prompt",
    "bootstrap/cli_tool/tool_dispatch",
]

# Env-reading constructors: calling these in the function body is evidence that
# the test's contract is env parsing / default resolution / fail-closed.
ENV_READING_PATTERNS = [
    re.compile(r"\bfrom_env\s*\(\s*\)"),
    re.compile(r"\bLlmClient::new\s*\("),         # bare new(), not new_with_*
    re.compile(r"\bmake_server\s*\("),             # bare make_server(), not _with_*
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

# Env-var mutation extraction: capture quoted env-var names from mutation calls.
ENV_SET_RE = re.compile(
    r'(?:EnvRestore::(?:set|unset|remove)|std::env::(?:set_var|remove_var))'
    r'\s*\(\s*"([A-Z_][A-Z0-9_]*)"'
)
ENV_SET_BARE_RE = re.compile(
    r'(?:EnvRestore::(?:set|unset|remove))\s*\(\s*([A-Z_]{3,}[A-Z0-9_]*)\s*[,)]'
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

    # Scan backwards from callsite for the enclosing fn.
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

    # Brace-match from the fn declaration to find the function end.
    depth = 0
    started = False
    end_idx = fn_line_idx
    for idx in range(fn_line_idx, min(fn_line_idx + 200, len(lines))):
        line = lines[idx]
        for ch in line:
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
        # Fell through without matching: use a generous fallback.
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
    """Extract env-var names mutated in the function body."""
    body_text = "\n".join(body_lines)
    found: set[str] = set()
    for m in ENV_SET_RE.finditer(body_text):
        found.add(m.group(1))
    for m in ENV_SET_BARE_RE.finditer(body_text):
        # Filter out Rust keywords/constants that aren't env vars.
        name = m.group(1)
        if name not in {"true", "false", "None", "Some"}:
            found.add(name)
    return sorted(found)


def deletion_scope_for(file_path: str) -> str:
    """Return '1319' if the file is in #1319 deletion scope, else 'none'."""
    return "1319" if any(frag in file_path for frag in DELETION_PATH_FRAGMENTS) else "none"


# -- historical evidence preservation --------------------------------------

def load_archive() -> dict | None:
    """Load the historical Leaf-0 archive for evidence preservation."""
    for path in [ARCHIVE_FALLBACK]:
        if path.exists():
            try:
                return json.loads(path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                pass
    return None


def match_historical(callsite: dict, archive: dict | None) -> dict | None:
    """Find the historical entry matching this callsite (by file + nearby line).

    Line numbers drift; match by file and nearest line within a window.
    """
    if not archive:
        return None
    f = callsite["file"]
    ln = callsite["line"]
    candidates = [c for c in archive.get("callsites", []) if c.get("file") == f]
    if not candidates:
        return None
    # Nearest line within ±5.
    nearest = min(candidates, key=lambda c: abs(c.get("line", 0) - ln))
    if abs(nearest.get("line", 0) - ln) <= 5:
        return nearest
    return None


# -- regen ------------------------------------------------------------------

def regenerate(fixture_path: Path) -> dict:
    """Produce the refreshed fixture and write it to *fixture_path*."""
    hits = discover_raw_hits()
    definitions, reexports, docrefs, raw_callsites = classify_raw_hits(hits)
    archive = load_archive()

    callsite_entries = []
    for cs in raw_callsites:
        file_path = REPO_ROOT / cs["file"]
        fn_name, body = find_enclosing_function(file_path, cs["line"])
        env_role, env_role_evidence = classify_env_role(body)
        env_vars = extract_env_vars(body)
        del_scope = deletion_scope_for(cs["file"])

        historical = match_historical(cs, archive)
        evidence = (
            historical.get("evidence", "")
            if historical
            else f"structural inspection of {fn_name}"
        )
        hist_class = historical.get("class", "class2_runtime_config") if historical else "class2_runtime_config"

        callsite_entries.append({
            "file": cs["file"],
            "line": cs["line"],
            "test_or_fn_name": fn_name,
            "class": hist_class,
            "evidence": evidence,
            "env_vars_touched": env_vars,
            "env_role": env_role,
            "env_role_evidence": env_role_evidence,
            "deletion_scope": del_scope,
        })

    # Sort for deterministic output.
    callsite_entries.sort(key=lambda c: (c["file"], c["line"]))

    # Stats
    per_env_role = Counter(c["env_role"] for c in callsite_entries)
    per_deletion = Counter(c["deletion_scope"] for c in callsite_entries)

    # Preserve historical secondary classes where available.
    secondary = {}
    if archive and "stats" in archive:
        secondary = archive["stats"].get("secondary_classes_present", {})

    fixture = {
        "schema_version": "2",
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "generated_from_note": "Refreshed by scripts/lock_census.py regen against the current branch head.",
        "definitions": definitions,
        "doc_comment_references_non_callsites": docrefs,
        "callsites": callsite_entries,
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
            "secondary_classes_present": secondary,
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

    # 1. Check every live callsite appears in fixture (missing detection).
    fixture_pairs = {(c["file"], c["line"]) for c in fixture.get("callsites", [])}
    live_set = set(live_callsites)
    missing = live_set - fixture_pairs
    for f, ln in sorted(missing):
        errors.append(f"missing: live callsite {f}:{ln} not in fixture")

    # 2. Check no fixture callsite is stale (not in live set).
    stale = fixture_pairs - live_set
    for f, ln in sorted(stale):
        errors.append(f"stale: fixture callsite {f}:{ln} not in live code")

    # 3. Check for duplicate identities.
    seen: dict[tuple, int] = {}
    for c in fixture.get("callsites", []):
        key = (c["file"], c["line"])
        seen[key] = seen.get(key, 0) + 1
    for key, count in seen.items():
        if count > 1:
            errors.append(f"duplicate: {key[0]}:{key[1]} appears {count} times")

    # 4. Validate enum values.
    for c in fixture.get("callsites", []):
        role = c.get("env_role", "")
        if role not in VALID_ENV_ROLES:
            errors.append(f"invalid env_role '{role}' at {c['file']}:{c['line']}")
        scope = c.get("deletion_scope", "")
        if scope not in VALID_DELETION_SCOPES:
            errors.append(f"invalid deletion_scope '{scope}' at {c['file']}:{c['line']}")

    # 5. Validate summary counts.
    stats = fixture.get("stats", {})
    expected_total = len(fixture.get("callsites", []))
    actual_total = stats.get("total_callsites", -1)
    if actual_total != expected_total:
        errors.append(
            f"count drift: stats.total_callsites={actual_total} but len(callsites)={expected_total}"
        )

    per_role = stats.get("per_env_role", {})
    for role in VALID_ENV_ROLES:
        expected = sum(1 for c in fixture.get("callsites", []) if c.get("env_role") == role)
        actual = per_role.get(role, 0)
        if expected != actual:
            errors.append(
                f"count drift: stats.per_env_role.{role}={actual} but actual={expected}"
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
        print(f"regen: wrote {n} callsites to {fixture_path}")
        return 0
    elif args.cmd == "validate":
        return validate_command(fixture_path)

    return 2


if __name__ == "__main__":
    raise SystemExit(main())
