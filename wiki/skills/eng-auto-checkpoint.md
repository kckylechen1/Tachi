---
name: eng-auto-checkpoint
description: |
  Persist durable eng-session milestones into Tachi memory on PR open, CI green,
  merge, or bench write-up. Call the in-repo helper so every agent emits the same
  `/eng/<repo>/<pr>/<event>` shape with idempotent dedup (#2132).
---

# eng-auto-checkpoint

Parent: [#2113](https://github.com/kckylechen1/Tachi/issues/2113) · Leaf: [#2132](https://github.com/kckylechen1/Tachi/issues/2132) · Global target: [#2135](https://github.com/kckylechen1/Tachi/issues/2135)

## When to call (required)

| Event | Fire when | Helper `--event` |
|-------|-----------|------------------|
| PR opened | after `gh pr create` succeeds | `opened` |
| CI green | Full CI success on tip (optional MVP) | `ci-green` |
| PR merged | after squash-merge lands on main | `merged` |
| Bench write-up | after bench deltas posted on PR/issue | `bench` |

**MVP acceptance:** call at least on **PR open** and (**merge** or **CI green**).

Do **not** dump raw chat. Prefer 1–5 bullets of what changed.

## Helper

```bash
./scripts/eng-checkpoint \
  --event opened \
  --pr 2130 \
  --sha "$(git rev-parse HEAD)" \
  --title "supersede prior facts" \
  --body $'- wired remember --supersedes\n- Full CI green' \
  --issue 2126 \
  --agent-seat opus
```

Dry-run (no write):

```bash
./scripts/eng-checkpoint --event merged --pr 2130 --sha "$SHA" --dry-run
```

The helper:

1. Builds path `/eng/<repo-short>/<pr>/<event>` (repo-short from `origin`).
2. Lists existing rows at that path via `tachi list` (all DBs it reports, tagged `db`).
3. **Same idempotency key** `(repo, pr, event, sha[, fingerprint])` → **skip**.
4. **Same event, newer tip** → `tachi remember --supersedes <prior_id>` (#2126).
5. Otherwise `tachi remember` with `category=handoff`, `topic=eng-milestone`,
   `retention-policy=durable`, tags `eng-checkpoint,pr-N,<event>`.

### Write target (#2135)

- **Default `--scope global`.** The write is
  `tachi --no-project-db remember --scope global …`, so it lands in the global DB
  that the box MCP serve (`--no-project-db`, global-only) reads. An unscoped
  `tachi_memory search path_prefix=/eng/<repo>/` then finds it.
  `--no-project-db` is required: from a repo checkout, a running global-only daemon
  otherwise gets the write forwarded "via named project", which binds the session
  to the workspace project DB and overrides `scope`.
- **Opt-out `--scope project`** writes to the workspace project DB (the pre-#2135
  behaviour).
- **Legacy project-DB rows** (written before #2135, e.g. `/eng/tachi/2133/merged`):
  - If a target-DB row exists at the path, it drives skip and supersede.
  - If only a legacy row exists and it has the **same key**, the run is a **skip**
    (`legacy_prior_id` is reported) and no duplicate is written.
  - If only a legacy row exists and the **tip is newer**, the run is a plain
    **save** into global with `legacy_prior_id` reported. It does not pass
    `--supersedes`, because the server refuses a cross-DB supersede as `not_found`.
  - Legacy rows are never moved or deleted.

## Payload recipe (matches design)

- **path:** `/eng/tachi/<pr>/<event>` (examples: `opened`, `merged`, `ci-green`, `bench`)
- **category:** `handoff` (not default `checkpoint` — avoids dated `/agent/checkpoints/...` + hardcoded `force=true`)
- **keywords/tags:** `eng-checkpoint`, `pr-<n>`, `<event>`
- **topic:** `eng-milestone`
- **source:** recorded in body meta as `eng-checkpoint` (CLI `remember` has no `--source` yet)
- **retention:** `durable`
- **references:** PR/issue shorthands + URLs embedded in body
- **metadata:** JSON fence `<!-- eng-checkpoint-meta {...} -->` in text (CLI has no `--metadata` yet) including `eng_event`, `pr`, `head_sha`, `repo`, `idempotency_key`

## Probe

```bash
tachi list --path-prefix /eng/tachi/<pr>/ --limit 10
tachi search "这次编程的记忆" --top-k 5
# or MCP: tachi_memory action=search query=... path_prefix=/eng/tachi/
```

Identical event twice → one stored row (`status=skipped` on second call).

Tests: `python3 -m unittest scripts.test_eng_checkpoint`. The pure tests always
run. The integration tests need a binary (`TACHI_BIN=/path/to/tachi` or `tachi`
on PATH) and use a temp `TACHI_HOME` only.

## Non-goals

No GitHub webhook, no retrieval P0–P2 changes, no Hypermem, no per-turn chat capture.
