#!/usr/bin/env python3
"""
Backfill script: reads Gemini CLI session transcripts and uses SIGIL's
extractor (Qwen3.5-27B) + Voyage-4 to extract and store memories.

Usage:
    cd <PROJECT_ROOT>
    .venv/bin/python scripts/backfill_gemini_cli.py [--since YYYY-MM-DD] [--project NAME] [--dry-run]

Environment (loaded from ~/.secrets/master.env):
    VOYAGE_API_KEY        Voyage-4 embeddings
    SILICONFLOW_API_KEY   Qwen3.5-27B extraction
    MEMORY_DB_PATH        Target SQLite DB (default: ~/.gemini/antigravity/memory.db)
"""

import argparse
import asyncio
import glob
import json
import os
import sys
import time
from datetime import datetime

# Inject SIGIL's mcp/ into path so we can reuse extractor + embedding + store
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
SIGIL_ROOT = os.path.dirname(SCRIPT_DIR)
MCP_DIR = os.path.join(SIGIL_ROOT, "mcp")
sys.path.insert(0, MCP_DIR)

from dotenv import load_dotenv

load_dotenv(os.path.expanduser("~/.secrets/master.env"))

# Default target DB = the one Gemini CLI's SIGIL MCP uses
if not os.environ.get("MEMORY_DB_PATH"):
    os.environ["MEMORY_DB_PATH"] = os.path.expanduser("~/.gemini/antigravity/memory.db")

import store
import embedding
import extractor

# ── Constants ────────────────────────────────────────────────────────────────
GEMINI_TMP = os.path.expanduser("~/.gemini/tmp")
CHUNK_SIZE = 6  # messages per extraction window
RATE_LIMIT_S = 0.3  # seconds between API calls
MIN_MSG_LEN = 20  # skip trivial messages
MAX_MSG_LEN = 6000  # truncate very long messages
MIN_WINDOW_LEN = 80  # skip tiny windows

# Known project directory names → human-readable labels
PROJECT_LABELS = {
    "quant-analyzer-2026": "quant-v8",
    "quant-analyzer": "quant-legacy",
    "sigil": "sigil",
    "kckylechen": "general",
    "openclaw": "openclaw",
    "dragonfly-imago": "dragonfly",
    "star-office-ui": "star-office",
    "autoresearch-lab": "autoresearch",
}


def find_sessions(since: str | None = None, project_filter: str | None = None):
    """Discover all Gemini CLI session JSON files."""
    since_ts = None
    if since:
        since_ts = datetime.fromisoformat(since).timestamp()

    sessions = []
    for chat_dir in glob.glob(os.path.join(GEMINI_TMP, "*/chats")):
        project_dir = os.path.basename(os.path.dirname(chat_dir))

        # Resolve project name
        if project_dir in PROJECT_LABELS:
            project_name = PROJECT_LABELS[project_dir]
        elif len(project_dir) == 64 and all(
            c in "0123456789abcdef" for c in project_dir
        ):
            project_name = f"hash-{project_dir[:8]}"
        else:
            project_name = project_dir

        if project_filter and project_name != project_filter:
            continue

        for path in sorted(glob.glob(os.path.join(chat_dir, "session-*.json"))):
            mtime = os.path.getmtime(path)
            if since_ts and mtime < since_ts:
                continue
            try:
                with open(path) as f:
                    data = json.load(f)
                msg_count = len(data.get("messages", []))
                if msg_count < 2:
                    continue
                sessions.append(
                    {
                        "path": path,
                        "project": project_name,
                        "project_dir": project_dir,
                        "session_id": data.get("sessionId", ""),
                        "start_time": data.get("startTime", ""),
                        "summary": data.get("summary", ""),
                        "msg_count": msg_count,
                        "mtime": mtime,
                    }
                )
            except (json.JSONDecodeError, OSError):
                pass

    return sorted(sessions, key=lambda s: s["mtime"])


def read_messages(session_path: str) -> list[dict]:
    """Parse Gemini CLI session JSON into user/assistant message pairs."""
    with open(session_path) as f:
        data = json.load(f)

    messages = []
    for msg in data.get("messages", []):
        msg_type = msg.get("type")
        if msg_type not in ("user", "gemini"):
            continue

        role = "user" if msg_type == "user" else "assistant"
        content = msg.get("content", "")

        # Handle content formats: list of {text: "..."} or bare string
        if isinstance(content, list):
            text_parts = []
            for part in content:
                if isinstance(part, dict) and "text" in part:
                    text_parts.append(part["text"])
                elif isinstance(part, str):
                    text_parts.append(part)
            content = "\n".join(text_parts)
        elif not isinstance(content, str):
            continue

        content = content.strip()

        # Skip trivial, tool-only, or slash-command messages
        if not content or len(content) < MIN_MSG_LEN:
            continue
        if content.startswith("/"):
            continue

        # Truncate very long messages
        if len(content) > MAX_MSG_LEN:
            content = content[:MAX_MSG_LEN] + "\n[...truncated...]"

        messages.append({"role": role, "content": content})

    return messages


def chunk_messages(messages: list[dict]) -> list[str]:
    """Chunk messages into windows for extraction (same as OpenClaw backfill)."""
    windows = []
    for i in range(0, len(messages), CHUNK_SIZE):
        chunk = messages[i : i + CHUNK_SIZE]
        window = "\n\n".join(
            f"[{j}] {m['role']}: {m['content']}" for j, m in enumerate(chunk)
        )
        if len(window) >= MIN_WINDOW_LEN:
            windows.append(window)
    return windows


async def process_session(
    session: dict,
    mem_store,
    dry_run: bool = False,
) -> tuple[int, int, int]:
    """Process one session: extract facts → embed → save to SIGIL.
    Returns (saved, skipped, errors).
    """
    messages = read_messages(session["path"])
    if len(messages) < 2:
        return 0, 0, 0

    windows = chunk_messages(messages)
    saved, skipped, errors = 0, 0, 0

    for chunk_idx, window in enumerate(windows):
        try:
            # Step 1: LLM extraction
            facts = await extractor.extract_facts(window)
            if not facts:
                print(f"    chunk {chunk_idx}: no facts extracted", flush=True)
                continue

            for fact in facts:
                fact_text = fact["text"]
                if not fact_text or len(fact_text.strip()) < 5:
                    continue

                if dry_run:
                    print(f"    [DRY] {fact_text[:80]}", flush=True)
                    saved += 1
                    continue

                # Step 2: Embed
                vec = await embedding.embed(fact_text, input_type="document")

                # Step 3: Build path
                scope = fact.get("scope", "project")
                topic = fact.get("topic", "")
                path = f"/gemini-cli/{session['project']}"
                if topic:
                    safe_topic = topic.replace(" ", "_").replace("/", "_")
                    path += f"/{safe_topic}"

                # Step 4: Save with dedup
                result = store.save_memory(
                    mem_store,
                    text=fact_text,
                    vector=vec,
                    path=path,
                    summary=fact_text[:80],
                    topic=topic,
                    keywords=fact.get("keywords", []),
                    scope=scope,
                    importance=fact.get("importance", 0.7),
                    source="gemini-cli-backfill",
                    metadata={
                        "session_id": session["session_id"],
                        "project": session["project"],
                        "session_start": session["start_time"],
                    },
                )

                if result:
                    print(f"    chunk {chunk_idx}: \u2705 {fact_text[:80]}", flush=True)
                    saved += 1
                else:
                    print(f"    chunk {chunk_idx}: \u23ed duplicate", flush=True)
                    skipped += 1

            await asyncio.sleep(RATE_LIMIT_S)

        except Exception as e:
            print(f"    chunk {chunk_idx}: \u274c error: {e}", flush=True)
            errors += 1

    return saved, skipped, errors


async def main():
    parser = argparse.ArgumentParser(
        description="Backfill Gemini CLI sessions into SIGIL memory"
    )
    parser.add_argument(
        "--since",
        type=str,
        default=None,
        help="Only process sessions modified after this date (YYYY-MM-DD)",
    )
    parser.add_argument(
        "--project",
        type=str,
        default=None,
        help="Only process sessions from this project",
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="Extract facts but don't save to DB"
    )
    parser.add_argument(
        "--limit", type=int, default=0, help="Max sessions to process (0 = all)"
    )
    args = parser.parse_args()

    sessions = find_sessions(since=args.since, project_filter=args.project)
    if args.limit > 0:
        sessions = sessions[: args.limit]

    db_path = os.environ["MEMORY_DB_PATH"]
    print(f"=== Gemini CLI → SIGIL Backfill ===", flush=True)
    print(f"Sessions found: {len(sessions)}", flush=True)
    print(f"Target DB: {db_path}", flush=True)
    print(f"Dry run: {args.dry_run}", flush=True)
    print(f"Voyage key: {'configured' if os.environ.get('VOYAGE_API_KEY') else 'MISSING'}", flush=True)
    print(
        f"SiliconFlow key: {'configured' if os.environ.get('SILICONFLOW_API_KEY') else 'MISSING'}",
        flush=True,
    )
    print(flush=True)

    if not sessions:
        print("No sessions to process.", flush=True)
        return

    mem_store = store.get_connection()
    total_saved, total_skipped, total_errors = 0, 0, 0

    for i, sess in enumerate(sessions, 1):
        print(
            f"\n[{i}/{len(sessions)}] {sess['project']}/{sess['session_id'][:8]}... "
            f"({sess['msg_count']} msgs, {sess['start_time'][:10]})",
            flush=True,
        )
        if sess.get("summary"):
            print(f"  Summary: {sess['summary'][:80]}", flush=True)

        try:
            s, sk, e = await process_session(sess, mem_store, dry_run=args.dry_run)
            total_saved += s
            total_skipped += sk
            total_errors += e
            print(f"  → saved={s}, skipped={sk}, errors={e}", flush=True)
        except Exception as ex:
            print(f"  Session error: {ex}", flush=True)
            total_errors += 1

    print(f"\n=== Done ===", flush=True)
    print(
        f"Total: {total_saved} saved, {total_skipped} skipped, {total_errors} errors",
        flush=True,
    )


if __name__ == "__main__":
    asyncio.run(main())
