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
# The staging directory is scaffolding: the tarball is the deliverable.
trap 'rm -rf -- "$STAGE"' EXIT

echo "== package ${ASSET_STEM} =="

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  echo ">> cargo build --release -p tachi-server --bin tachi-server (triple=$TRIPLE)"
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    echo "   CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
  fi
  # Host triple: no --target (avoids needing an explicit rustup target install
  # and keeps the binary path at target/release/tachi-server).
  HOST="$(host_triple)"
  if [[ "$TRIPLE" == "$HOST" ]]; then
    cargo build --release --locked -p tachi-server --bin tachi-server
    BIN_PATH="${CARGO_TARGET_DIR:-$ROOT/target}/release/tachi-server"
  else
    cargo build --release --locked -p tachi-server --bin tachi-server --target "$TRIPLE"
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

# Whether the packaged binary runs here: the host's own triple, or an x86_64
# Apple binary on arm64 macOS through Rosetta.
runs_here() {
  [[ "$TRIPLE" == "$HOST" ]] || [[ "$HOST" == "aarch64-apple-darwin" && "$TRIPLE" == "x86_64-apple-darwin" ]]
}

echo ">> binary: $BIN_PATH"
if runs_here; then
  if ! "$BIN_PATH" --version; then
    echo "error: smoke test failed — '$BIN_PATH --version' did not run cleanly; refusing to package a broken binary" >&2
    exit 1
  fi
else
  echo ">> smoke test skipped: $TRIPLE does not run on $HOST"
fi

rm -rf "$STAGE"
mkdir -p "$STAGE"
# Public install name is `tachi` (Homebrew bin.install "tachi").
cp "$BIN_PATH" "$STAGE/tachi"
chmod +x "$STAGE/tachi"

# Ad-hoc re-sign (#937): Apple's linker ad-hoc-signs at build time
# ("Signature=linker-signed"), which launchd/AMFI rejects when the binary is
# spawned as a daemon. Force a real ad-hoc signature on the staged, final-name
# binary so what we sign is what ships. Decided by the target, not by the host:
# a macOS host packaging another platform's triple has nothing to sign.
if [[ "$TRIPLE" == *-apple-* && "$(uname -s)" == "Darwin" ]]; then
  echo ">> codesign --force -s - $STAGE/tachi"
  codesign --force -s - "$STAGE/tachi"
fi

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
  # Drop any previous line matching this exact asset filename, then append.
  # Checked field-wise in a private temporary file so a failed rewrite leaves
  # the existing sums untouched and adjacent extensions (e.g. `.tar.gz.sig`)
  # are preserved.
  sums_tmp="$(mktemp "$OUT_DIR/.tachi-SHASUMS256.XXXXXX")"
  if ! awk -v target="${ASSET_STEM}.tar.gz" '$2 != target' "$SUMS" >"$sums_tmp"; then
    rm -f "$sums_tmp"
    echo "error: could not rewrite $SUMS" >&2
    exit 1
  fi
  mv "$sums_tmp" "$SUMS"
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
