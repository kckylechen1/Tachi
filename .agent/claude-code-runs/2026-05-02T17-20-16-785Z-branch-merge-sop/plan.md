# Execution Plan: Branch Merge SOP

## Current State
- **Branch**: `main`, 31 commits ahead of `origin/main`
- **Already merged** (10 branches): fix/manifest-self-lock, fix/doctor-checkpoint-safety, fix/distill-llm-fallback-swallow, fix/foundry-skip-reasons, feat/storage-enum-enforcement, feat/manifest-canonicalize, feat/path-routing-rules, feat/worker-multi-db, feat/tachi-repair-tool, feat/split-rerank-maintenance-cache
- **In progress with conflicts**: feat/interactive-setup-wizard (8 conflicting files)

## Execution Steps

### Step 1: Resolve feat/interactive-setup-wizard merge conflicts
- Examine all 8 conflicting files
- Resolve each conflict by analyzing both sides and choosing correct code
- Commit the merge

### Step 2: Merge remaining branches (topological order)
- feat/wiki-enhancement (Phase 3)
- feat/async-dispatch (Phase 3, local branch)
- chore/dead-code-cleanup (Phase 4)
- docs/storage-audit-2026-04-30 (Phase 4)

### Step 3: Compile verification
- `cargo check -p memory-server` after each merge
- `cargo test` after all merges

### Step 4: Final release build & deploy
- `cargo build --release`
- Copy binary to `/Users/kckylechen/bin/tachi`

### Step 5: Write completion report
