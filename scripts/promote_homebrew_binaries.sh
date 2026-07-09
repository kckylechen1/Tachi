#!/usr/bin/env bash
# Promote private Tachi release binaries onto the public homebrew-tachi tap
# and rewrite Formula/tachi.rb for unauthenticated binary install (#728/#874).
#
# Usage:
#   scripts/promote_homebrew_binaries.sh 1.7.0
#   scripts/promote_homebrew_binaries.sh 1.7.0 --dry-run
#
# Requires: gh (auth'd for private tachi + push to homebrew-tachi), python3, shasum.
set -euo pipefail

VERSION="${1:-}"
shift || true
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  echo "usage: $0 <version> [--dry-run]" >&2
  exit 2
fi
VERSION="${VERSION#v}"

SOURCE_REPO="${SOURCE_REPO:-kckylechen1/tachi}"
TAP_REPO="${TAP_REPO:-kckylechen1/homebrew-tachi}"
ASSET_TAG="tachi-${VERSION}"
PRIVATE_TAG="v${VERSION}"
TRIPLE_DEFAULT="${TRIPLE:-aarch64-apple-darwin}"
ASSET_NAME="tachi-v${VERSION}-${TRIPLE_DEFAULT}.tar.gz"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/tachi-promote.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

echo "== promote ${ASSET_NAME} → ${TAP_REPO}@${ASSET_TAG} =="

# Optional split tokens: private source read vs public tap write.
# Default: single authenticated `gh` session (local operator).
SOURCE_GH_TOKEN="${SOURCE_GH_TOKEN:-${GH_TOKEN:-}}"
TAP_GH_TOKEN="${TAP_GH_TOKEN:-${GH_TOKEN:-}}"

gh_source() {
  if [[ -n "${SOURCE_GH_TOKEN}" ]]; then
    GH_TOKEN="${SOURCE_GH_TOKEN}" gh "$@"
  else
    gh "$@"
  fi
}

gh_tap() {
  if [[ -n "${TAP_GH_TOKEN}" ]]; then
    GH_TOKEN="${TAP_GH_TOKEN}" gh "$@"
  else
    gh "$@"
  fi
}

echo ">> download private release asset"
gh_source release download "$PRIVATE_TAG" \
  --repo "$SOURCE_REPO" \
  -p "$ASSET_NAME" \
  -D "$WORK"

# #911 follow-up (was #907): verify the downloaded tarball against the
# CI-produced SHASUMS256 manifest before blessing it. Without this, a
# private-release asset swapped out-of-band (compromised token, race,
# manual re-upload) would get promoted to the public tap unnoticed.
SHASUMS_NAME="tachi-v${VERSION}-SHASUMS256.txt"
echo ">> download + verify CI checksum manifest ($SHASUMS_NAME)"
gh_source release download "$PRIVATE_TAG" \
  --repo "$SOURCE_REPO" \
  -p "$SHASUMS_NAME" \
  -D "$WORK"

SHA="$(shasum -a 256 "$WORK/$ASSET_NAME" | awk '{print $1}')"
echo "   sha256=$SHA"

EXPECTED_SHA="$(awk -v f="$ASSET_NAME" '$2 == f { print $1 }' "$WORK/$SHASUMS_NAME")"
if [[ -z "$EXPECTED_SHA" ]]; then
  echo "!! no SHASUMS256 entry for $ASSET_NAME in $SHASUMS_NAME — refusing to promote" >&2
  exit 1
fi
if [[ "$SHA" != "$EXPECTED_SHA" ]]; then
  echo "!! sha256 mismatch: downloaded=$SHA CI-manifest=$EXPECTED_SHA — refusing to promote a possibly-replaced asset" >&2
  exit 1
fi
echo "   sha256 verified against CI SHASUMS256 manifest"

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo ">> dry-run: skip upload / formula push"
  echo "   would ensure release $ASSET_TAG on $TAP_REPO"
  echo "   would upload $ASSET_NAME"
  exit 0
fi

echo ">> ensure public tap release $ASSET_TAG"
if ! gh_tap release view "$ASSET_TAG" --repo "$TAP_REPO" >/dev/null 2>&1; then
  gh_tap release create "$ASSET_TAG" \
    --repo "$TAP_REPO" \
    --title "$ASSET_TAG" \
    --notes "Public binary distribution for Tachi v${VERSION} (arm64 macOS). Source remains private; this release is install-only (#874)."
fi

echo ">> upload binary asset (clobber)"
gh_tap release upload "$ASSET_TAG" "$WORK/$ASSET_NAME" --repo "$TAP_REPO" --clobber

echo ">> checkout tap and rewrite formula"
TAP_DIR="$WORK/tap"
if [[ -n "${TAP_GH_TOKEN}" ]]; then
  GH_TOKEN="${TAP_GH_TOKEN}" gh repo clone "$TAP_REPO" "$TAP_DIR" -- --depth 1
else
  gh repo clone "$TAP_REPO" "$TAP_DIR" -- --depth 1
fi
python3 "$ROOT/scripts/update_homebrew_formula.py" \
  "$TAP_DIR/Formula/tachi.rb" \
  --version "$VERSION" \
  --mode binary \
  --asset-repo "$TAP_REPO" \
  --asset-tag "$ASSET_TAG" \
  --sha256="${TRIPLE_DEFAULT}=${SHA}"

# README: public install is available again
if [[ -f "$TAP_DIR/README.md" ]]; then
  cat > "$TAP_DIR/README.md" <<EOF
# Homebrew Tap for Tachi

Public **binary** distribution surface for Tachi (#874).

This repository is intentionally not the source of truth for Tachi product
requirements, capability metadata, agent instructions, skills, plugins, MCP
bundles, or source code. The authoritative source repository, issue tracker, and
review workflow are maintainer-controlled and private.

## Install

\`\`\`bash
brew tap kckylechen1/tachi
brew install tachi
\`\`\`

Current public binaries: **macOS arm64** (Apple Silicon). Intel macOS is not
published yet.

After upgrade, verify the **running** binary / daemon identity:

\`\`\`bash
tachi --version
tachi status   # runtime.build.git_sha / runtime.binary
\`\`\`

## What this tap may contain

- Formula files under \`Formula/\`
- release artifacts (prebuilt \`tachi\` binaries) and checksums
- bottle metadata when present
- install notes

It must not contain source code, product planning, capability authority, agent
runtime state, or issue-driven support workflow.

## Promotion path

1. Cut and verify a private Tachi source release (\`kckylechen1/tachi\` tag \`vX.Y.Z\`).
2. Attach reviewed platform binaries (\`tachi-vX.Y.Z-<triple>.tar.gz\`).
3. Run \`scripts/promote_homebrew_binaries.sh X.Y.Z\` (or the CI workflow) to
   copy assets here and rewrite the formula.
4. End users \`brew upgrade tachi\` — no Rust toolchain required.
EOF
fi

pushd "$TAP_DIR" >/dev/null
git config user.name "tachi-promote"
git config user.email "promote@users.noreply.github.com"
git add Formula/tachi.rb README.md
if git diff --cached --quiet; then
  echo ">> formula already up to date"
else
  git commit -m "bump tachi to v${VERSION} (public binary, arm64)"
  if [[ -n "${TAP_GH_TOKEN}" ]]; then
    # #911 follow-up (was #907): push via GIT_ASKPASS instead of embedding
    # the tap token in the remote URL. `git remote set-url` (the prior
    # approach) writes the token into .git/config in plaintext, where it
    # can be leaked by `git remote -v`, error messages, or a later `rm -rf`
    # miss. GIT_ASKPASS supplies the credential only for this one push
    # invocation and it never touches git config or the remote URL.
    ASKPASS_SCRIPT="$WORK/git-askpass.sh"
    cat > "$ASKPASS_SCRIPT" <<'ASKPASS_EOF'
#!/usr/bin/env bash
# Invoked by git for the HTTPS password prompt only; username is already
# embedded as x-access-token in the push URL below.
printf '%s' "$TAP_GH_TOKEN"
ASKPASS_EOF
    chmod 700 "$ASKPASS_SCRIPT"
    GIT_ASKPASS="$ASKPASS_SCRIPT" GIT_TERMINAL_PROMPT=0 TAP_GH_TOKEN="$TAP_GH_TOKEN" \
      git -c credential.helper= push "https://x-access-token@github.com/${TAP_REPO}.git" HEAD:main
  else
    git push origin HEAD:main
  fi
  echo ">> pushed formula to $TAP_REPO"
fi
popd >/dev/null

echo "== done =="
echo "   brew tap kckylechen1/tachi && brew reinstall tachi"
echo "   expected sha256 ${SHA}"
