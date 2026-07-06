#!/usr/bin/env bash
# zvec-rust binding feasibility probe (tachi#683 Phase 0, G1).
#
# This does NOT vendor zvec-rust into this repo (it's a separate upstream
# crate with its own C++ core download step) and does NOT touch crates/ --
# it is a standalone, reproducible recipe for the G1 feasibility check:
# "can zvec-ai/zvec-rust (pinned release tag) be built on this machine
# (macOS arm64) without a from-source C++ build?"
#
# Result recorded in this spike (2026-07-06, macOS arm64, rustc 1.96.1):
# SUCCESS. `cargo build -p zvec` at tag v0.5.0 completed in ~6s using a
# freshly downloaded prebuilt dylib (no CMake build of the C++ core
# required); `cargo run -p zvec --example basic` ran end-to-end
# (collection create -> insert -> vector search -> fetch -> stats ->
# delete) in ~9s including dependency compilation. See FINDINGS.md for the
# full transcript and what this does/doesn't imply for Phase 1.
#
# Usage: bash probe_rust_binding.sh [WORKDIR]
set -euo pipefail

WORKDIR="${1:-$HOME/.cache/zvec-shadow-rust-probe}"
TAG="v0.5.0"  # pinned release tag, NOT main -- per the frozen spec

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/sigil-shared-target}"
echo "CARGO_TARGET_DIR=$CARGO_TARGET_DIR"

if [ ! -d "$WORKDIR" ]; then
  git clone https://github.com/zvec-ai/zvec-rust.git "$WORKDIR"
fi
cd "$WORKDIR"
git fetch --tags
git checkout "$TAG"
git rev-parse HEAD
git describe --tags || true

echo "== cargo build -p zvec (pinned $TAG) =="
time cargo build -p zvec

echo "== cargo run -p zvec --example basic (end-to-end smoke) =="
time cargo run -p zvec --example basic
