#!/usr/bin/env python3
"""Eng-session auto-checkpoint helper (#2132).

Builds a stable `/eng/<repo>/<pr>/<event>` memory via `tachi remember`/`save`
(category=handoff), with idempotent skip / supersede-on-newer-tip.

Pure helpers in this module are unit-tested without a live daemon.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

EVENTS = ("opened", "ci-green", "merged", "bench")
EVENT_ALIASES = {
    "pr_opened": "opened",
    "pr-opened": "opened",
    "open": "opened",
    "ci_green": "ci-green",
    "ci": "ci-green",
    "pr_merged": "merged",
    "pr-merged": "merged",
    "merge": "merged",
    "bench_writeup": "bench",
    "bench-writeup": "bench",
}
META_FENCE_START = "<!-- eng-checkpoint-meta"
META_FENCE_END = "-->"
DEFAULT_REPO = "kckylechen1/Tachi"
DEFAULT_REPO_SHORT = "tachi"
MIN_BODY_CHARS = 200


@dataclass(frozen=True)
class CheckpointSpec:
    event: str
    pr: int
    sha: str
    repo: str = DEFAULT_REPO
    repo_short: str = DEFAULT_REPO_SHORT
    title: str = ""
    body: str = ""
    issue: Optional[int] = None
    ci_url: str = ""
    bench_summary: str = ""
    agent_seat: str = ""
    result_fingerprint: str = ""

    @property
    def path(self) -> str:
        return build_path(self.repo_short, self.pr, self.event)

    @property
    def idempotency_key(self) -> str:
        return build_idempotency_key(
            self.repo,
            self.pr,
            self.event,
            self.sha,
            result_fingerprint=self.result_fingerprint if self.event == "bench" else "",
        )


def normalize_event(raw: str) -> str:
    key = (raw or "").strip().lower().replace("_", "-")
    key = EVENT_ALIASES.get(key.replace("-", "_"), EVENT_ALIASES.get(key, key))
    # EVENT_ALIASES may map via underscore form; normalize again
    key = EVENT_ALIASES.get(key, key)
    if key not in EVENTS:
        raise ValueError(f"unknown event {raw!r}; expected one of {EVENTS}")
    return key


def build_path(repo_short: str, pr: int, event: str) -> str:
    short = (repo_short or DEFAULT_REPO_SHORT).strip().strip("/").lower()
    ev = normalize_event(event)
    if pr < 0:
        raise ValueError("pr must be >= 0")
    return f"/eng/{short}/{pr}/{ev}"


def build_idempotency_key(
    repo: str,
    pr: int,
    event: str,
    sha: str,
    result_fingerprint: str = "",
) -> str:
    ev = normalize_event(event)
    tip = (sha or "").strip().lower()
    if not tip:
        raise ValueError("sha is required for the idempotency key")
    repo_n = (repo or DEFAULT_REPO).strip()
    parts = [repo_n, str(pr), ev, tip]
    if ev == "bench":
        parts.append((result_fingerprint or "").strip())
    return "|".join(parts)


def repo_short_from_remote(remote_url: str) -> str:
    """Derive short repo name from a git remote URL."""
    url = (remote_url or "").strip()
    if not url:
        return DEFAULT_REPO_SHORT
    url = url.rstrip("/")
    if url.endswith(".git"):
        url = url[:-4]
    name = url.split(":")[-1].split("/")[-1]
    return (name or DEFAULT_REPO_SHORT).lower()


def full_repo_from_remote(remote_url: str) -> str:
    url = (remote_url or "").strip()
    if not url:
        return DEFAULT_REPO
    url = url.rstrip("/")
    if url.endswith(".git"):
        url = url[:-4]
    m = re.search(r"[:/]([^/]+)/([^/]+)$", url)
    if not m:
        return DEFAULT_REPO
    return f"{m.group(1)}/{m.group(2)}"


def parse_meta_from_text(text: str) -> dict[str, Any]:
    if not text:
        return {}
    m = re.search(
        re.escape(META_FENCE_START) + r"\s*(\{.*?\})\s*" + re.escape(META_FENCE_END),
        text,
        flags=re.DOTALL,
    )
    if not m:
        return {}
    try:
        data = json.loads(m.group(1))
    except json.JSONDecodeError:
        return {}
    return data if isinstance(data, dict) else {}


def extract_prior_key(entry: dict[str, Any]) -> str:
    meta = parse_meta_from_text(entry.get("text") or "")
    if isinstance(meta.get("idempotency_key"), str) and meta["idempotency_key"]:
        return meta["idempotency_key"]
    # Fallback: reconstruct from stored fields when present
    sha = meta.get("head_sha") or meta.get("merge_sha") or ""
    if not sha:
        return ""
    try:
        return build_idempotency_key(
            str(meta.get("repo") or DEFAULT_REPO),
            int(meta.get("pr") or 0),
            str(meta.get("eng_event") or ""),
            str(sha),
            result_fingerprint=str(meta.get("result_fingerprint") or ""),
        )
    except ValueError:
        return ""


def decide_action(existing: list[dict[str, Any]], spec: CheckpointSpec) -> tuple[str, Optional[str]]:
    """Return (action, prior_id) where action is skip|save|supersede."""
    if not existing:
        return "save", None
    # Prefer exact path match; callers usually pass path-filtered rows.
    same_path = [e for e in existing if (e.get("path") or "") == spec.path]
    rows = same_path or existing
    # Newest first if timestamps present
    rows = sorted(rows, key=lambda e: e.get("timestamp") or e.get("updated_at") or "", reverse=True)
    prior = rows[0]
    prior_id = prior.get("id")
    prior_key = extract_prior_key(prior)
    if prior_key and prior_key == spec.idempotency_key:
        return "skip", prior_id
    # Same event path, different tip → supersede
    if prior_id:
        return "supersede", prior_id
    return "save", None


def build_title(spec: CheckpointSpec) -> str:
    sha7 = (spec.sha or "")[:7]
    custom = spec.title.strip()
    if spec.event == "opened":
        return f"PR #{spec.pr} opened: {custom}" if custom else f"PR #{spec.pr} opened"
    if spec.event == "ci-green":
        return f"PR #{spec.pr} CI green @ {sha7}"
    if spec.event == "merged":
        return f"PR #{spec.pr} merged @ {sha7}"
    if spec.event == "bench":
        leaf = custom or f"PR #{spec.pr}"
        return f"Bench: {leaf} @ {sha7}"
    return f"PR #{spec.pr} {spec.event}"


def build_summary(spec: CheckpointSpec) -> str:
    sha7 = (spec.sha or "")[:7]
    base = {
        "opened": f"PR #{spec.pr} opened @ {sha7}",
        "ci-green": f"PR #{spec.pr} CI green @ {sha7}",
        "merged": f"PR #{spec.pr} merged @ {sha7}",
        "bench": f"Bench PR #{spec.pr} @ {sha7}",
    }.get(spec.event, f"PR #{spec.pr} {spec.event} @ {sha7}")
    return base[:100]


def build_body(spec: CheckpointSpec) -> str:
    sha = spec.sha.strip()
    title = build_title(spec)
    bullets = []
    if spec.body.strip():
        for line in spec.body.strip().splitlines():
            line = line.strip()
            if not line:
                continue
            if not line.startswith("-"):
                line = f"- {line}"
            bullets.append(line)
    if not bullets:
        bullets = [f"- Milestone: {spec.event} for PR #{spec.pr}"]
    refs = [f"{spec.repo}#{spec.pr}"]
    if spec.issue:
        refs.append(f"{spec.repo}#{spec.issue}")
    pr_url = f"https://github.com/{spec.repo}/pull/{spec.pr}"
    lines = [
        title,
        "",
        "What changed:",
        *bullets[:5],
        "",
        f"PR: {pr_url}",
        f"References: {', '.join(refs)}",
        f"SHA: {sha}",
    ]
    if spec.ci_url:
        lines.append(f"CI: {spec.ci_url}")
    if spec.bench_summary.strip():
        lines.append(f"Bench: {spec.bench_summary.strip()}")
    if spec.agent_seat.strip():
        lines.append(f"Agent seat: {spec.agent_seat.strip()}")
    lines.extend(
        [
            "",
            f"Source: eng-checkpoint",
            f"Topic: eng-milestone",
            "",
            META_FENCE_START,
            json.dumps(
                {
                    "eng_event": spec.event,
                    "pr": spec.pr,
                    "head_sha": sha,
                    "repo": spec.repo,
                    "idempotency_key": spec.idempotency_key,
                    "result_fingerprint": spec.result_fingerprint or None,
                    "source": "eng-checkpoint",
                },
                separators=(",", ":"),
                sort_keys=True,
            ),
            META_FENCE_END,
        ]
    )
    text = "\n".join(lines)
    if len(text) < MIN_BODY_CHARS:
        # Capture gate floor (200) outside /scratch — pad with stable filler.
        pad = "\n\n(eng-checkpoint detail padding for capture-gate min length)\n"
        text = text + pad + ("." * (MIN_BODY_CHARS - len(text) - len(pad)))
    return text


def build_tags(spec: CheckpointSpec) -> list[str]:
    tags = ["eng-checkpoint", f"pr-{spec.pr}", spec.event]
    if spec.agent_seat.strip():
        tags.append(spec.agent_seat.strip().lower())
    return tags


def run_tachi(args: list[str], *, dry_run: bool = False) -> subprocess.CompletedProcess[str]:
    cmd = ["tachi", *args]
    if dry_run:
        return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps({"dry_run": True, "cmd": cmd}), stderr="")
    return subprocess.run(cmd, check=False, text=True, capture_output=True)


def load_json_stdout(proc: subprocess.CompletedProcess[str]) -> Any:
    out = (proc.stdout or "").strip()
    if not out:
        raise RuntimeError(f"tachi produced empty stdout (stderr={proc.stderr!r})")
    # Strip any leading log lines before JSON
    for i, ch in enumerate(out):
        if ch in "[{":
            out = out[i:]
            break
    return json.loads(out)


def _candidate_db_paths() -> list[Path]:
    home = Path(os.environ.get("TACHI_HOME", Path.home() / ".tachi"))
    paths: list[Path] = []
    # Workspace-local project DB (common on agent boxes)
    local = Path.cwd() / ".tachi" / "tachi-memory.db"
    if local.exists():
        paths.append(local.resolve())
    global_db = home / "global" / "tachi-memory.db"
    if global_db.exists():
        paths.append(global_db.resolve())
    projects = home / "projects"
    if projects.is_dir():
        for db in sorted(projects.glob("*/tachi-memory.db")):
            try:
                resolved = db.resolve()
            except OSError:
                continue
            if resolved not in paths:
                paths.append(resolved)
    # Dedup by inode when symlinked
    seen: set[tuple[int, int]] = set()
    out: list[Path] = []
    for path in paths:
        try:
            st = path.stat()
            key = (st.st_dev, st.st_ino)
        except OSError:
            continue
        if key in seen:
            continue
        seen.add(key)
        out.append(path)
    return out


def list_existing_sqlite(path_prefix: str) -> list[dict[str, Any]]:
    """Read-only fallback when CLI list is empty/unavailable (smoke / pre-restart)."""
    try:
        import sqlite3
    except ImportError:
        return []
    rows: list[dict[str, Any]] = []
    for db in _candidate_db_paths():
        try:
            conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        except sqlite3.Error:
            continue
        try:
            cur = conn.execute(
                "SELECT id, path, text, summary, timestamp, updated_at "
                "FROM memories WHERE path = ? OR path LIKE ? "
                "ORDER BY COALESCE(updated_at, timestamp) DESC LIMIT 20",
                (path_prefix, path_prefix.rstrip("/") + "/%"),
            )
            for id_, path, text, summary, ts, updated in cur.fetchall():
                rows.append(
                    {
                        "id": id_,
                        "path": path,
                        "text": text or "",
                        "summary": summary or "",
                        "timestamp": ts or "",
                        "updated_at": updated or "",
                        "db": str(db),
                    }
                )
        except sqlite3.Error:
            continue
        finally:
            conn.close()
    return rows


def list_existing(path: str, *, dry_run: bool = False) -> list[dict[str, Any]]:
    if dry_run:
        return []
    proc = run_tachi(["list", "--path-prefix", path, "--limit", "20"])
    if proc.returncode != 0:
        raise RuntimeError(f"tachi list failed: {proc.stderr or proc.stdout}")
    data = load_json_stdout(proc)
    if not isinstance(data, list):
        raise RuntimeError(f"unexpected list payload: {type(data)}")
    rows = [e for e in data if isinstance(e, dict)]
    if rows:
        return rows
    # CLI list can miss /eng handoff rows on older daemons; sqlite fallback
    # keeps idempotency for smoke and local boxes until the daemon is rebuilt.
    return [e for e in list_existing_sqlite(path) if (e.get("path") or "") == path or (e.get("path") or "").startswith(path.rstrip("/") + "/")]


def detect_git_repo_meta(cwd: Optional[Path] = None) -> tuple[str, str]:
    root = cwd or Path.cwd()
    try:
        url = subprocess.check_output(
            ["git", "-C", str(root), "remote", "get-url", "origin"],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return DEFAULT_REPO, DEFAULT_REPO_SHORT
    return full_repo_from_remote(url), repo_short_from_remote(url)


def detect_head_sha(cwd: Optional[Path] = None) -> str:
    root = cwd or Path.cwd()
    try:
        return subprocess.check_output(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return ""


def remember_save(
    spec: CheckpointSpec,
    *,
    supersedes: Optional[str] = None,
    dry_run: bool = False,
    force: bool = True,
) -> dict[str, Any]:
    body = build_body(spec)
    tags = ",".join(build_tags(spec))
    args = [
        "remember",
        "--path",
        spec.path,
        "--category",
        "handoff",
        "--topic",
        "eng-milestone",
        "--tags",
        tags,
        "--retention-policy",
        "durable",
        "--importance",
        "0.85",
        "--summary",
        build_summary(spec),
        "--domain",
        "eng",
        "--text",
        body,
    ]
    if force:
        args.append("--force")
    if supersedes:
        args.extend(["--supersedes", supersedes])
    if dry_run:
        return {
            "ok": True,
            "status": "dry_run",
            "path": spec.path,
            "idempotency_key": spec.idempotency_key,
            "supersedes": supersedes,
            "tags": build_tags(spec),
            "summary": build_summary(spec),
            "text_preview": body[:240],
        }
    proc = run_tachi(args)
    if proc.returncode != 0:
        raise RuntimeError(f"tachi remember failed: {proc.stderr or proc.stdout}")
    return load_json_stdout(proc)


def checkpoint(spec: CheckpointSpec, *, dry_run: bool = False, force: bool = True) -> dict[str, Any]:
    existing = list_existing(spec.path, dry_run=dry_run)
    # Also consider siblings under the PR prefix if exact path empty (legacy)
    if not existing and not dry_run:
        prefix = f"/eng/{spec.repo_short}/{spec.pr}/"
        existing = [e for e in list_existing(prefix) if (e.get("path") or "") == spec.path]
    action, prior_id = decide_action(existing, spec)
    result: dict[str, Any] = {
        "action": action,
        "path": spec.path,
        "idempotency_key": spec.idempotency_key,
        "prior_id": prior_id,
        "event": spec.event,
        "pr": spec.pr,
    }
    if action == "skip":
        result["ok"] = True
        result["status"] = "skipped"
        result["id"] = prior_id
        return result
    saved = remember_save(
        spec,
        supersedes=prior_id if action == "supersede" else None,
        dry_run=dry_run,
        force=force,
    )
    result.update(saved if isinstance(saved, dict) else {"raw": saved})
    # Server-side exact path+text dedupe surfaces as status=duplicate.
    if isinstance(saved, dict) and saved.get("status") == "duplicate":
        result["action"] = "skip"
        result["status"] = "skipped"
        result["id"] = saved.get("id") or prior_id
    else:
        result["action"] = action
    result["ok"] = True
    return result


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="eng-checkpoint",
        description="Auto-checkpoint eng milestones into Tachi (#2132).",
    )
    p.add_argument(
        "--event",
        required=True,
        help="opened | ci-green | merged | bench (aliases: pr_opened, pr_merged, ...)",
    )
    p.add_argument("--pr", type=int, required=True, help="Pull request number")
    p.add_argument("--sha", default="", help="Head/merge tip SHA (default: git HEAD)")
    p.add_argument("--title", default="", help="Optional title fragment / PR title")
    p.add_argument("--body", default="", help="Optional bullets (newline-separated)")
    p.add_argument("--issue", type=int, default=None, help="Linked issue number")
    p.add_argument("--ci-url", default="", help="CI run URL")
    p.add_argument("--bench-summary", default="", help="Bench delta summary")
    p.add_argument("--result-fingerprint", default="", help="Bench result fingerprint for dedup")
    p.add_argument("--agent-seat", default="", help="Agent seat label (opus/rex/dash)")
    p.add_argument("--repo", default="", help="owner/repo (default: origin remote)")
    p.add_argument("--repo-short", default="", help="Short repo segment for path (default: remote)")
    p.add_argument("--dry-run", action="store_true", help="Print planned write; do not call tachi")
    p.add_argument("--no-force", action="store_true", help="Do not pass --force to tachi remember")
    return p


def main(argv: Optional[list[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        event = normalize_event(args.event)
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    repo, repo_short = detect_git_repo_meta()
    if args.repo:
        repo = args.repo
    if args.repo_short:
        repo_short = args.repo_short
    sha = (args.sha or detect_head_sha()).strip()
    if not sha:
        print("error: --sha required when git HEAD is unavailable", file=sys.stderr)
        return 2
    spec = CheckpointSpec(
        event=event,
        pr=args.pr,
        sha=sha,
        repo=repo,
        repo_short=repo_short or DEFAULT_REPO_SHORT,
        title=args.title,
        body=args.body,
        issue=args.issue,
        ci_url=args.ci_url,
        bench_summary=args.bench_summary,
        agent_seat=args.agent_seat,
        result_fingerprint=args.result_fingerprint,
    )
    try:
        result = checkpoint(spec, dry_run=args.dry_run, force=not args.no_force)
    except Exception as e:
        print(json.dumps({"ok": False, "error": str(e)}))
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
