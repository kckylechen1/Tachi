#!/usr/bin/env bash
# Build and package the Tachi CLI binary for a release triple (#728).
#
# Layout (must match public Homebrew formula / promote script):
#   tachi-vX.Y.Z-<triple>/tachi
# → tachi-vX.Y.Z-<triple>.tar.gz
#
# Usage:
#   scripts/package_release_binary.sh 1.7.0
#   scripts/package_release_binary.sh 1.7.0 --triple aarch64-apple-darwin
#   scripts/package_release_binary.sh 1.7.0 --skip-build   # package existing release binary
#
# Env:
#   CARGO_TARGET_DIR  — cargo target dir (default: cargo's default)
#   OUT_DIR           — where to write the tarball (default: cwd)
set -euo pipefail

VERSION="${1:-}"
shift || true
TRIPLE=""
SKIP_BUILD=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --triple)
      TRIPLE="${2:-}"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    *)
      echo "unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  echo "usage: $0 <version> [--triple <rust-triple>] [--skip-build]" >&2
  exit 2
fi
VERSION="${VERSION#v}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

host_triple() {
  # Read full rustc -vV (no early consumer exit) under `set -o pipefail`.
  rustc -vV | sed -n 's/^host: //p'
}

if [[ -z "$TRIPLE" ]]; then
  TRIPLE="$(host_triple)"
fi
if [[ -z "$TRIPLE" ]]; then
  echo "error: could not resolve host triple (pass --triple)" >&2
  exit 1
fi

OUT_DIR="${OUT_DIR:-$ROOT}"
mkdir -p "$OUT_DIR"
ASSET_STEM="tachi-v${VERSION}-${TRIPLE}"
STAGE="$OUT_DIR/$ASSET_STEM"
TARBALL="$OUT_DIR/${ASSET_STEM}.tar.gz"

echo "== package ${ASSET_STEM} =="

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  echo ">> cargo build --release -p tachi-server (triple=$TRIPLE)"
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    echo "   CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
  fi
  # Host triple: no --target (avoids needing an explicit rustup target install
  # and keeps the binary path at target/release/tachi-server).
  HOST="$(host_triple)"
  if [[ "$TRIPLE" == "$HOST" ]]; then
    cargo build --release --locked -p tachi-server
    BIN_PATH="${CARGO_TARGET_DIR:-$ROOT/target}/release/tachi-server"
  else
    cargo build --release --locked -p tachi-server --target "$TRIPLE"
    BIN_PATH="${CARGO_TARGET_DIR:-$ROOT/target}/${TRIPLE}/release/tachi-server"
  fi
else
  HOST="$(host_triple)"
  if [[ "$TRIPLE" == "$HOST" ]]; then
    BIN_PATH="${CARGO_TARGET_DIR:-$ROOT/target}/release/tachi-server"
  else
    BIN_PATH="${CARGO_TARGET_DIR:-$ROOT/target}/${TRIPLE}/release/tachi-server"
  fi
fi

if [[ ! -x "$BIN_PATH" ]]; then
  echo "error: binary not found or not executable: $BIN_PATH" >&2
  exit 1
fi

echo ">> binary: $BIN_PATH"
if ! "$BIN_PATH" --version; then
  echo "error: smoke test failed — '$BIN_PATH --version' did not run cleanly; refusing to package a broken binary" >&2
  exit 1
fi

rm -rf "$STAGE"
mkdir -p "$STAGE"
# Public install name is `tachi` (Homebrew bin.install "tachi").
cp "$BIN_PATH" "$STAGE/tachi"
chmod +x "$STAGE/tachi"

# Prefer BSD/GNU portable tar from stage parent.
(
  cd "$OUT_DIR"
  rm -f "${ASSET_STEM}.tar.gz"
  tar -czf "${ASSET_STEM}.tar.gz" "$ASSET_STEM"
)

SHA="$(shasum -a 256 "$TARBALL" | awk '{print $1}')"
echo ">> wrote $TARBALL"
echo "   sha256=$SHA"

# Append/update a SHASUMS line for this asset in OUT_DIR when present.
SUMS="$OUT_DIR/tachi-v${VERSION}-SHASUMS256.txt"
if [[ -f "$SUMS" ]]; then
  # Drop any previous line for this asset name, then append.
  grep -v " ${ASSET_STEM}.tar.gz\$" "$SUMS" >"${SUMS}.tmp" || true
  mv "${SUMS}.tmp" "$SUMS"
fi
echo "${SHA}  ${ASSET_STEM}.tar.gz" >>"$SUMS"
echo ">> sums: $SUMS"

# Machine-readable outputs for CI.
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  {
    echo "tarball=$TARBALL"
    echo "asset_name=${ASSET_STEM}.tar.gz"
    echo "sha256=$SHA"
    echo "triple=$TRIPLE"
    echo "version=$VERSION"
  } >>"$GITHUB_OUTPUT"
fi
