---
name: eng-auto-checkpoint
description: |
  Persist durable eng-session milestones into Tachi memory on PR open, CI green,
  merge, or bench write-up. Call the in-repo helper so every agent emits the same
  `/eng/<repo>/<pr>/<event>` shape with idempotent dedup (#2132).
---

# eng-auto-checkpoint

Parent: [#2113](https://github.com/kckylechen1/Tachi/issues/2113) · Leaf: [#2132](https://github.com/kckylechen1/Tachi/issues/2132)

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
2. Lists existing rows at that path via `tachi list`.
3. **Same idempotency key** `(repo, pr, event, sha[, fingerprint])` → **skip**.
4. **Same event, newer tip** → `tachi remember --supersedes <prior_id>` (#2126).
5. Otherwise `tachi remember` with `category=handoff`, `topic=eng-milestone`,
   `retention-policy=durable`, tags `eng-checkpoint,pr-N,<event>`.

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

## Non-goals

No GitHub webhook, no retrieval P0–P2 changes, no Hypermem, no per-turn chat capture.
